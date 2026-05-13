use super::{
    BadWorkspaceManifestNameError, FindWorkspaceDirError, INVALID_WORKSPACE_MANIFEST_FILENAMES,
    find_workspace_dir, find_workspace_dir_from_env,
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

/// An empty `NPM_CONFIG_WORKSPACE_DIR` must be treated as unset so
/// the upward walk takes over. Otherwise an exported-but-empty
/// variable would short-circuit discovery and force the install into
/// `PathBuf::from("")`. Mirrors upstream's truthy `if (workspaceDir)`
/// check.
///
/// `std::env::set_var` mutates process-wide state, so this test is
/// `#[serial]`-ish in spirit; in practice no other test in the crate
/// reads these vars, so we keep it simple and just restore at the end.
#[test]
fn empty_env_var_is_treated_as_unset() {
    use std::env;
    const VAR: &str = "NPM_CONFIG_WORKSPACE_DIR";

    let prior = env::var_os(VAR);
    // SAFETY: single-threaded test runner pinpoints this var to one
    // test process; no other test reads it. Restored below.
    unsafe { env::set_var(VAR, "") };
    let result = find_workspace_dir_from_env();
    // Restore before asserting so a failure doesn't pollute neighbors.
    match prior {
        Some(value) => unsafe { env::set_var(VAR, value) },
        None => unsafe { env::remove_var(VAR) },
    }
    assert_eq!(result, None, "empty env var must fall through to the upward walk");
}
