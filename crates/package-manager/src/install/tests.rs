use super::{Install, InstallError};
use pacquet_config::Config;
use pacquet_lockfile::Lockfile;
use pacquet_modules_yaml::{
    DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH, LayoutVersion, Modules, NodeLinker, RealApi,
    read_modules_manifest,
};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry_mock::AutoMockInstance;
use pacquet_reporter::{
    BrokenModulesLog, ContextLog, IgnoredScriptsLog, LogEvent, PackageManifestLog,
    PackageManifestMessage, ProgressLog, ProgressMessage, Reporter, SilentReporter, Stage,
    StageLog, StatsLog, StatsMessage, SummaryLog,
};
use pacquet_testing_utils::fs::{get_all_folders, is_symlink_or_junction};
use pipe_trait::Pipe;
use std::{path::PathBuf, sync::Mutex};
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

    let mut config = Config::new();
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
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<SilentReporter>()
    .await
    .expect("install should succeed");

    // Make sure the package is installed
    let path = project_root.join("node_modules/@pnpm.e2e/hello-world-js-bin");
    eprintln!("path={path:?} symlink_or_junction={:?}", is_symlink_or_junction(&path));
    assert!(is_symlink_or_junction(&path).unwrap());
    let path = project_root.join("node_modules/.pacquet/@pnpm.e2e+hello-world-js-bin@1.0.0");
    eprintln!("path={path:?} exists={}", path.exists());
    assert!(path.exists());
    // Make sure we install dev-dependencies as well
    let path = project_root.join("node_modules/@pnpm/xyz");
    eprintln!("path={path:?} symlink_or_junction={:?}", is_symlink_or_junction(&path));
    assert!(is_symlink_or_junction(&path).unwrap());
    let path = project_root.join("node_modules/.pacquet/@pnpm+xyz@1.0.0");
    eprintln!("path={path:?} is_dir={}", path.is_dir());
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

    let mut config = Config::new();
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
        supported_architectures: None,
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

    let mut config = Config::new();
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
        supported_architectures: None,
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

    let mut config = Config::new();
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
        supported_architectures: None,
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

    let mut config = Config::new();
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
        supported_architectures: None,
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

    let mut config = Config::new();
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
        supported_architectures: None,
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

    let mut config = Config::new();
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
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<SilentReporter>()
    .await;

    assert!(matches!(result, Err(InstallError::NoLockfile)));
    drop(dir);
}

/// [`Install::run`] emits `pnpm:package-manifest initial`,
/// `pnpm:context`, then `pnpm:stage` `importing_started`, then on
/// the success path `importing_done` followed by `pnpm:summary`.
/// On an early-error path such as [`InstallError::NoLockfile`]
/// only the leading events fire. This matches pnpm: the manifest
/// snapshot lands first so consumers can diff it against
/// `updated`, context is emitted alongside the install header, the
/// stage pairing drives the JS reporter's progress UI, and summary
/// closes the run so the reporter can render its "+N -M" block.
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
    // Reset in case nextest reuses the process for a retry of this test.
    EVENTS.lock().unwrap().clear();

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

    let mut config = Config::new();
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
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<RecordingReporter>()
    .await
    .expect("empty-lockfile frozen install should succeed");

    let captured = EVENTS.lock().unwrap();

    // Event ordering matches pnpm: manifest snapshot, context,
    // importing_started, the `pnpm:stats` added/removed pair from
    // `CreateVirtualStore::run`, then `importing_done` once extraction
    // and symlink linking are complete (mirrors upstream `link.ts:167`),
    // followed by the `pnpm:ignored-scripts` summary that
    // `BuildModules::run` produces, then summary closing the run. The
    // empty snapshot map still triggers the stats emit (`added: 0`,
    // `removed: 0`), matching pnpm's unconditional emit at link time.
    // The empty lockfile produces no ignored builds, so
    // `ignored-scripts` carries an empty list.
    assert!(
        matches!(
            captured.as_slice(),
            [
                LogEvent::PackageManifest(PackageManifestLog {
                    message: PackageManifestMessage::Initial { .. },
                    ..
                }),
                LogEvent::Context(_),
                LogEvent::Stage(StageLog { stage: Stage::ImportingStarted, .. }),
                LogEvent::Stats(StatsLog { message: StatsMessage::Added { added: 0, .. }, .. }),
                LogEvent::Stats(StatsLog { message: StatsMessage::Removed { removed: 0, .. }, .. }),
                LogEvent::Stage(StageLog { stage: Stage::ImportingDone, .. }),
                LogEvent::IgnoredScripts(_),
                LogEvent::Summary(_),
            ],
        ),
        "unexpected event sequence: {captured:?}",
    );

    // Empty lockfile produces no ignored builds.
    let LogEvent::IgnoredScripts(IgnoredScriptsLog { package_names, .. }) = &captured[6] else {
        unreachable!("ignored-scripts at index 6, asserted above");
    };
    assert!(package_names.is_empty(), "no builds in empty lockfile: {package_names:?}");

    let expected_prefix = manifest.path().parent().unwrap().to_string_lossy().into_owned();

    // Manifest event carries the on-disk JSON unchanged so consumers
    // can diff `initial` vs a later `updated` byte-for-byte.
    let LogEvent::PackageManifest(PackageManifestLog {
        message: PackageManifestMessage::Initial { prefix: manifest_prefix, initial },
        ..
    }) = &captured[0]
    else {
        unreachable!("first event is package-manifest, asserted above");
    };
    assert_eq!(manifest_prefix, &expected_prefix);
    assert_eq!(initial, manifest.value());

    // Spot-check the context payload: pacquet's directories must
    // round-trip through the wire shape, and `currentLockfileExists`
    // is `false` on this first install because no `lock.yaml` exists
    // in the (just-created) virtual store yet — pacquet writes the
    // file at end-of-install, so the next install would see `true`.
    let LogEvent::Context(ContextLog {
        current_lockfile_exists,
        store_dir: emitted_store_dir,
        virtual_store_dir: emitted_virtual_store_dir,
        ..
    }) = &captured[1]
    else {
        unreachable!("second event is context, asserted above");
    };
    assert!(!current_lockfile_exists);
    assert_eq!(emitted_store_dir, &store_dir.display().to_string());
    assert_eq!(emitted_virtual_store_dir, &virtual_store_dir.to_string_lossy().into_owned());

    // Summary's `prefix` must equal the manifest-parent value
    // `Install::run` derives, since pnpm's reporter keys its
    // accumulated root-events by prefix to render the diff.
    let LogEvent::Summary(SummaryLog { prefix: summary_prefix, .. }) = captured.last().unwrap()
    else {
        unreachable!("last event is summary, asserted above");
    };
    assert_eq!(summary_prefix, &expected_prefix);

    drop(dir);
}

