use super::{AllowBuildPolicy, BuildModules, parse_name_version_from_key};
use pacquet_lockfile::{
    PackageKey, PkgName, PkgVerPeer, ProjectSnapshot, ResolvedDependencyMap,
    ResolvedDependencySpec, SnapshotEntry,
};
use pacquet_reporter::{
    IgnoredScriptsLog, LogEvent, Reporter, SilentReporter, SkippedOptionalReason,
};
use pretty_assertions::assert_eq;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tempfile::tempdir;

/// Build a rules map from a list of (name, allowed) pairs, for tests.
fn rules<const N: usize>(entries: [(&str, bool); N]) -> HashMap<String, bool> {
    entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
fn parse_scoped_key() {
    let (name, version) = parse_name_version_from_key("/@pnpm.e2e/install-script-example@1.0.0");
    assert_eq!(name, "@pnpm.e2e/install-script-example");
    assert_eq!(version, "1.0.0");
}

#[test]
fn parse_unscoped_key() {
    let (name, version) = parse_name_version_from_key("/is-positive@1.0.0");
    assert_eq!(name, "is-positive");
    assert_eq!(version, "1.0.0");
}

#[test]
fn parse_key_without_leading_slash() {
    let (name, version) = parse_name_version_from_key("express@4.18.1");
    assert_eq!(name, "express");
    assert_eq!(version, "4.18.1");
}

// Policy-logic tests below mirror upstream `building/policy/test/index.ts`
// (`https://github.com/pnpm/pnpm/blob/80037699fb/building/policy/test/index.ts`).
// They drive `AllowBuildPolicy::new` directly with in-memory rule maps so the
// policy logic stays decoupled from the manifest reader, exactly the way
// upstream tests drive `createAllowBuildFunction(opts)` decoupled from
// `Config` parsing.

#[test]
fn default_policy_denies_all() {
    let policy = AllowBuildPolicy::default();
    assert_eq!(policy.check("any-package", "1.0.0"), None);
}

#[test]
fn explicit_allow() {
    let policy = AllowBuildPolicy::new(rules([("@pnpm.e2e/install-script-example", true)]), false);
    assert_eq!(policy.check("@pnpm.e2e/install-script-example", "1.0.0"), Some(true));
}

#[test]
fn explicit_deny() {
    let policy = AllowBuildPolicy::new(rules([("@pnpm.e2e/bad-package", false)]), false);
    assert_eq!(policy.check("@pnpm.e2e/bad-package", "1.0.0"), Some(false));
}

#[test]
fn unlisted_returns_none() {
    let policy = AllowBuildPolicy::new(rules([("@pnpm.e2e/allowed", true)]), false);
    assert_eq!(policy.check("@pnpm.e2e/not-listed", "1.0.0"), None);
}

#[test]
fn exact_version_takes_precedence() {
    let policy = AllowBuildPolicy::new(
        rules([("@pnpm.e2e/pkg@1.0.0", true), ("@pnpm.e2e/pkg", false)]),
        false,
    );
    assert_eq!(policy.check("@pnpm.e2e/pkg", "1.0.0"), Some(true));
    assert_eq!(policy.check("@pnpm.e2e/pkg", "2.0.0"), Some(false));
}

#[test]
fn empty_rules_denies_all() {
    let policy = AllowBuildPolicy::new(HashMap::new(), false);
    assert_eq!(policy.check("any-package", "1.0.0"), None);
}

#[test]
fn dangerously_allow_all_builds() {
    let policy = AllowBuildPolicy::new(HashMap::new(), true);
    assert_eq!(policy.check("any-package", "1.0.0"), Some(true));
    assert_eq!(policy.check("other-package", "2.0.0"), Some(true));
}

#[test]
fn dangerously_allow_all_overrides_deny() {
    let policy = AllowBuildPolicy::new(rules([("@pnpm.e2e/pkg", false)]), true);
    assert_eq!(policy.check("@pnpm.e2e/pkg", "1.0.0"), Some(true));
}

// The next two tests exercise `from_manifest` end-to-end: missing manifest
// folds to the empty default, and a real `package.json` round-trips through
// the parser into the same logic the in-memory tests above cover.

#[test]
fn missing_manifest_denies_all() {
    let dir = tempdir().expect("create temp dir");
    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("anything", "1.0.0"), None);
}

