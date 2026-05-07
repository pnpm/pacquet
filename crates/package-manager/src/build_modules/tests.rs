use super::{AllowBuildPolicy, BuildModules, parse_name_version_from_key};
use pacquet_lockfile::{
    LockfileResolution, PackageKey, PackageMetadata, PkgName, PkgVerPeer, ProjectSnapshot,
    RegistryResolution, ResolvedDependencyMap, ResolvedDependencySpec, SnapshotEntry,
};
use pacquet_reporter::{IgnoredScriptsLog, LogEvent, Reporter, SilentReporter};
use pretty_assertions::assert_eq;
use ssri::Integrity;
use std::{collections::HashMap, fs, sync::Mutex};
use tempfile::tempdir;

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

#[test]
fn default_policy_denies_all() {
    let policy = AllowBuildPolicy::default();
    assert_eq!(policy.check("any-package", "1.0.0"), None);
}

#[test]
fn explicit_allow() {
    let dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": {
            "allowBuilds": {
                "@pnpm.e2e/install-script-example": true,
            },
        },
    });
    fs::write(dir.path().join("package.json"), manifest.to_string()).expect("write manifest");

    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("@pnpm.e2e/install-script-example", "1.0.0"), Some(true));
}

#[test]
fn explicit_deny() {
    let dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": {
            "allowBuilds": {
                "@pnpm.e2e/bad-package": false,
            },
        },
    });
    fs::write(dir.path().join("package.json"), manifest.to_string()).expect("write manifest");

    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("@pnpm.e2e/bad-package", "1.0.0"), Some(false));
}

#[test]
fn unlisted_returns_none() {
    let dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": {
            "allowBuilds": {
                "@pnpm.e2e/allowed": true,
            },
        },
    });
    fs::write(dir.path().join("package.json"), manifest.to_string()).expect("write manifest");

    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("@pnpm.e2e/not-listed", "1.0.0"), None);
}

#[test]
fn exact_version_takes_precedence() {
    let dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": {
            "allowBuilds": {
                "@pnpm.e2e/pkg@1.0.0": true,
                "@pnpm.e2e/pkg": false,
            },
        },
    });
    fs::write(dir.path().join("package.json"), manifest.to_string()).expect("write manifest");

    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("@pnpm.e2e/pkg", "1.0.0"), Some(true));
    assert_eq!(policy.check("@pnpm.e2e/pkg", "2.0.0"), Some(false));
}

#[test]
fn missing_manifest_denies_all() {
    let dir = tempdir().expect("create temp dir");
    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("anything", "1.0.0"), None);
}

#[test]
fn empty_allow_builds_denies_all() {
    let dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": {
            "allowBuilds": {},
        },
    });
    fs::write(dir.path().join("package.json"), manifest.to_string()).expect("write manifest");

    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("any-package", "1.0.0"), None);
}

#[test]
fn dangerously_allow_all_builds() {
    let dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": {
            "dangerouslyAllowAllBuilds": true,
        },
    });
    fs::write(dir.path().join("package.json"), manifest.to_string()).expect("write manifest");

    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("any-package", "1.0.0"), Some(true));
    assert_eq!(policy.check("other-package", "2.0.0"), Some(true));
}

#[test]
fn dangerously_allow_all_overrides_deny() {
    let dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": {
            "dangerouslyAllowAllBuilds": true,
            "allowBuilds": {
                "@pnpm.e2e/pkg": false,
            },
        },
    });
    fs::write(dir.path().join("package.json"), manifest.to_string()).expect("write manifest");

    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("@pnpm.e2e/pkg", "1.0.0"), Some(true));
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

fn pkg_meta(requires_build: Option<bool>) -> PackageMetadata {
    PackageMetadata {
        resolution: LockfileResolution::Registry(RegistryResolution {
            integrity: "sha512-deadbeef".parse::<Integrity>().expect("parse integrity"),
        }),
        engines: None,
        cpu: None,
        os: None,
        libc: None,
        deprecated: None,
        has_bin: None,
        prepare: None,
        requires_build,
        bundled_dependencies: None,
        peer_dependencies: None,
        peer_dependencies_meta: None,
    }
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

/// Default-deny: a `requires_build: true` package not listed in
/// `allowBuilds` lands in the returned ignored set, sorted lexically.
#[test]
fn build_modules_collects_ignored_builds() {
    let snapshots = HashMap::from([
        (key("zzz", "1.0.0"), SnapshotEntry::default()),
        (key("aaa", "2.0.0"), SnapshotEntry::default()),
    ]);
    let packages = HashMap::from([
        (key("zzz", "1.0.0"), pkg_meta(Some(true))),
        (key("aaa", "2.0.0"), pkg_meta(Some(true))),
    ]);
    let importers = root_importers(&[("zzz", "1.0.0"), ("aaa", "2.0.0")]);
    let policy = AllowBuildPolicy::default(); // empty → default-deny

    let virtual_store_dir = tempdir().expect("create temp dir");
    let modules_dir = tempdir().expect("create temp dir");
    let lockfile_dir = tempdir().expect("create temp dir");

    let ignored = BuildModules {
        virtual_store_dir: virtual_store_dir.path(),
        modules_dir: modules_dir.path(),
        lockfile_dir: lockfile_dir.path(),
        packages: Some(&packages),
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
    let packages = HashMap::from([
        (key("denied", "1.0.0"), pkg_meta(Some(true))),
        (key("ignored", "1.0.0"), pkg_meta(Some(true))),
    ]);
    let importers = root_importers(&[("denied", "1.0.0"), ("ignored", "1.0.0")]);

    let manifest_dir = tempdir().expect("create temp dir");
    let manifest = serde_json::json!({
        "pnpm": { "allowBuilds": { "denied": false } },
    });
    fs::write(manifest_dir.path().join("package.json"), manifest.to_string())
        .expect("write manifest");
    let policy = AllowBuildPolicy::from_manifest(manifest_dir.path());

    let virtual_store_dir = tempdir().expect("create temp dir");
    let modules_dir = tempdir().expect("create temp dir");

    let ignored = BuildModules {
        virtual_store_dir: virtual_store_dir.path(),
        modules_dir: modules_dir.path(),
        lockfile_dir: manifest_dir.path(),
        packages: Some(&packages),
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