/// A successful install must persist `<modules_dir>/.modules.yaml`,
/// matching pnpm's
/// [`writeModulesManifest`](https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1608-L1630)
/// call. Asserts the on-disk fields a follow-up install (or third-
/// party tool) keys off: `layoutVersion`, `nodeLinker`, the
/// `included` set derived from the dispatched dependency groups, the
/// store and virtual-store directories, and the `default` registry.
#[tokio::test]
async fn install_writes_modules_yaml() {
    let dir = tempdir().unwrap();
    let store_dir = dir.path().join("pacquet-store");
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    let manifest_path = dir.path().join("package.json");
    let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

    let mut config = Config::new();
    config.lockfile = false;
    config.store_dir = store_dir.clone().into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    let config = config.leak();

    // Empty v9 lockfile drives the cheapest successful install path,
    // which is enough to prove `.modules.yaml` is written on success.
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
        // Drive a non-default `included`: prod + optional, no dev,
        // so the assertion below pins the mapping of dispatched
        // groups to the on-disk `included` field.
        dependency_groups: [DependencyGroup::Prod, DependencyGroup::Optional],
        frozen_lockfile: true,
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<SilentReporter>()
    .await
    .expect("frozen-lockfile install should succeed");

    let Modules {
        layout_version,
        node_linker,
        included,
        store_dir: emitted_store_dir,
        virtual_store_dir: emitted_virtual_store_dir,
        virtual_store_dir_max_length,
        registries,
        package_manager,
        ..
    } = modules_dir
        .pipe_as_ref(read_modules_manifest::<RealApi>)
        .expect("read .modules.yaml")
        .expect("modules manifest exists");

    assert_eq!(layout_version, Some(LayoutVersion));
    assert_eq!(node_linker, Some(NodeLinker::Isolated));
    assert!(included.dependencies);
    assert!(!included.dev_dependencies);
    assert!(included.optional_dependencies);
    assert_eq!(emitted_store_dir, store_dir.display().to_string());
    // `read_modules_manifest` resolves `virtualStoreDir` against
    // `modules_dir`, so a relative on-disk value round-trips back
    // to the absolute install-time path.
    assert_eq!(emitted_virtual_store_dir, virtual_store_dir.to_string_lossy());
    assert_eq!(virtual_store_dir_max_length, DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH);
    assert_eq!(
        registries.as_ref().and_then(|r| r.get("default")).map(String::as_str),
        Some(config.registry.as_str()),
    );
    assert!(
        package_manager.starts_with("pacquet@"),
        "expected `pacquet@<version>`, got {package_manager:?}",
    );

    drop(dir);
}