#[test]
fn from_manifest_parses_pnpm_section() {
    let dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": {
            "dangerouslyAllowAllBuilds": false,
            "allowBuilds": {
                "@pnpm.e2e/install-script-example": true,
                "@pnpm.e2e/bad-package": false,
            },
        },
    });
    fs::write(dir.path().join("package.json"), manifest.to_string()).expect("write manifest");

    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("@pnpm.e2e/install-script-example", "1.0.0"), Some(true));
    assert_eq!(policy.check("@pnpm.e2e/bad-package", "1.0.0"), Some(false));
    assert_eq!(policy.check("@pnpm.e2e/unrelated", "1.0.0"), None);
}

fn name(s: &str) -> PkgName {
    PkgName::parse(s).expect("parse pkg name")
}

fn ver(s: &str) -> PkgVerPeer {
    s.parse().expect("parse PkgVerPeer")
}

fn key(n: &str, v: &str) -> PackageKey {
    PackageKey::new(name(n), ver(v))
}

/// Materialize a `<virtual_store_dir>/<store_name>/node_modules/<pkg_name>/package.json`
/// fixture so `pkg_requires_build` returns true for `key`. The script body
/// is harmless (`true`) — the existing tests rely on the policy gate to block
/// execution, so the script never actually runs.
fn create_buildable_pkg(virtual_store_dir: &Path, key: &PackageKey) -> PathBuf {
    let key_str = key.without_peer().to_string();
    let name_version = key_str.strip_prefix('/').unwrap_or(&key_str);
    let at_idx = name_version.rfind('@').unwrap_or(name_version.len());
    let pkg_name = &name_version[..at_idx];
    let store_name = name_version.replace('/', "+");
    let pkg_dir = virtual_store_dir.join(&store_name).join("node_modules").join(pkg_name);
    fs::create_dir_all(&pkg_dir).expect("create pkg dir");
    let manifest = serde_json::json!({
        "scripts": { "postinstall": "true" },
    });
    fs::write(pkg_dir.join("package.json"), manifest.to_string()).expect("write manifest");
    pkg_dir
}

fn root_importers(deps: &[(&str, &str)]) -> HashMap<String, ProjectSnapshot> {
    let map: ResolvedDependencyMap = deps
        .iter()
        .map(|(n, v)| {
            (name(n), ResolvedDependencySpec { specifier: (*v).to_string(), version: ver(v) })
        })
        .collect();
    HashMap::from([(
        ".".to_string(),
        ProjectSnapshot {
            specifiers: None,
            dependencies: (!map.is_empty()).then_some(map),
            optional_dependencies: None,
            dev_dependencies: None,
            dependencies_meta: None,
            publish_directory: None,
        },
    )])
}

/// Default-deny: a buildable package not listed in `allowBuilds` lands in
/// the returned ignored set, sorted lexically. "Buildable" is computed from
/// the extracted package directory (postinstall in package.json), matching
/// upstream's `pkgRequiresBuild`.
#[test]
fn build_modules_collects_ignored_builds() {
    let snapshots = HashMap::from([
        (key("zzz", "1.0.0"), SnapshotEntry::default()),
        (key("aaa", "2.0.0"), SnapshotEntry::default()),
    ]);
    let importers = root_importers(&[("zzz", "1.0.0"), ("aaa", "2.0.0")]);
    let policy = AllowBuildPolicy::default(); // empty → default-deny

    let virtual_store_dir = tempdir().expect("create temp dir");
    let modules_dir = tempdir().expect("create temp dir");
    let lockfile_dir = tempdir().expect("create temp dir");

    create_buildable_pkg(virtual_store_dir.path(), &key("zzz", "1.0.0"));
    create_buildable_pkg(virtual_store_dir.path(), &key("aaa", "2.0.0"));

    let ignored = BuildModules {
        virtual_store_dir: virtual_store_dir.path(),
        modules_dir: modules_dir.path(),
        lockfile_dir: lockfile_dir.path(),
        snapshots: Some(&snapshots),
        importers: &importers,
        packages: None,
        allow_build_policy: &policy,
        side_effects_maps_by_snapshot: None,
        engine_name: None,
        side_effects_cache: true,
    }
    .run::<SilentReporter>()
    .expect("run BuildModules");
    dbg!(&ignored);

    assert_eq!(
        ignored,
        vec!["aaa@2.0.0".to_string(), "zzz@1.0.0".to_string()],
        "ignored set must be sorted lexicographically: {ignored:?}",
    );
}

