use crate::from_manifest::{LoadPatchedDependenciesError, load_patched_dependencies_from_manifest};
use pretty_assertions::assert_eq;
use std::fs;
use tempfile::tempdir;

fn write_manifest(dir: &std::path::Path, body: &str) {
    fs::write(dir.join("package.json"), body).unwrap();
}

/// SHA-256 of `"hello\n"`, used as the canonical patch body in
/// fixture tests below.
const HELLO_SHA256_HEX: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

#[test]
fn missing_package_json_returns_none() {
    let dir = tempdir().unwrap();
    let result = load_patched_dependencies_from_manifest(dir.path()).unwrap();
    assert_eq!(result, None);
}

#[test]
fn manifest_without_pnpm_returns_none() {
    let dir = tempdir().unwrap();
    write_manifest(dir.path(), r#"{ "name": "x", "version": "1.0.0" }"#);
    let result = load_patched_dependencies_from_manifest(dir.path()).unwrap();
    assert_eq!(result, None);
}

#[test]
fn manifest_without_patched_dependencies_returns_none() {
    let dir = tempdir().unwrap();
    write_manifest(dir.path(), r#"{ "name": "x", "pnpm": { "allowBuilds": {} } }"#);
    let result = load_patched_dependencies_from_manifest(dir.path()).unwrap();
    assert_eq!(result, None);
}

#[test]
fn empty_patched_dependencies_returns_none() {
    let dir = tempdir().unwrap();
    write_manifest(dir.path(), r#"{ "pnpm": { "patchedDependencies": {} } }"#);
    let result = load_patched_dependencies_from_manifest(dir.path()).unwrap();
    assert_eq!(result, None);
}

#[test]
fn resolves_relative_paths_and_hashes_files() {
    let dir = tempdir().unwrap();
    let patches = dir.path().join("patches");
    fs::create_dir(&patches).unwrap();
    fs::write(patches.join("lodash@4.17.21.patch"), b"hello\n").unwrap();

    write_manifest(
        dir.path(),
        r#"{
            "pnpm": {
                "patchedDependencies": {
                    "lodash@4.17.21": "patches/lodash@4.17.21.patch"
                }
            }
        }"#,
    );

    let groups = load_patched_dependencies_from_manifest(dir.path()).unwrap().unwrap();
    let lodash = groups.get("lodash").expect("lodash group");
    let exact = lodash.exact.get("4.17.21").expect("exact entry present");
    assert_eq!(exact.hash, HELLO_SHA256_HEX);
    assert_eq!(exact.key, "lodash@4.17.21");
    assert_eq!(
        exact.patch_file_path.as_deref(),
        Some(patches.join("lodash@4.17.21.patch")).as_deref()
    );
}

#[test]
fn absolute_path_used_verbatim() {
    let dir = tempdir().unwrap();
    let patch_file = dir.path().join("absolute.patch");
    fs::write(&patch_file, b"hello\n").unwrap();

    let manifest = format!(
        r#"{{
            "pnpm": {{
                "patchedDependencies": {{
                    "foo@1.0.0": {:?}
                }}
            }}
        }}"#,
        patch_file.display().to_string()
    );
    write_manifest(dir.path(), &manifest);

    let groups = load_patched_dependencies_from_manifest(dir.path()).unwrap().unwrap();
    let foo = groups.get("foo").expect("foo group");
    let exact = foo.exact.get("1.0.0").expect("exact 1.0.0 present");
    assert_eq!(exact.patch_file_path.as_deref(), Some(patch_file.as_path()));
}

#[test]
fn nonexistent_patch_file_errors() {
    let dir = tempdir().unwrap();
    write_manifest(
        dir.path(),
        r#"{
            "pnpm": {
                "patchedDependencies": {
                    "foo@1.0.0": "patches/missing.patch"
                }
            }
        }"#,
    );
    let err = load_patched_dependencies_from_manifest(dir.path()).unwrap_err();
    assert!(matches!(err, LoadPatchedDependenciesError::Hash(_)));
}

#[test]
fn malformed_manifest_errors() {
    let dir = tempdir().unwrap();
    write_manifest(dir.path(), "{ not valid json");
    let err = load_patched_dependencies_from_manifest(dir.path()).unwrap_err();
    assert!(matches!(err, LoadPatchedDependenciesError::ParseManifest { .. }));
}

#[test]
fn invalid_shape_errors() {
    let dir = tempdir().unwrap();
    // `patchedDependencies` is an array — not an object.
    write_manifest(
        dir.path(),
        r#"{ "pnpm": { "patchedDependencies": ["foo@1.0.0", "patches/foo.patch"] } }"#,
    );
    let err = load_patched_dependencies_from_manifest(dir.path()).unwrap_err();
    assert!(matches!(err, LoadPatchedDependenciesError::InvalidShape { .. }));
}

#[test]
fn invalid_value_type_errors() {
    let dir = tempdir().unwrap();
    write_manifest(dir.path(), r#"{ "pnpm": { "patchedDependencies": { "foo@1.0.0": 42 } } }"#);
    let err = load_patched_dependencies_from_manifest(dir.path()).unwrap_err();
    assert!(matches!(err, LoadPatchedDependenciesError::InvalidShape { .. }));
}

#[test]
fn invalid_version_range_propagates() {
    let dir = tempdir().unwrap();
    let patches = dir.path().join("patches");
    fs::create_dir(&patches).unwrap();
    fs::write(patches.join("foo.patch"), b"hello\n").unwrap();

    write_manifest(
        dir.path(),
        r#"{
            "pnpm": {
                "patchedDependencies": {
                    "foo@link:packages/foo": "patches/foo.patch"
                }
            }
        }"#,
    );
    let err = load_patched_dependencies_from_manifest(dir.path()).unwrap_err();
    assert!(matches!(err, LoadPatchedDependenciesError::Range(_)));
}