/// Ports `'do not fail on an optional dependency that has a non-optional
/// dependency with a failing postinstall script'` at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/installing/deps-installer/test/install/optionalDependencies.ts#L563-L572>.
///
/// Resolves `@pnpm.e2e/has-failing-postinstall-dep@1.0.0` as an
/// optional dependency through the live registry-mock instance. The
/// transitive `@pnpm.e2e/failing-postinstall@1.0.0` has a
/// `postinstall` that exits non-zero. Pacquet's
/// `frozen_lockfile=false` path stops at extraction (script execution
/// lives behind `BuildModules` in the frozen-lockfile branch —
/// `BuildModules` itself is unit-tested against the same fixture in
/// `crate::build_modules::tests::do_not_fail_on_optional_dep_with_failing_postinstall`).
/// This test pins the fetch + extract behavior on the optional edge:
/// both packages must land in the virtual store and the install must
/// NOT abort, matching the upstream expectation that `addDependenciesToPackage`
/// resolves.
#[tokio::test]
async fn install_optional_failing_postinstall_dep_via_registry_mock_succeeds() {
    let mock_instance = AutoMockInstance::load_or_init();

    let dir = tempdir().unwrap();
    let store_dir = dir.path().join("pacquet-store");
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    let manifest_path = dir.path().join("package.json");
    let mut manifest = PackageManifest::create_if_needed(manifest_path.clone()).unwrap();
    manifest
        .add_dependency("@pnpm.e2e/has-failing-postinstall-dep", "1.0.0", DependencyGroup::Optional)
        .unwrap();
    manifest.save().unwrap();

    let mut config = Config::new();
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
        dependency_groups: [DependencyGroup::Prod, DependencyGroup::Optional],
        frozen_lockfile: false,
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<SilentReporter>()
    .await
    .expect("optional dep with failing transitive postinstall must NOT abort the install");

    // Both the wrapper and the transitive must reach the virtual store.
    assert!(
        is_symlink_or_junction(
            &project_root.join("node_modules/@pnpm.e2e/has-failing-postinstall-dep"),
        )
        .unwrap(),
        "wrapper symlink missing",
    );
    assert!(
        project_root
            .join("node_modules/.pacquet/@pnpm.e2e+has-failing-postinstall-dep@1.0.0")
            .is_dir(),
        "wrapper virtual-store dir missing",
    );
    assert!(
        project_root.join("node_modules/.pacquet/@pnpm.e2e+failing-postinstall@1.0.0").is_dir(),
        "transitive `failing-postinstall` must be extracted to the virtual store",
    );

    drop((dir, mock_instance));
}

/// A v9 lockfile fixture pinned to a placeholder package whose
/// integrity is bogus on purpose. Pacquet enforces tarball integrity
/// on the install path, so any test that lets the install reach the
/// fetch site would fail — meaning a successful install with this
/// fixture is *proof* that the per-snapshot skip path (issue #433
/// section B) short-circuited the fetch entirely.
const PARTIAL_INSTALL_LOCKFILE: &str = text_block! {
    "lockfileVersion: '9.0'"
    "importers:"
    "  .:"
    "    dependencies:"
    "      placeholder:"
    "        specifier: 1.0.0"
    "        version: 1.0.0"
    "packages:"
    "  placeholder@1.0.0:"
    "    resolution: {integrity: sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, tarball: 'http://invalid.local/placeholder.tgz'}"
    "snapshots:"
    "  placeholder@1.0.0: {}"
};

/// Pre-populate the virtual-store slot that `PARTIAL_INSTALL_LOCKFILE`
/// describes so the skip path has a directory to point at. Just the
/// `<virtual_store_dir>/placeholder@1.0.0/node_modules/placeholder`
/// dirent is enough — the skip check only stats the directory, it
/// doesn't read CAS contents.
fn seed_placeholder_virtual_store_slot(virtual_store_dir: &std::path::Path) {
    let slot = virtual_store_dir.join("placeholder@1.0.0").join("node_modules").join("placeholder");
    std::fs::create_dir_all(&slot).expect("create placeholder virtual-store slot");
}