/// Explicit `false` in `allowBuilds` is silently skipped — it does NOT
/// land in the ignored-scripts list. Mirrors upstream
/// `building/during-install/src/index.ts:91-93`.
#[test]
fn build_modules_excludes_explicit_deny_from_ignored() {
    let snapshots = HashMap::from([
        (key("denied", "1.0.0"), SnapshotEntry::default()),
        (key("ignored", "1.0.0"), SnapshotEntry::default()),
    ]);
    let importers = root_importers(&[("denied", "1.0.0"), ("ignored", "1.0.0")]);

    let policy = AllowBuildPolicy::new(rules([("denied", false)]), false);

    let virtual_store_dir = tempdir().expect("create temp dir");
    let modules_dir = tempdir().expect("create temp dir");
    let lockfile_dir = tempdir().expect("create temp dir");

    create_buildable_pkg(virtual_store_dir.path(), &key("denied", "1.0.0"));
    create_buildable_pkg(virtual_store_dir.path(), &key("ignored", "1.0.0"));

    let ignored = BuildModules {
        virtual_store_dir: virtual_store_dir.path(),
        modules_dir: modules_dir.path(),
        lockfile_dir: lockfile_dir.path(),
        snapshots: Some(&snapshots),
        importers: &importers,
        packages: None,
        allow_build_policy: &policy,
        side_effects_maps_by_snapshot: None,
        engine_name: None,
        side_effects_cache: true,
    }
    .run::<SilentReporter>()
    .expect("run BuildModules");
    dbg!(&ignored);

    assert_eq!(
        ignored,
        vec!["ignored@1.0.0".to_string()],
        "explicit-false must NOT appear in ignored set: {ignored:?}",
    );
}

/// Optional dep whose postinstall fails must be reported through the
/// `pnpm:skipped-optional-dependency` channel (reason `build_failure`)
/// and NOT abort the install. Mirrors upstream
/// `building/during-install/src/index.ts:218-240` and the spirit of
/// `'do not fail on an optional dependency that has a non-optional
/// dependency with a failing postinstall script'` at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/installing/deps-installer/test/install/optionalDependencies.ts#L563-L572>.
///
/// The test uses the upstream fixture `@pnpm.e2e/failing-postinstall@1.0.0`
/// (script body verbatim from `/Volumes/src/pnpm/registry-mock/packages/failing-postinstall/package.json`)
/// so the failure mode is exactly the one upstream's optional-dep
/// tests exercise.
///
/// Unix-gated because the upstream script (`echo hello && echo world && exit 1`)
/// is POSIX shell syntax. The cmd-on-Windows path picks a different
/// shell — `pacquet_executor::select_shell` (tested in the executor
/// crate's `shell::tests`) covers the shell-selection branches in
/// isolation.
#[cfg(unix)]
#[test]
fn do_not_fail_on_optional_dep_with_failing_postinstall() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().expect("lock").clear();

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().expect("lock").push(event.clone());
        }
    }

    let pkg_key = key("@pnpm.e2e/failing-postinstall", "1.0.0");
    let mut optional_snapshot = SnapshotEntry::default();
    optional_snapshot.optional = true;
    let snapshots = HashMap::from([(pkg_key.clone(), optional_snapshot)]);
    let importers = root_importers(&[("@pnpm.e2e/failing-postinstall", "1.0.0")]);
    // `dangerouslyAllowAllBuilds` so the policy lets the failing
    // script through to actually run — this test exercises the
    // build-failure path, not the policy gate.
    let policy = AllowBuildPolicy::new(rules([]), true);

    let virtual_store_dir = tempdir().expect("create temp dir");
    let modules_dir = tempdir().expect("create temp dir");
    let lockfile_dir = tempdir().expect("create temp dir");

    create_failing_postinstall_fixture(virtual_store_dir.path(), &pkg_key);

    let ignored = BuildModules {
        virtual_store_dir: virtual_store_dir.path(),
        modules_dir: modules_dir.path(),
        lockfile_dir: lockfile_dir.path(),
        snapshots: Some(&snapshots),
        importers: &importers,
        packages: None,
        allow_build_policy: &policy,
        side_effects_maps_by_snapshot: None,
        engine_name: None,
        side_effects_cache: true,
    }
    .run::<RecordingReporter>()
    .expect("optional build failure must NOT abort the install");
    dbg!(&ignored);

    let captured = EVENTS.lock().expect("lock").clone();
    dbg!(&captured);
    let skipped_event = captured
        .iter()
        .find_map(|e| match e {
            LogEvent::SkippedOptionalDependency(log) => Some(log),
            _ => None,
        })
        .expect("must emit pnpm:skipped-optional-dependency");
    assert_eq!(skipped_event.reason, SkippedOptionalReason::BuildFailure);
    assert_eq!(skipped_event.package.name, "@pnpm.e2e/failing-postinstall");
    assert_eq!(skipped_event.package.version, "1.0.0");
    assert!(skipped_event.details.is_some(), "details must carry the error toString");
}

