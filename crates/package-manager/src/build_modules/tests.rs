use super::{AllowBuildPolicy, parse_name_version_from_key, parse_name_version_from_store_entry};
use pretty_assertions::assert_eq;
use std::fs;
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
fn parse_scoped_store_entry() {
    let (name, version) =
        parse_name_version_from_store_entry("@pnpm.e2e+install-script-example@1.0.0");
    assert_eq!(name, "@pnpm.e2e/install-script-example");
    assert_eq!(version, "1.0.0");
}

#[test]
fn parse_unscoped_store_entry() {
    let (name, version) = parse_name_version_from_store_entry("is-positive@1.0.0");
    assert_eq!(name, "is-positive");
    assert_eq!(version, "1.0.0");
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