/// Section B of pnpm/pacquet#433: a snapshot whose wiring and
/// integrity match the current lockfile *and* whose virtual-store
/// slot exists on disk is dropped from the install graph entirely.
/// We prove this by pointing the lockfile at a bogus tarball URL —
/// any code path that reaches the fetch site would fail, so a
/// successful install demonstrates the skip path took over.
#[tokio::test]
async fn warm_reinstall_skips_snapshot_when_current_lockfile_matches() {
    let dir = tempdir().unwrap();
    let store_dir = dir.path().join("pacquet-store");
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    let manifest_path = dir.path().join("package.json");
    let mut manifest = PackageManifest::create_if_needed(manifest_path).unwrap();
    // Manifest must match `PARTIAL_INSTALL_LOCKFILE` — the freshness
    // check (#447) rejects any drift between the on-disk manifest and
    // the lockfile importer entry.
    manifest.add_dependency("placeholder", "1.0.0", DependencyGroup::Prod).unwrap();
    manifest.save().unwrap();

    let mut config = Config::new();
    // Opt out of the (now-default) global virtual store: the
    // `seed_placeholder_virtual_store_slot` helper writes the legacy
    // `<virtual_store_dir>/<flat-name>` shape, which only matches the
    // skip-probe path when `VirtualStoreLayout` is in legacy mode.
    // The partial-install behaviour under test (skip when the
    // current lockfile matches + slot exists) is independent of the
    // GVS layout; the GVS-on equivalent is exercised by the
    // `frozen_lockfile_under_gvs_*` tests below.
    config.enable_global_virtual_store = false;
    config.store_dir = store_dir.into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    let config = config.leak();

    let lockfile: Lockfile = serde_saphyr::from_str(PARTIAL_INSTALL_LOCKFILE)
        .expect("parse partial-install fixture lockfile");

    // Pre-seed the previous-install state: write the current lockfile
    // identical to the wanted lockfile, and materialize the virtual-
    // store slot the skip check stats against.
    std::fs::create_dir_all(&virtual_store_dir).unwrap();
    lockfile.save_current_to_virtual_store_dir(&virtual_store_dir).expect("seed current lockfile");
    seed_placeholder_virtual_store_slot(&virtual_store_dir);

    Install {
        tarball_mem_cache: &Default::default(),
        http_client: &Default::default(),
        config,
        manifest: &manifest,
        lockfile: Some(&lockfile),
        dependency_groups: [DependencyGroup::Prod],
        frozen_lockfile: true,
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<SilentReporter>()
    .await
    .expect(
        "skip path must short-circuit the fetch for the placeholder snapshot \
         (bogus integrity + URL would otherwise fail the install)",
    );

    // `lock.yaml` survives the install — the end-of-install write
    // persists the wanted lockfile back to disk.
    let written = Lockfile::load_current_from_virtual_store_dir(&virtual_store_dir)
        .expect("read written current lockfile")
        .expect("current lockfile should be written");
    assert_eq!(written.snapshots.as_ref().map(|s| s.len()), Some(1));

    drop(dir);
}

/// When the cached directory is gone but the cache key still matches,
/// pacquet emits `pnpm:_broken_node_modules` (mirroring upstream's
/// debug emit at `lockfileToDepGraph.ts:258`) and falls through to the
/// full install path for that snapshot.
#[tokio::test]
async fn warm_reinstall_emits_broken_modules_when_dir_is_missing() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().unwrap().clear();

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
    let mut manifest = PackageManifest::create_if_needed(manifest_path).unwrap();
    // Manifest must match `PARTIAL_INSTALL_LOCKFILE` — the freshness
    // check (#447) rejects any drift between the on-disk manifest and
    // the lockfile importer entry.
    manifest.add_dependency("placeholder", "1.0.0", DependencyGroup::Prod).unwrap();
    manifest.save().unwrap();

    let mut config = Config::new();
    // Opt out of the GVS layout — see the rationale on
    // [`warm_reinstall_skips_snapshot_when_current_lockfile_matches`].
    // The pre-seeded `<virtual_store_dir>/<flat-name>` slot is the
    // legacy shape the probe matches; the BrokenModules emit fires
    // identically under either layout once the slot is missing.
    config.enable_global_virtual_store = false;
    config.store_dir = store_dir.into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    // Skip fetch retries entirely — the install is expected to fail
    // after emitting `_broken_node_modules`, so any retry budget is
    // pure waste here.
    config.fetch_retries = 0;
    config.fetch_retry_mintimeout = 1;
    config.fetch_retry_maxtimeout = 1;
    let config = config.leak();

    let lockfile: Lockfile = serde_saphyr::from_str(PARTIAL_INSTALL_LOCKFILE)
        .expect("parse partial-install fixture lockfile");

    // Pre-seed the current lockfile but deliberately *not* the
    // virtual-store slot — the cache key matches but the directory is
    // gone (the `rm -rf node_modules/.pnpm/<slot>` scenario).
    std::fs::create_dir_all(&virtual_store_dir).unwrap();
    lockfile.save_current_to_virtual_store_dir(&virtual_store_dir).expect("seed current lockfile");

    // The install will attempt to fetch the placeholder (bogus URL),
    // which fails — what we're testing is that the broken-modules
    // signal fires *before* the fetch happens. So we look for the
    // event in the captured set regardless of the final install
    // result.
    let _ = Install {
        tarball_mem_cache: &Default::default(),
        http_client: &Default::default(),
        config,
        manifest: &manifest,
        lockfile: Some(&lockfile),
        dependency_groups: [DependencyGroup::Prod],
        frozen_lockfile: true,
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<RecordingReporter>()
    .await;

    let captured = EVENTS.lock().unwrap();
    let broken: Vec<&BrokenModulesLog> = captured
        .iter()
        .filter_map(|e| match e {
            LogEvent::BrokenModules(b) => Some(b),
            _ => None,
        })
        .collect();
    assert_eq!(
        broken.len(),
        1,
        "expected exactly one pnpm:_broken_node_modules emit; got: {captured:?}",
    );
    assert!(
        broken[0].missing.contains("placeholder@1.0.0"),
        "broken-modules `missing` path must name the affected slot; got: {missing}",
        missing = broken[0].missing,
    );

    drop(dir);
}

/// Section A + D of pnpm/pacquet#433: a second install observes
/// `pnpm:context.currentLockfileExists: true` once the first install
/// has written `<virtual_store_dir>/lock.yaml`. Drives the read site
/// (`Install::run` → `load_current_from_virtual_store_dir`) on real
/// disk state produced by the matching write site.
#[tokio::test]
async fn context_log_reflects_current_lockfile_after_first_install() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().unwrap().clear();

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
    let mut manifest = PackageManifest::create_if_needed(manifest_path).unwrap();
    // Manifest must match the fixture lockfile below — the freshness
    // check (#447) rejects any drift between the on-disk manifest and
    // the lockfile importer entry.
    manifest.add_dependency("placeholder", "1.0.0", DependencyGroup::Prod).unwrap();
    manifest.save().unwrap();

    let mut config = Config::new();
    config.store_dir = store_dir.into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    let config = config.leak();

    // Non-empty lockfile with no snapshots: the root importer lists
    // one dependency so `Lockfile::is_empty` returns `false` (and
    // the end-of-install write persists the file rather than
    // deleting it), but the empty `snapshots:` map means
    // `CreateVirtualStore::run` has no fetches to attempt. The
    // dangling symlink that `SymlinkDirectDependencies` creates is
    // fine — `link_direct_dep_bins` swallows `NotFound` on the
    // target's `package.json`. This keeps the test off the mock
    // registry while still driving the read-after-write loop.
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      placeholder:"
        "        specifier: 1.0.0"
        "        version: 1.0.0"
        "packages: {}"
        "snapshots: {}"
    })
    .expect("parse minimal v9 lockfile");
    assert!(!lockfile.is_empty(), "fixture must be non-empty so the write path persists it");

    // First install: `lock.yaml` does not exist yet.
    EVENTS.lock().unwrap().clear();
    Install {
        tarball_mem_cache: &Default::default(),
        http_client: &Default::default(),
        config,
        manifest: &manifest,
        lockfile: Some(&lockfile),
        dependency_groups: [DependencyGroup::Prod],
        frozen_lockfile: true,
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<RecordingReporter>()
    .await
    .expect("first install should succeed");

    let first_context = EVENTS
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            LogEvent::Context(c) => Some(c.clone()),
            _ => None,
        })
        .expect("first install emitted a context event");
    assert!(!first_context.current_lockfile_exists);

    // The first install must have persisted the lockfile under the
    // virtual store. If `save_current_to_virtual_store_dir` regressed
    // for non-empty lockfiles, this check fails — and so does the
    // false→true assertion below, which is the whole point of pinning
    // the read-after-write loop.
    let lock_yaml = virtual_store_dir.join(Lockfile::CURRENT_FILE_NAME);
    assert!(
        lock_yaml.is_file(),
        "non-empty wanted lockfile must be persisted under <virtual_store_dir>/lock.yaml; found nothing at {lock_yaml:?}",
    );

    // Second install: identical inputs. The skip filter has nothing
    // to skip (no snapshots), but the read-after-write loop still
    // fires `current_lockfile_exists: true` because the first
    // install's `lock.yaml` is now on disk.
    EVENTS.lock().unwrap().clear();
    Install {
        tarball_mem_cache: &Default::default(),
        http_client: &Default::default(),
        config,
        manifest: &manifest,
        lockfile: Some(&lockfile),
        dependency_groups: [DependencyGroup::Prod],
        frozen_lockfile: true,
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<RecordingReporter>()
    .await
    .expect("second install should succeed");

    let second_context = EVENTS
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            LogEvent::Context(c) => Some(c.clone()),
            _ => None,
        })
        .expect("second install emitted a context event");
    assert!(
        second_context.current_lockfile_exists,
        "context.currentLockfileExists must flip to true once lock.yaml is on disk",
    );

    drop(dir);
}