/// Ports the upstream `'using side effects cache'` test at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/installing/deps-installer/test/install/sideEffects.ts#L79-L131>.
///
/// Upstream runs the install twice — first to populate the cache
/// via the WRITE path, then to consume it. Pacquet doesn't have a
/// WRITE path yet (#421's slice (B)), so we hand-craft the same
/// state directly: a `side_effects_maps_by_snapshot` entry whose
/// cache key matches what `BuildModules` will compute via
/// `calc_dep_state`. With that in place, the gate skips the build
/// even though the package's `postinstall` would have failed —
/// observable via the absence of a `pnpm:lifecycle` event for the
/// stage.
///
/// The fixture (`@pnpm.e2e/failing-postinstall@1.0.0`, `postinstall:
/// echo hello && echo world && exit 1`) is the same upstream's
/// own tests use; if the gate were broken the build would run and
/// the install would propagate the exit-1 failure (cf.
/// `fail_when_failing_postinstall_is_required` below).
#[cfg(unix)]
#[test]
fn using_side_effects_cache_skips_rebuild() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().expect("lock").clear();

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().expect("lock").push(event.clone());
        }
    }

    let pkg_key = key("@pnpm.e2e/failing-postinstall", "1.0.0");
    let snapshots = HashMap::from([(pkg_key.clone(), SnapshotEntry::default())]);
    let packages: HashMap<pacquet_lockfile::PackageKey, pacquet_lockfile::PackageMetadata> =
        HashMap::from([(
            pkg_key.without_peer(),
            pacquet_lockfile::PackageMetadata {
                resolution: pacquet_lockfile::LockfileResolution::Registry(
                    pacquet_lockfile::RegistryResolution {
                        integrity: "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                            .parse()
                            .expect("parse integrity"),
                    },
                ),
                engines: None,
                cpu: None,
                os: None,
                libc: None,
                deprecated: None,
                has_bin: None,
                prepare: None,
                bundled_dependencies: None,
                peer_dependencies: None,
                peer_dependencies_meta: None,
            },
        )]);
    let importers = root_importers(&[("@pnpm.e2e/failing-postinstall", "1.0.0")]);
    let policy = AllowBuildPolicy::new(rules([]), true);

    let virtual_store_dir = tempdir().expect("create temp dir");
    let modules_dir = tempdir().expect("create temp dir");
    let lockfile_dir = tempdir().expect("create temp dir");

    create_failing_postinstall_fixture(virtual_store_dir.path(), &pkg_key);

    // Compute the cache key the same way `BuildModules` will, then
    // pre-populate `side_effects_maps_by_snapshot` with a matching
    // entry. The inner FilesMap value is irrelevant for this
    // assertion — only presence of the key matters for the gate.
    let engine = "darwin;arm64;node20";
    let dep_graph = crate::build_deps_graph(&snapshots, &packages);
    let mut state_cache = pacquet_graph_hasher::DepsStateCache::new();
    let expected_cache_key = pacquet_graph_hasher::calc_dep_state(
        &dep_graph,
        &mut state_cache,
        &pkg_key,
        &pacquet_graph_hasher::CalcDepStateOptions {
            engine_name: engine,
            patch_file_hash: None,
            include_dep_graph_hash: true,
        },
    );
    let mut overlay = std::collections::HashMap::new();
    overlay.insert(expected_cache_key, std::collections::HashMap::new());
    let mut side_effects_maps = std::collections::HashMap::new();
    side_effects_maps.insert(pkg_key.clone(), std::sync::Arc::new(overlay));

    BuildModules {
        virtual_store_dir: virtual_store_dir.path(),
        modules_dir: modules_dir.path(),
        lockfile_dir: lockfile_dir.path(),
        snapshots: Some(&snapshots),
        packages: Some(&packages),
        importers: &importers,
        allow_build_policy: &policy,
        side_effects_maps_by_snapshot: Some(&side_effects_maps),
        engine_name: Some(engine),
        side_effects_cache: true,
    }
    .run::<RecordingReporter>()
    .expect("install must succeed when the cache hit skips the rebuild");

    // The build was skipped, so no `pnpm:lifecycle` event for the
    // postinstall stage should have been emitted. If the gate were
    // broken the failing-postinstall script would have run and
    // emitted a `Script` (and a non-zero `Exit`) event, plus
    // returned `Err(BuildModulesError::LifecycleScript(...))` from
    // `.run()`.
    let captured = EVENTS.lock().expect("lock").clone();
    let any_lifecycle = captured.iter().any(|e| matches!(e, LogEvent::Lifecycle(_)));
    assert!(!any_lifecycle, "side-effects cache hit must skip lifecycle scripts: {captured:#?}");
}

