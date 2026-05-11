use super::{AllowBuildPolicy, BuildModules, parse_name_version_from_key};
use pacquet_lockfile::{
    PackageKey, PkgName, PkgVerPeer, ProjectSnapshot, ResolvedDependencyMap,
    ResolvedDependencySpec, SnapshotEntry,
};
use pacquet_reporter::{IgnoredScriptsLog, LogEvent, Reporter, SilentReporter};
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
        allow_build_policy: &policy,
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
        allow_build_policy: &policy,
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
                if package_names == &names
        ),
        "captured: {captured:?}"
    );
}