/// The skip path drops the snapshot from both the warm and cold
/// batches, so a warm reinstall must report `added: 0` and emit
/// zero `pnpm:progress imported` events. Pre-seeds `lock.yaml` and
/// the virtual-store slot manually here — the
/// [`context_log_reflects_current_lockfile_after_first_install`]
/// test covers the read-after-write loop on its own, so this one
/// can focus on the skip's reporter-visible effect.
#[tokio::test]
async fn warm_reinstall_reports_added_zero_and_emits_no_imported_events() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().unwrap().clear();

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
    let mut manifest = PackageManifest::create_if_needed(manifest_path).unwrap();
    // Manifest must match `PARTIAL_INSTALL_LOCKFILE` — the freshness
    // check (#447) rejects any drift between the on-disk manifest and
    // the lockfile importer entry.
    manifest.add_dependency("placeholder", "1.0.0", DependencyGroup::Prod).unwrap();
    manifest.save().unwrap();

    let mut config = Config::new();
    // Opt out of the GVS layout — the pre-seeded
    // `<virtual_store_dir>/<flat-name>` slot is the legacy shape the
    // skip probe matches under
    // [`warm_reinstall_skips_snapshot_when_current_lockfile_matches`].
    config.enable_global_virtual_store = false;
    config.store_dir = store_dir.into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    let config = config.leak();

    let lockfile: Lockfile = serde_saphyr::from_str(PARTIAL_INSTALL_LOCKFILE)
        .expect("parse partial-install fixture lockfile");

    std::fs::create_dir_all(&virtual_store_dir).unwrap();
    lockfile.save_current_to_virtual_store_dir(&virtual_store_dir).expect("seed current lockfile");
    seed_placeholder_virtual_store_slot(&virtual_store_dir);

    EVENTS.lock().unwrap().clear();
    Install {
        tarball_mem_cache: &Default::default(),
        http_client: &Default::default(),
        config,
        manifest: &manifest,
        lockfile: Some(&lockfile),
        dependency_groups: [DependencyGroup::Prod],
        frozen_lockfile: true,
        supported_architectures: None,
        resolved_packages: &Default::default(),
    }
    .run::<RecordingReporter>()
    .await
    .expect("warm reinstall should succeed via the skip path");

    // Stats reports `added: 0` — the only snapshot is the one that
    // got skipped.
    let added: Vec<u64> = EVENTS
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            LogEvent::Stats(StatsLog { message: StatsMessage::Added { added, .. }, .. }) => {
                Some(*added)
            }
            _ => None,
        })
        .collect();
    assert_eq!(added, vec![0], "warm reinstall must report added: 0; got {added:?}");

    // No per-snapshot `imported` progress event — the skip path
    // removes the snapshot from both warm and cold batches.
    let imported_count = EVENTS
        .lock()
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                e,
                LogEvent::Progress(ProgressLog { message: ProgressMessage::Imported { .. }, .. }),
            )
        })
        .count();
    assert_eq!(
        imported_count, 0,
        "skip path must suppress `pnpm:progress imported` for skipped snapshots",
    );

    drop(dir);
}