/// Negative pair: with `side_effects_cache = false`, even a
/// matching cache entry is ignored — the build runs. Mirrors
/// upstream's `sideEffectsCache: false` config branch.
#[cfg(unix)]
#[test]
fn side_effects_cache_disabled_bypasses_the_gate() {
    let pkg_key = key("@pnpm.e2e/failing-postinstall", "1.0.0");
    let snapshots = HashMap::from([(pkg_key.clone(), SnapshotEntry::default())]);
    let packages: HashMap<pacquet_lockfile::PackageKey, pacquet_lockfile::PackageMetadata> =
        HashMap::new();
    let importers = root_importers(&[("@pnpm.e2e/failing-postinstall", "1.0.0")]);
    let policy = AllowBuildPolicy::new(rules([]), true);

    let virtual_store_dir = tempdir().expect("create temp dir");
    let modules_dir = tempdir().expect("create temp dir");
    let lockfile_dir = tempdir().expect("create temp dir");

    create_failing_postinstall_fixture(virtual_store_dir.path(), &pkg_key);

    // Same overlay shape as the positive test, but the
    // `side_effects_cache: false` flag must short-circuit before
    // the lookup even runs.
    let mut overlay = std::collections::HashMap::new();
    overlay.insert("any-key".to_string(), std::collections::HashMap::new());
    let mut side_effects_maps = std::collections::HashMap::new();
    side_effects_maps.insert(pkg_key.clone(), std::sync::Arc::new(overlay));

    let err = BuildModules {
        virtual_store_dir: virtual_store_dir.path(),
        modules_dir: modules_dir.path(),
        lockfile_dir: lockfile_dir.path(),
        snapshots: Some(&snapshots),
        packages: Some(&packages),
        importers: &importers,
        allow_build_policy: &policy,
        side_effects_maps_by_snapshot: Some(&side_effects_maps),
        engine_name: Some("darwin;arm64;node20"),
        side_effects_cache: false,
    }
    .run::<SilentReporter>()
    .expect_err("with cache disabled, the failing postinstall must run and the install must fail");
    assert!(matches!(err, crate::build_modules::BuildModulesError::LifecycleScript(_)));
}

