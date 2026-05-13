use super::{
    BadWorkspaceManifestNameError, FindWorkspaceDirError, INVALID_WORKSPACE_MANIFEST_FILENAMES,
    find_workspace_dir,
};
use crate::WORKSPACE_MANIFEST_FILENAME;
use pretty_assertions::assert_eq;
use std::fs;
use tempfile::TempDir;

/// `pnpm-workspace.yaml` exists at the start dir → returns that dir.
#[test]
fn finds_workspace_dir_at_start() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(WORKSPACE_MANIFEST_FILENAME), "packages:\n  - pkgs/*\n").unwrap();
    let found = find_workspace_dir(tmp.path()).unwrap();
    assert_eq!(found.as_deref(), Some(tmp.path()));
}

/// Walk up: start dir is a child, manifest lives in an ancestor.
#[test]
fn finds_workspace_dir_in_ancestor() {
    let tmp = TempDir::new().unwrap();
    let nested = tmp.path().join("packages").join("a");
    fs::create_dir_all(&nested).unwrap();
    fs::write(tmp.path().join(WORKSPACE_MANIFEST_FILENAME), "packages:\n  - packages/*\n").unwrap();
    let found = find_workspace_dir(&nested).unwrap();
    assert_eq!(found.as_deref(), Some(tmp.path()));
}

/// No `pnpm-workspace.yaml` anywhere → `Ok(None)` (not an error).
#[test]
fn returns_none_when_no_manifest() {
    let tmp = TempDir::new().unwrap();
    let found = find_workspace_dir(tmp.path()).unwrap();
    assert_eq!(found, None);
}

/// `pnpm-workspace.yml` (or any other misnamed variant) → error.
/// One sub-test per variant so a failure points at the exact filename.
#[test]
fn rejects_invalid_filenames() {
    for bad in INVALID_WORKSPACE_MANIFEST_FILENAMES {
        let tmp = TempDir::new().unwrap();
        let bad_path = tmp.path().join(bad);
        fs::write(&bad_path, "packages: [a]\n").unwrap();
        let err = find_workspace_dir(tmp.path()).unwrap_err();
        match err {
            FindWorkspaceDirError::BadName(BadWorkspaceManifestNameError { path }) => {
                assert_eq!(path, bad_path, "bad variant: {bad}");
            }
        }
    }
}

/// When both the correct file and a misnamed variant are present,
/// the correct one wins — upstream's `findUp` returns the first match
/// in pattern order at each level, but the misnamed-variant check
/// applies only after the correct file is ruled out at the current
/// level. Same reasoning preserved here.
#[test]
fn correct_filename_wins_over_misnamed_sibling() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(WORKSPACE_MANIFEST_FILENAME), "packages:\n  - pkgs/*\n").unwrap();
    fs::write(tmp.path().join("pnpm-workspace.yml"), "packages: [bad]\n").unwrap();
    let found = find_workspace_dir(tmp.path()).unwrap();
    assert_eq!(found.as_deref(), Some(tmp.path()));
}