/// Issue #447: a `--frozen-lockfile` install where the on-disk
/// `package.json` has drifted from the lockfile importer entry must
/// fail with `OutdatedLockfile` *before* any fetch or link work
/// starts. Mirrors upstream's `ERR_PNPM_OUTDATED_LOCKFILE` thrown
/// from `pkg-manager/core/src/install/index.ts:823` — CI-correctness
/// guarantee that pacquet can't silently install the wrong shape of
/// `node_modules` when the manifest and lockfile diverge.
///
/// We use the partial-install fixture (bogus tarball URL) and *omit*
/// adding the placeholder dep to the manifest. If the check fails to
/// fire, the install reaches the fetch site and errors with a
/// network / integrity failure — distinguishable from the early
/// `OutdatedLockfile` we expect.
#[tokio::test]
async fn frozen_lockfile_errors_when_manifest_drifts_from_lockfile() {
    let dir = tempdir().unwrap();
    let store_dir = dir.path().join("pacquet-store");
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    let manifest_path = dir.path().join("package.json");
    // Deliberately do NOT add the `placeholder` dep — this is the
    // drift case the check has to catch.
    let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

    let mut config = Config::new();
    config.store_dir = store_dir.into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir;
    let config = config.leak();

    let lockfile: Lockfile = serde_saphyr::from_str(PARTIAL_INSTALL_LOCKFILE)
        .expect("parse partial-install fixture lockfile");

    let result = Install {
        tarball_mem_cache: &Default::default(),
        http_client: &Default::default(),
        config,
        manifest: &manifest,
        lockfile: Some(&lockfile),
        dependency_groups: [DependencyGroup::Prod],
        frozen_lockfile: true,
        resolved_packages: &Default::default(),
        supported_architectures: None,
    }
    .run::<SilentReporter>()
    .await;

    let err = result.expect_err("drifted manifest must surface as OutdatedLockfile");
    assert!(
        matches!(err, InstallError::OutdatedLockfile { .. }),
        "expected OutdatedLockfile, got {err:?}",
    );

    drop(dir);
}

