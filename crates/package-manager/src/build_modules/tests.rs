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
fn allow_build_policy_no_rules_allows_all() {
    let policy = AllowBuildPolicy::default();
    assert_eq!(policy.check("any-package", "1.0.0"), Some(true));
}

#[test]
fn allow_build_policy_explicit_allow() {
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
fn allow_build_policy_explicit_deny() {
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
fn allow_build_policy_unlisted_returns_none() {
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
fn allow_build_policy_exact_version_match() {
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
fn allow_build_policy_missing_manifest() {
    let dir = tempdir().expect("create temp dir");
    let policy = AllowBuildPolicy::from_manifest(dir.path());
    assert_eq!(policy.check("anything", "1.0.0"), Some(true));
}

#[test]
fn allow_build_policy_empty_object_blocks_unlisted() {
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