/// Mirrors `'fail on a package with failing postinstall if the
/// package is both an optional and non-optional dependency'` at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/installing/deps-installer/test/install/optionalDependencies.ts#L574-L591>.
///
/// Upstream's resolver folds reachability ALL-paths-optional, so a
/// package reachable through any non-optional edge has
/// `snapshots[...].optional = false` in the lockfile (cf.
/// `installing/deps-resolver/src/resolveDependencies.ts:1605-1610`).
/// `BuildModules` then propagates the build failure rather than
/// swallowing it. Pacquet trusts the precomputed flag; this test
/// pins the propagation branch by supplying the same fixture with
/// `optional: false`, which is the lockfile shape upstream produces
/// for the dual-reachability case.
#[cfg(unix)]
#[test]
fn fail_when_failing_postinstall_is_required() {
    let pkg_key = key("@pnpm.e2e/failing-postinstall", "1.0.0");
    // `optional: false` — pacquet's analog of upstream's
    // ALL-paths-optional fold concluding the dep is required.
    let snapshots = HashMap::from([(pkg_key.clone(), SnapshotEntry::default())]);
    let importers = root_importers(&[("@pnpm.e2e/failing-postinstall", "1.0.0")]);
    let policy = AllowBuildPolicy::new(rules([]), true);

    let virtual_store_dir = tempdir().expect("create temp dir");
    let modules_dir = tempdir().expect("create temp dir");
    let lockfile_dir = tempdir().expect("create temp dir");

    create_failing_postinstall_fixture(virtual_store_dir.path(), &pkg_key);

    let err = BuildModules {
        virtual_store_dir: virtual_store_dir.path(),
        modules_dir: modules_dir.path(),
        lockfile_dir: lockfile_dir.path(),
        snapshots: Some(&snapshots),
        importers: &importers,
        packages: None,
        allow_build_policy: &policy,
        side_effects_maps_by_snapshot: None,
        engine_name: None,
        side_effects_cache: true,
    }
    .run::<SilentReporter>()
    .expect_err("required build failure must propagate");
    eprintln!("ERR: {err}");
    assert!(matches!(err, crate::build_modules::BuildModulesError::LifecycleScript(_)));
}

/// Materialize a package fixture whose contents are byte-identical
/// to upstream's `@pnpm.e2e/failing-postinstall@1.0.0` at
/// `/Volumes/src/pnpm/registry-mock/packages/failing-postinstall/package.json`.
/// Reusing the upstream script body (`echo hello && echo world && exit 1`)
/// keeps the failure mode and exit code identical to what
/// `optionalDependencies.ts` exercises against the live mock
/// registry, without dragging the lockfile-with-real-integrity
/// machinery into a `BuildModules`-unit test.
fn create_failing_postinstall_fixture(virtual_store_dir: &Path, key: &PackageKey) -> PathBuf {
    let key_str = key.without_peer().to_string();
    let name_version = key_str.strip_prefix('/').unwrap_or(&key_str);
    let at_idx = name_version.rfind('@').unwrap_or(name_version.len());
    let pkg_name = &name_version[..at_idx];
    let store_name = name_version.replace('/', "+");
    let pkg_dir = virtual_store_dir.join(&store_name).join("node_modules").join(pkg_name);
    fs::create_dir_all(&pkg_dir).expect("create pkg dir");
    let manifest = serde_json::json!({
        "name": pkg_name,
        "version": name_version[at_idx + 1..].to_string(),
        "scripts": { "postinstall": "echo hello && echo world && exit 1" },
    });
    fs::write(pkg_dir.join("package.json"), manifest.to_string()).expect("write manifest");
    pkg_dir
}

/// Recording fake confirms `pnpm:ignored-scripts` is the right channel
/// for the package list. The frozen-install path emits this once after
/// `BuildModules::run` returns; this test exercises the equivalent
/// emit shape directly so `LogEvent::IgnoredScripts` stays connected
/// to the BuildModules return value.
#[test]
fn ignored_scripts_event_carries_returned_names() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().expect("lock").clear();

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().expect("lock").push(event.clone());
        }
    }

    let names = vec!["a@1.0.0".to_string(), "b@2.0.0".to_string()];
    RecordingReporter::emit(&LogEvent::IgnoredScripts(IgnoredScriptsLog {
        level: pacquet_reporter::LogLevel::Debug,
        package_names: names.clone(),
    }));

    let captured = EVENTS.lock().expect("lock").clone();
    dbg!(&captured);
    assert!(
        matches!(
            captured.as_slice(),
            [LogEvent::IgnoredScripts(IgnoredScriptsLog { package_names, .. })]
                if package_names == &names,
        ),
        "captured: {captured:?}",
    );
}