/// Negative-case: lockfile loads successfully but has no
/// `importers["."]` entry for the project being installed. Distinct
/// from `NoLockfile` (file missing entirely) — here the file is
/// well-formed but doesn't describe this project. Should surface as
/// `NoImporter`, also before any fetch attempt.
#[tokio::test]
async fn frozen_lockfile_errors_when_lockfile_has_no_root_importer() {
    let dir = tempdir().unwrap();
    let store_dir = dir.path().join("pacquet-store");
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    let manifest_path = dir.path().join("package.json");
    let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

    let mut config = Config::new();
    config.store_dir = store_dir.into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir;
    let config = config.leak();

    // Empty-importers lockfile — valid v9 shape, but no entry for
    // the root project.
    let lockfile: Lockfile =
        serde_saphyr::from_str("lockfileVersion: '9.0'\n").expect("parse minimal lockfile");

    let result = Install {
        tarball_mem_cache: &Default::default(),
        http_client: &Default::default(),
        config,
        manifest: &manifest,
        lockfile: Some(&lockfile),
        dependency_groups: [DependencyGroup::Prod],
        frozen_lockfile: true,
        resolved_packages: &Default::default(),
        supported_architectures: None,
    }
    .run::<SilentReporter>()
    .await;

    let err = result.expect_err("missing root importer must surface as NoImporter");
    assert!(
        matches!(err, InstallError::NoImporter { ref importer_id } if importer_id == "."),
        "expected NoImporter for `.`, got {err:?}",
    );

    drop(dir);
}

/// GVS-on frozen-lockfile install. With
/// `enable_global_virtual_store: true` (pacquet's default, matching
/// upstream's
/// [`config/reader/src/index.ts:392-394`](https://github.com/pnpm/pnpm/blob/94240bc046/config/reader/src/index.ts#L392-L394)),
/// `Install::run` registers the project at
/// `<store_dir>/projects/<short-hash>` (mirroring upstream's
/// [`registerProject`](https://github.com/pnpm/pnpm/blob/94240bc046/store/controller/src/storeController/projectRegistry.ts))
/// and routes every per-snapshot slot through
/// [`crate::VirtualStoreLayout`]. The empty-snapshot lockfile here is
/// enough to prove the wiring runs end-to-end without panicking and
/// that the registry entry actually lands on disk; the GVS-shaped
/// per-package path layout itself is unit-tested inside the
/// [`crate::VirtualStoreLayout`] module, and the e2e port of
/// upstream's `globalVirtualStore.ts` cases (with non-empty
/// snapshots) is tracked as a follow-up.
#[tokio::test]
async fn frozen_lockfile_under_gvs_registers_project_and_runs_clean() {
    let dir = tempdir().unwrap();
    let store_dir = dir.path().join("pacquet-store");
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    // Place the manifest *inside* `project_root` — `Install::run`
    // derives the registry target from `manifest.path().parent()`,
    // so a manifest at `<tmp>/package.json` would register `<tmp>`
    // and the symlink-resolves-to-project_root assertion below
    // would silently pass for the wrong reason.
    std::fs::create_dir_all(&project_root).expect("create project root");
    let manifest_path = project_root.join("package.json");
    let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

    let mut config = Config::new();
    // Pacquet's default is `true`; pin it explicitly so the test
    // doesn't silently degrade if the default flips someday.
    config.enable_global_virtual_store = true;
    config.lockfile = false;
    config.store_dir = store_dir.clone().into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    // Pin the GVS root to a known location under the test temp dir
    // so any future assertions can target it without walking the
    // SmartDefault'd cwd-based fallback.
    config.global_virtual_store_dir = store_dir.join("links");
    let config = config.leak();

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
        supported_architectures: None,
    }
    .run::<SilentReporter>()
    .await
    .expect("frozen-lockfile install under GVS should succeed");

    // `register_project` wrote `<store_dir>/projects/<short-hash>`
    // pointing back at the project dir. Resolve the symlink and
    // canonicalize both ends — the test temp dir may itself be a
    // symlink (e.g. `/tmp` → `/private/tmp` on macOS), and the
    // registry stores the absolute project path at write time.
    let projects_dir = store_dir.join("projects");
    assert!(projects_dir.is_dir(), "GVS-on install must create <store_dir>/projects/");
    let entries: Vec<_> =
        std::fs::read_dir(&projects_dir).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(entries.len(), 1, "exactly one project entry per `Install::run` invocation");
    let entry_target = std::fs::read_link(entries[0].path()).expect("registry entry is a symlink");
    assert_eq!(
        dunce::canonicalize(&entry_target).expect("canonicalize registry target"),
        dunce::canonicalize(&project_root).expect("canonicalize project root"),
        "registry symlink must resolve back to the install's project root",
    );

    drop(dir);
}

