use std::{path::Path, sync::atomic::AtomicU8};

use crate::{
    InstallFrozenLockfile, InstallFrozenLockfileError, InstallWithoutLockfile,
    InstallWithoutLockfileError, ResolvedPackages,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::Lockfile;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_reporter::{ContextLog, LogEvent, LogLevel, Reporter, Stage, StageLog, SummaryLog};
use pacquet_tarball::MemCache;

/// This subroutine does everything `pacquet install` is supposed to do.
#[must_use]
pub struct Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub lockfile: Option<&'a Lockfile>,
    pub dependency_groups: DependencyGroupList,
    pub frozen_lockfile: bool,
}

/// Error type of [`Install`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallError {
    #[display(
        "Headless installation requires a pnpm-lock.yaml file, but none was found. Run `pacquet install` without --frozen-lockfile to create one."
    )]
    #[diagnostic(code(pacquet_package_manager::no_lockfile))]
    NoLockfile,

    #[display(
        "Installing with a writable lockfile is not yet supported. Disable lockfile in .npmrc (lockfile=false) or pass --frozen-lockfile with an existing pnpm-lock.yaml."
    )]
    #[diagnostic(code(pacquet_package_manager::unsupported_lockfile_mode))]
    UnsupportedLockfileMode,

    #[diagnostic(transparent)]
    WithoutLockfile(#[error(source)] InstallWithoutLockfileError),

    #[diagnostic(transparent)]
    FrozenLockfile(#[error(source)] InstallFrozenLockfileError),
}

impl<'a, DependencyGroupList> Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run<R: Reporter>(self) -> Result<(), InstallError> {
        let Install {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config,
            manifest,
            lockfile,
            dependency_groups,
            frozen_lockfile,
        } = self;

        // Project root for the [bunyan]-envelope `prefix`. Upstream pnpm
        // emits this as `lockfileDir`, the directory containing
        // `pnpm-lock.yaml`. With workspace support that equals the
        // workspace root. Pacquet has no workspace support yet, so the
        // manifest's parent directory is the correct value today.
        // pnpm/pacquet#357 tracks resolving this via a
        // `findWorkspaceDir`-equivalent once workspaces land.
        //
        // [bunyan]: https://github.com/trentm/node-bunyan
        let prefix = manifest
            .path()
            .parent()
            .map(Path::to_str)
            .map(Option::<&str>::unwrap)
            .unwrap()
            .to_owned();

        // `pnpm:context` carries the directories pnpm's reporter prints
        // in the install header. `currentLockfileExists` reflects
        // `node_modules/.pnpm/lock.yaml` upstream; pacquet doesn't yet
        // read or write that file, so it's always `false` today.
        // TODO: flip when the current-lockfile path lands.
        // Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/context/src/index.ts#L196>.
        R::emit(&LogEvent::Context(ContextLog {
            level: LogLevel::Debug,
            current_lockfile_exists: false,
            store_dir: config.store_dir.display().to_string(),
            virtual_store_dir: config.virtual_store_dir.to_string_lossy().into_owned(),
        }));

        R::emit(&LogEvent::Stage(StageLog {
            level: LogLevel::Debug,
            prefix: prefix.clone(),
            stage: Stage::ImportingStarted,
        }));

        // Install-scoped dedupe state for `pnpm:package-import-method`.
        // Threaded down to `link_file::log_method_once` so each install
        // emits the channel afresh — mirroring upstream pnpm's per-
        // importer closure capture rather than a process-static.
        let logged_methods = AtomicU8::new(0);

        tracing::info!(target: "pacquet::install", "Start all");

        // Dispatch priority, matching pnpm's CLI semantics:
        //
        // 1. `--frozen-lockfile` is the strongest signal. If the user
        //    passed the flag, use the frozen-lockfile path regardless of
        //    `config.lockfile`. The prior `match` treated
        //    `config.lockfile=false` as "skip the lockfile entirely" and
        //    silently dropped the CLI flag — so pacquet's new-config
        //    default (lockfile unset → `false`) turned every
        //    `--frozen-lockfile` install into a registry-resolving
        //    no-lockfile install, which is also what the integrated
        //    benchmark has been measuring.
        //
        // 2. Otherwise follow `config.lockfile`. `true` means we'd
        //    normally generate / update a lockfile, which pacquet
        //    doesn't support yet → `UnsupportedLockfileMode`. `false`
        //    means "lockfile disabled, resolve from registry".
        if frozen_lockfile {
            let Some(lockfile) = lockfile else {
                return Err(InstallError::NoLockfile);
            };
            let Lockfile { lockfile_version, importers, packages, snapshots, .. } = lockfile;
            assert_eq!(lockfile_version.major, 9); // compatibility check already happens at serde, but this still helps preventing programmer mistakes.

            InstallFrozenLockfile {
                http_client,
                config,
                importers,
                packages: packages.as_ref(),
                snapshots: snapshots.as_ref(),
                dependency_groups,
                logged_methods: &logged_methods,
            }
            .run::<R>()
            .await
            .map_err(InstallError::FrozenLockfile)?;
        } else if config.lockfile {
            return Err(InstallError::UnsupportedLockfileMode);
        } else {
            InstallWithoutLockfile {
                tarball_mem_cache,
                resolved_packages,
                http_client,
                config,
                manifest,
                dependency_groups,
                logged_methods: &logged_methods,
            }
            .run::<R>()
            .await
            .map_err(InstallError::WithoutLockfile)?;
        }

        tracing::info!(target: "pacquet::install", "Complete all");

        R::emit(&LogEvent::Stage(StageLog {
            level: LogLevel::Debug,
            prefix: prefix.clone(),
            stage: Stage::ImportingDone,
        }));

        // `pnpm:summary` closes the install and lets the reporter render
        // the accumulated `pnpm:root` events as a "+N -M" block. Must
        // come after `importing_done`, matching pnpm's ordering at
        // <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1663>.
        R::emit(&LogEvent::Summary(SummaryLog { level: LogLevel::Debug, prefix }));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Install, InstallError};
    use pacquet_lockfile::Lockfile;
    use pacquet_npmrc::Npmrc;
    use pacquet_package_manifest::{DependencyGroup, PackageManifest};
    use pacquet_registry_mock::AutoMockInstance;
    use pacquet_reporter::{
        ContextLog, LogEvent, Reporter, SilentReporter, Stage, StageLog, SummaryLog,
    };
    use pacquet_testing_utils::fs::{get_all_folders, is_symlink_or_junction};
    use std::sync::Mutex;
    use tempfile::tempdir;
    use text_block_macros::text_block;

    #[tokio::test]
    async fn should_install_dependencies() {
        let mock_instance = AutoMockInstance::load_or_init();

        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules"); // TODO: we shouldn't have to define this
        let virtual_store_dir = modules_dir.join(".pacquet"); // TODO: we shouldn't have to define this

        let manifest_path = dir.path().join("package.json");
        let mut manifest = PackageManifest::create_if_needed(manifest_path.clone()).unwrap();

        manifest
            .add_dependency("@pnpm.e2e/hello-world-js-bin", "1.0.0", DependencyGroup::Prod)
            .unwrap();
        manifest.add_dependency("@pnpm/xyz", "1.0.0", DependencyGroup::Dev).unwrap();

        manifest.save().unwrap();

        let mut config = Npmrc::new();
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir.to_path_buf();
        config.registry = mock_instance.url();
        let config = config.leak();

        Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: None,
            dependency_groups: [
                DependencyGroup::Prod,
                DependencyGroup::Dev,
                DependencyGroup::Optional,
            ],
            frozen_lockfile: false,
            resolved_packages: &Default::default(),
        }
        .run::<SilentReporter>()
        .await
        .expect("install should succeed");

        // Make sure the package is installed
        let path = project_root.join("node_modules/@pnpm.e2e/hello-world-js-bin");
        assert!(is_symlink_or_junction(&path).unwrap());
        let path = project_root.join("node_modules/.pacquet/@pnpm.e2e+hello-world-js-bin@1.0.0");
        assert!(path.exists());
        // Make sure we install dev-dependencies as well
        let path = project_root.join("node_modules/@pnpm/xyz");
        assert!(is_symlink_or_junction(&path).unwrap());
        let path = project_root.join("node_modules/.pacquet/@pnpm+xyz@1.0.0");
        assert!(path.is_dir());

        insta::assert_debug_snapshot!(get_all_folders(&project_root));

        drop((dir, mock_instance)); // cleanup
    }

    #[tokio::test]
    async fn should_error_when_frozen_lockfile_is_requested_but_none_exists() {
        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules");
        let virtual_store_dir = modules_dir.join(".pacquet");

        let manifest_path = dir.path().join("package.json");
        let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

        let mut config = Npmrc::new();
        config.lockfile = true;
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir;
        let config = config.leak();

        let result = Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: None,
            dependency_groups: [DependencyGroup::Prod],
            frozen_lockfile: true,
            resolved_packages: &Default::default(),
        }
        .run::<SilentReporter>()
        .await;

        assert!(matches!(result, Err(InstallError::NoLockfile)));
        drop(dir);
    }

    #[tokio::test]
    async fn should_error_when_writable_lockfile_mode_is_used() {
        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules");
        let virtual_store_dir = modules_dir.join(".pacquet");

        let manifest_path = dir.path().join("package.json");
        let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

        let mut config = Npmrc::new();
        config.lockfile = true;
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir;
        let config = config.leak();

        let result = Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: None,
            dependency_groups: [DependencyGroup::Prod],
            frozen_lockfile: false,
            resolved_packages: &Default::default(),
        }
        .run::<SilentReporter>()
        .await;

        assert!(matches!(result, Err(InstallError::UnsupportedLockfileMode)));
        drop(dir);
    }

    /// `--frozen-lockfile` passed on the CLI must take precedence over
    /// `config.lockfile=false`. Before this fix the dispatch matched on
    /// `(config.lockfile, frozen_lockfile, lockfile)` in an order that
    /// treated `config.lockfile=false` as "skip lockfile entirely",
    /// silently dropping the CLI flag and resolving from the registry
    /// instead — the very regression the integrated benchmark was
    /// measuring. Pin the new priority: frozen flag + lockfile present
    /// → `InstallFrozenLockfile`, regardless of `config.lockfile`.
    ///
    /// We don't need the full install to succeed here — any error that
    /// *isn't* `NoLockfile` / `UnsupportedLockfileMode` proves the
    /// dispatch picked the frozen path. Passing a malformed lockfile
    /// integrity surfaces as `FrozenLockfile(...)`.
    #[tokio::test]
    async fn frozen_lockfile_flag_overrides_config_lockfile_false() {
        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules");
        let virtual_store_dir = modules_dir.join(".pacquet");

        let manifest_path = dir.path().join("package.json");
        let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

        let mut config = Npmrc::new();
        // Explicitly disabled — this is the pacquet default today. The
        // CLI flag must still take over.
        config.lockfile = false;
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir;
        let config = config.leak();

        // Minimal v9 lockfile with no snapshots — the frozen path will
        // run through `CreateVirtualStore` with an empty snapshot set,
        // which is a successful no-op. That's enough to prove we took
        // the frozen branch.
        let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
            "lockfileVersion: '9.0'"
            "importers:"
            "  .:"
            "    dependencies: {}"
            "packages: {}"
            "snapshots: {}"
        })
        .expect("parse minimal v9 lockfile");

        Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: Some(&lockfile),
            dependency_groups: [DependencyGroup::Prod],
            frozen_lockfile: true,
            resolved_packages: &Default::default(),
        }
        .run::<SilentReporter>()
        .await
        .expect("--frozen-lockfile + empty lockfile should succeed via InstallFrozenLockfile");

        drop(dir);
    }

    /// Issue #312: an npm-alias dependency
    /// (`"<key>": "npm:<real>@<range>"`) used to panic during install
    /// because the whole `npm:...` spec was fed to
    /// `node_semver::Range::parse`. Assert that:
    ///
    /// * the install completes,
    /// * the virtual-store directory uses the *real* package name, and
    /// * the symlink under `node_modules/` uses the alias key.
    ///
    /// Mirrors pnpm's `parseBareSpecifier`. Reference:
    /// <https://github.com/pnpm/pnpm/blob/1819226b51/resolving/npm-resolver/src/parseBareSpecifier.ts>
    #[tokio::test]
    async fn npm_alias_dependency_installs_under_alias_key() {
        let mock_instance = AutoMockInstance::load_or_init();

        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules");
        let virtual_store_dir = modules_dir.join(".pacquet");

        let manifest_path = dir.path().join("package.json");
        let mut manifest = PackageManifest::create_if_needed(manifest_path.clone()).unwrap();

        manifest
            .add_dependency(
                "hello-world-alias",
                "npm:@pnpm.e2e/hello-world-js-bin@1.0.0",
                DependencyGroup::Prod,
            )
            .unwrap();
        manifest.save().unwrap();

        let mut config = Npmrc::new();
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir.to_path_buf();
        config.registry = mock_instance.url();
        let config = config.leak();

        Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: None,
            dependency_groups: [DependencyGroup::Prod],
            frozen_lockfile: false,
            resolved_packages: &Default::default(),
        }
        .run::<SilentReporter>()
        .await
        .expect("npm-alias install should succeed");

        // Symlink lives under the alias key, *not* the real package name.
        let alias_link = project_root.join("node_modules/hello-world-alias");
        assert!(
            is_symlink_or_junction(&alias_link).unwrap(),
            "expected alias symlink at {alias_link:?}",
        );
        assert!(
            !project_root.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists(),
            "the real package name must not be exposed alongside an unrelated alias",
        );

        // Virtual-store directory uses the real package name.
        let virtual_store_path =
            project_root.join("node_modules/.pacquet/@pnpm.e2e+hello-world-js-bin@1.0.0");
        assert!(virtual_store_path.is_dir(), "expected real-name virtual store dir");
        assert!(virtual_store_path.join("node_modules/@pnpm.e2e/hello-world-js-bin").is_dir());

        drop((dir, mock_instance));
    }

    /// Issue #312, unversioned variant: `"foo": "npm:bar"` (no `@<range>`)
    /// must default to `latest` without panicking. `resolve_registry_dependency`
    /// turns `"npm:bar"` into `("bar", "latest")`; the previous code then
    /// fed `"latest"` to `package.pinned_version()` which panics because
    /// `node_semver::Range` cannot parse the string. The fix is to route
    /// `"latest"` (and any `PackageTag`-parseable value) through
    /// `PackageVersion::fetch_from_registry` directly.
    ///
    /// We use the same scoped test package as the pinned-version test above
    /// but omit the `@1.0.0` suffix to trigger the default-to-`latest` path.
    #[tokio::test]
    async fn unversioned_npm_alias_defaults_to_latest() {
        let mock_instance = AutoMockInstance::load_or_init();

        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules");
        let virtual_store_dir = modules_dir.join(".pacquet");

        let manifest_path = dir.path().join("package.json");
        let mut manifest = PackageManifest::create_if_needed(manifest_path.clone()).unwrap();

        // No `@<version>` — should resolve to the `latest` tag.
        manifest
            .add_dependency(
                "hello-world-alias",
                "npm:@pnpm.e2e/hello-world-js-bin",
                DependencyGroup::Prod,
            )
            .unwrap();
        manifest.save().unwrap();

        let mut config = Npmrc::new();
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir.to_path_buf();
        config.registry = mock_instance.url();
        let config = config.leak();

        Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: None,
            dependency_groups: [DependencyGroup::Prod],
            frozen_lockfile: false,
            resolved_packages: &Default::default(),
        }
        .run::<SilentReporter>()
        .await
        .expect("unversioned npm-alias install should succeed (defaults to latest)");

        // Symlink lives under the alias key, not the real package name.
        let alias_link = project_root.join("node_modules/hello-world-alias");
        assert!(
            is_symlink_or_junction(&alias_link).unwrap(),
            "expected alias symlink at {alias_link:?}",
        );
        assert!(
            !project_root.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists(),
            "the real package name must not be exposed alongside the alias",
        );

        // Virtual-store directory uses the real package name (version resolved
        // at runtime from `latest` — just assert the real name prefix exists).
        let virtual_store_dir_path = project_root.join("node_modules/.pacquet");
        let has_real_name_dir = std::fs::read_dir(&virtual_store_dir_path)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("@pnpm.e2e+hello-world-js-bin@"));
        assert!(has_real_name_dir, "expected real-name virtual store directory");

        drop((dir, mock_instance));
    }

    /// Symmetric negative: `--frozen-lockfile` with no lockfile
    /// loadable must surface `NoLockfile`, even when `config.lockfile`
    /// is `false` (which used to fall through to the no-lockfile path
    /// and silently succeed).
    #[tokio::test]
    async fn frozen_lockfile_flag_with_no_lockfile_errors() {
        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules");
        let virtual_store_dir = modules_dir.join(".pacquet");

        let manifest_path = dir.path().join("package.json");
        let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

        let mut config = Npmrc::new();
        config.lockfile = false;
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir;
        let config = config.leak();

        let result = Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: None,
            dependency_groups: [DependencyGroup::Prod],
            frozen_lockfile: true,
            resolved_packages: &Default::default(),
        }
        .run::<SilentReporter>()
        .await;

        assert!(matches!(result, Err(InstallError::NoLockfile)));
        drop(dir);
    }

    /// [`Install::run`] emits `pnpm:context`, then `pnpm:stage`
    /// `importing_started`, then on the success path `importing_done`
    /// followed by `pnpm:summary`. On an early-error path such as
    /// [`InstallError::NoLockfile`] only the leading events fire. This
    /// matches pnpm: context is emitted once alongside the install
    /// header, the stage pairing drives the JS reporter's progress
    /// UI, and summary closes the run so the reporter can render its
    /// "+N -M" block.
    ///
    /// `pnpm:package-import-method` is emitted lazily by `link_file`
    /// the first time each method actually resolves (after `auto`'s
    /// fallback chain finishes), so an empty-lockfile install like this
    /// one has no link_file calls and no such event in the captured
    /// sequence. See `link_file::tests` for that channel's coverage.
    ///
    /// `pnpm:context` carries `currentLockfileExists`, `storeDir`,
    /// `virtualStoreDir`. `currentLockfileExists` is hard-coded
    /// `false` today (pacquet doesn't read or write
    /// `node_modules/.pnpm/lock.yaml`), matching the TODO in
    /// [`Install::run`].
    #[tokio::test]
    async fn install_emits_pnpm_event_sequence() {
        static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());

        struct RecordingReporter;
        impl Reporter for RecordingReporter {
            fn emit(event: &LogEvent) {
                EVENTS.lock().unwrap().push(event.clone());
            }
        }

        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules");
        let virtual_store_dir = modules_dir.join(".pacquet");

        let manifest_path = dir.path().join("package.json");
        let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

        let mut config = Npmrc::new();
        config.lockfile = false;
        config.store_dir = store_dir.clone().into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir.clone();
        let config = config.leak();

        // Empty v9 lockfile: `--frozen-lockfile` walks an empty snapshot
        // set successfully, which is the cheapest "real" install path.
        let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
            "lockfileVersion: '9.0'"
            "importers:"
            "  .:"
            "    dependencies: {}"
            "packages: {}"
            "snapshots: {}"
        })
        .expect("parse minimal v9 lockfile");

        Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: Some(&lockfile),
            dependency_groups: [DependencyGroup::Prod],
            frozen_lockfile: true,
            resolved_packages: &Default::default(),
        }
        .run::<RecordingReporter>()
        .await
        .expect("empty-lockfile frozen install should succeed");

        let captured = EVENTS.lock().unwrap();

        // Event ordering matches pnpm: context, then the stage
        // bracketing pair, then summary closing the run.
        assert!(
            matches!(
                captured.as_slice(),
                [
                    LogEvent::Context(_),
                    LogEvent::Stage(StageLog { stage: Stage::ImportingStarted, .. }),
                    LogEvent::Stage(StageLog { stage: Stage::ImportingDone, .. }),
                    LogEvent::Summary(_),
                ]
            ),
            "unexpected event sequence: {captured:?}",
        );

        // Spot-check the context payload: pacquet's directories must
        // round-trip through the wire shape, and `currentLockfileExists`
        // is the hard-coded `false` documented in `Install::run`.
        let LogEvent::Context(ContextLog {
            current_lockfile_exists,
            store_dir: emitted_store_dir,
            virtual_store_dir: emitted_virtual_store_dir,
            ..
        }) = &captured[0]
        else {
            unreachable!("first event is context, asserted above");
        };
        assert!(!current_lockfile_exists);
        assert_eq!(emitted_store_dir, &store_dir.display().to_string());
        assert_eq!(emitted_virtual_store_dir, &virtual_store_dir.to_string_lossy().into_owned());

        // Summary's `prefix` must equal the manifest-parent value
        // `Install::run` derives, since pnpm's reporter keys its
        // accumulated root-events by prefix to render the diff.
        let LogEvent::Summary(SummaryLog { prefix: summary_prefix, .. }) = &captured[3] else {
            unreachable!("fourth event is summary, asserted above");
        };
        assert_eq!(
            summary_prefix,
            &manifest.path().parent().unwrap().to_string_lossy().into_owned(),
        );

        drop(dir);
    }
}
