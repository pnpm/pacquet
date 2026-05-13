use super::{
    InvalidWorkspaceManifestError, ReadWorkspaceManifestError, WORKSPACE_MANIFEST_FILENAME,
    WorkspaceManifest, read_workspace_manifest,
};
use pretty_assertions::assert_eq;
use std::fs;
use tempfile::TempDir;

#[test]
fn missing_file_returns_none() {
    let tmp = TempDir::new().unwrap();
    let manifest = read_workspace_manifest(tmp.path()).unwrap();
    assert_eq!(manifest, None);
}

#[test]
fn empty_file_returns_default() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(WORKSPACE_MANIFEST_FILENAME), "").unwrap();
    let manifest = read_workspace_manifest(tmp.path()).unwrap();
    assert_eq!(manifest, Some(WorkspaceManifest::default()));
}

#[test]
fn parses_packages_array() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join(WORKSPACE_MANIFEST_FILENAME),
        "packages:\n  - packages/*\n  - apps/*\n",
    )
    .unwrap();
    let manifest = read_workspace_manifest(tmp.path()).unwrap().unwrap();
    assert_eq!(manifest.packages, vec!["packages/*".to_string(), "apps/*".to_string()]);
}

/// Settings-only manifests (no `packages:`) still produce a valid
/// manifest with an empty `packages` list. Matches upstream's
/// `validateWorkspaceManifest`, which only errors on a *non-array*
/// `packages` value — omitted is fine.
#[test]
fn settings_only_manifest_has_empty_packages() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join(WORKSPACE_MANIFEST_FILENAME),
        "storeDir: /tmp/store\nregistry: https://example.com/\n",
    )
    .unwrap();
    let manifest = read_workspace_manifest(tmp.path()).unwrap().unwrap();
    assert_eq!(manifest.packages, Vec::<String>::new());
}

#[test]
fn empty_package_entry_rejected() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(WORKSPACE_MANIFEST_FILENAME), "packages:\n  - ''\n  - apps/*\n")
        .unwrap();
    let err = read_workspace_manifest(tmp.path()).unwrap_err();
    assert!(
        matches!(
            err,
            ReadWorkspaceManifestError::Invalid(InvalidWorkspaceManifestError::EmptyPackageEntry),
        ),
        "unexpected error: {err}",
    );
}