/// GVS-off frozen-lockfile install. The dispatch path is the same,
/// but `Install::run` skips the project-registry write entirely.
/// Pins that turning off `enable_global_virtual_store` makes the
/// install behave like today — no `<store_dir>/projects/` directory
/// appears.
#[tokio::test]
async fn frozen_lockfile_with_gvs_off_skips_project_registry() {
    let dir = tempdir().unwrap();
    let store_dir = dir.path().join("pacquet-store");
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    std::fs::create_dir_all(&project_root).expect("create project root");
    let manifest_path = project_root.join("package.json");
    let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();

    let mut config = Config::new();
    config.enable_global_virtual_store = false;
    config.lockfile = false;
    config.store_dir = store_dir.clone().into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    let config = config.leak();

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
        supported_architectures: None,
    }
    .run::<SilentReporter>()
    .await
    .expect("frozen-lockfile install with GVS off should succeed");

    assert!(
        !store_dir.join("projects").exists(),
        "GVS-off install must NOT create the project-registry directory",
    );

    drop(dir);
}

/// Workspace install under GVS registers each importer separately.
/// Mirrors upstream's per-project
/// [`registerProject`](https://github.com/pnpm/pnpm/blob/94240bc046/store/controller/src/storeController/projectRegistry.ts)
/// call site, which fires once per workspace package — a workspace
/// with `.` (root) and `packages/web` therefore ends up with two
/// entries in `<store_dir>/projects/`, each resolving back to its
/// own root dir. `pacquet store prune` (tracked separately) needs
/// every reachable importer in the registry to keep the
/// `<store_dir>/links/...` slots they share alive.
#[tokio::test]
async fn frozen_lockfile_under_gvs_registers_each_workspace_importer() {
    let dir = tempdir().unwrap();
    let store_dir = dir.path().join("pacquet-store");
    let workspace_root = dir.path().join("workspace");
    let modules_dir = workspace_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    // Workspace layout: root + one sub-importer. Both directories
    // have to exist on disk because `register_project` canonicalises
    // the target before writing the symlink.
    let web_dir = workspace_root.join("packages/web");
    std::fs::create_dir_all(&web_dir).expect("create packages/web");
    let manifest_path = workspace_root.join("package.json");
    let manifest = PackageManifest::create_if_needed(manifest_path).unwrap();
    // The sub-importer needs a `package.json` too — the freshness check
    // satisfies on the root only today, but the per-importer registry
    // write still resolves the target on disk.
    std::fs::write(web_dir.join("package.json"), "{}").expect("write packages/web/package.json");

    let mut config = Config::new();
    config.enable_global_virtual_store = true;
    config.lockfile = false;
    config.store_dir = store_dir.clone().into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    config.global_virtual_store_dir = store_dir.join("links");
    let config = config.leak();

    // Two importers: `.` and `packages/web`. Empty dep graph so the
    // install reaches the per-importer registry-write loop without
    // doing any actual fetch/link work.
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies: {}"
        "  packages/web:"
        "    dependencies: {}"
        "packages: {}"
        "snapshots: {}"
    })
    .expect("parse minimal v9 workspace lockfile");

    Install {
        tarball_mem_cache: &Default::default(),
        http_client: &Default::default(),
        config,
        manifest: &manifest,
        lockfile: Some(&lockfile),
        dependency_groups: [DependencyGroup::Prod],
        frozen_lockfile: true,
        resolved_packages: &Default::default(),
        supported_architectures: None,
    }
    .run::<SilentReporter>()
    .await
    .expect("workspace frozen-lockfile install under GVS should succeed");

    // Exactly two registry entries — one per importer. Resolve the
    // symlink targets and confirm both project roots are present.
    let projects_dir = store_dir.join("projects");
    assert!(projects_dir.is_dir(), "GVS-on workspace install must create <store_dir>/projects/");
    let mut targets: Vec<PathBuf> = std::fs::read_dir(&projects_dir)
        .unwrap()
        .map(|entry| {
            let target = std::fs::read_link(entry.unwrap().path()).expect("registry entry");
            dunce::canonicalize(&target).expect("canonicalize registry target")
        })
        .collect();
    targets.sort();
    let mut expected = [
        dunce::canonicalize(&workspace_root).expect("canonicalize workspace root"),
        dunce::canonicalize(&web_dir).expect("canonicalize packages/web"),
    ];
    expected.sort();
    assert_eq!(targets, expected, "every importer must have a registry entry");

    drop(dir);
}
