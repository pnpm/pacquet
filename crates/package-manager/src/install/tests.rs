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
        dependency_groups: [DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional],
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
    assert_eq!(summary_prefix, &manifest.path().parent().unwrap().to_string_lossy().into_owned(),);

    drop(dir);
}
