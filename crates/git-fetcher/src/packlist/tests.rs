use super::packlist;
use serde_json::json;
use std::{fs, path::Path};
use tempfile::tempdir;

fn touch(root: &Path, rel: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, "").unwrap();
}

#[test]
fn includes_everything_when_files_field_absent() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "index.js");
    touch(root, "lib/inner.js");

    let manifest = json!({ "name": "x", "version": "0.0.0" });
    let mut out = packlist(root, &manifest).unwrap();
    out.sort();

    assert_eq!(out, vec!["index.js".to_string(), "lib/inner.js".into(), "package.json".into()]);
}

#[test]
fn excludes_git_and_node_modules_subtrees() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "index.js");
    touch(root, ".git/HEAD");
    touch(root, "node_modules/.bin/foo");
    touch(root, "node_modules/foo/index.js");

    let manifest = json!({ "name": "x", "version": "0.0.0" });
    let mut out = packlist(root, &manifest).unwrap();
    out.sort();

    assert_eq!(out, vec!["index.js".to_string(), "package.json".into()]);
}

#[test]
fn excludes_cruft_files_at_any_depth() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "src/file.js");
    touch(root, "src/file.js.orig");
    touch(root, ".DS_Store");
    touch(root, "npm-debug.log");
    touch(root, "package-lock.json");

    let manifest = json!({ "name": "x", "version": "0.0.0" });
    let mut out = packlist(root, &manifest).unwrap();
    out.sort();

    assert_eq!(out, vec!["package.json".to_string(), "src/file.js".into()]);
}

#[test]
fn files_field_restricts_to_listed_globs() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "dist/index.js");
    touch(root, "dist/sub/inner.js");
    touch(root, "src/index.ts");
    touch(root, "README.md");
    touch(root, "LICENSE");

    let manifest = json!({
        "name": "x",
        "version": "0.0.0",
        "files": ["dist/**"],
    });
    let mut out = packlist(root, &manifest).unwrap();
    out.sort();

    assert_eq!(
        out,
        vec![
            "LICENSE".to_string(),
            "README.md".into(),
            "dist/index.js".into(),
            "dist/sub/inner.js".into(),
            "package.json".into(),
        ],
        "always-included files (README/LICENSE/package.json) ship alongside the `files` glob",
    );
}

#[test]
fn main_and_bin_paths_are_force_included_under_files_field() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "lib/index.js");
    touch(root, "bin/cli");
    touch(root, "dist/index.js");

    let manifest = json!({
        "name": "x",
        "version": "0.0.0",
        "files": ["dist/**"],
        "main": "lib/index.js",
        "bin": { "x-cli": "bin/cli" },
    });
    let mut out = packlist(root, &manifest).unwrap();
    out.sort();

    assert!(out.contains(&"lib/index.js".to_string()));
    assert!(out.contains(&"bin/cli".to_string()));
    assert!(out.contains(&"dist/index.js".to_string()));
}

#[test]
fn question_mark_does_not_cross_directory() {
    // Regression: `?` matches a single non-slash byte, not arbitrary
    // characters. Without the explicit `/` guard, `a?b/index.js` would
    // incorrectly match `a/b/index.js`.
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "a/b/index.js");

    let manifest = json!({
        "name": "x",
        "version": "0.0.0",
        "files": ["a?b/index.js"],
    });
    let out = packlist(root, &manifest).unwrap();

    assert!(!out.iter().any(|p| p == "a/b/index.js"), "`?` must not match `/`; received {out:?}");
}

#[test]
fn single_star_does_not_cross_directory() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "lib/index.js");
    touch(root, "lib/sub/inner.js");

    let manifest = json!({
        "name": "x",
        "version": "0.0.0",
        "files": ["lib/*.js"],
    });
    let mut out = packlist(root, &manifest).unwrap();
    out.sort();

    assert!(out.contains(&"lib/index.js".to_string()));
    assert!(!out.contains(&"lib/sub/inner.js".to_string()));
}

#[test]
fn npmignore_excludes_listed_paths() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "index.js");
    touch(root, "test/foo.test.js");
    fs::write(root.join(".npmignore"), "test/\n").unwrap();

    let manifest = json!({ "name": "x", "version": "0.0.0" });
    let mut out = packlist(root, &manifest).unwrap();
    out.sort();

    assert!(out.contains(&"index.js".to_string()));
    assert!(out.contains(&"package.json".to_string()));
    assert!(
        !out.iter().any(|p| p.starts_with("test/")),
        "`.npmignore` must exclude `test/`; received {out:?}",
    );
}

#[test]
fn gitignore_excludes_when_no_npmignore() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "index.js");
    touch(root, "build/output.js");
    fs::write(root.join(".gitignore"), "build/\n").unwrap();

    let manifest = json!({ "name": "x", "version": "0.0.0" });
    let mut out = packlist(root, &manifest).unwrap();
    out.sort();

    assert!(out.contains(&"index.js".to_string()));
    assert!(
        !out.iter().any(|p| p.starts_with("build/")),
        "`.gitignore` must exclude `build/` when no `.npmignore` exists; received {out:?}",
    );
}

#[test]
fn npmignore_does_not_drop_always_included_files() {
    // Even when `.npmignore` says to drop README/LICENSE/etc., they
    // must still ship — npm-packlist's always-included override.
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "README.md");
    touch(root, "LICENSE");
    touch(root, "index.js");
    fs::write(root.join(".npmignore"), "README.md\nLICENSE\n").unwrap();

    let manifest = json!({ "name": "x", "version": "0.0.0" });
    let out = packlist(root, &manifest).unwrap();

    assert!(out.contains(&"README.md".to_string()), "README.md is always-included: {out:?}");
    assert!(out.contains(&"LICENSE".to_string()), "LICENSE is always-included: {out:?}");
    assert!(out.contains(&"package.json".to_string()));
    assert!(out.contains(&"index.js".to_string()));
}

#[test]
fn npmignore_in_subdir_applies_to_subtree_only() {
    // A nested `.npmignore` should narrow only its own subtree, not
    // override the root's view.
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "lib/index.js");
    touch(root, "lib/internal/private.js");
    fs::write(root.join("lib/internal/.npmignore"), "private.js\n").unwrap();

    let manifest = json!({ "name": "x", "version": "0.0.0" });
    let out = packlist(root, &manifest).unwrap();

    assert!(out.contains(&"lib/index.js".to_string()));
    assert!(
        !out.contains(&"lib/internal/private.js".to_string()),
        "nested .npmignore must exclude `private.js`: {out:?}",
    );
}

#[test]
fn bundle_dependencies_subtree_is_included() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "index.js");
    touch(root, "node_modules/dep/package.json");
    touch(root, "node_modules/dep/lib.js");
    // Sibling node_modules entry that is NOT bundled — must not ship.
    touch(root, "node_modules/other/package.json");
    touch(root, "node_modules/other/lib.js");

    let manifest = json!({
        "name": "x",
        "version": "0.0.0",
        "bundleDependencies": ["dep"],
    });
    let out = packlist(root, &manifest).unwrap();

    assert!(out.contains(&"node_modules/dep/package.json".to_string()));
    assert!(out.contains(&"node_modules/dep/lib.js".to_string()));
    assert!(
        !out.iter().any(|p| p.starts_with("node_modules/other")),
        "non-bundled `other` must not ship: {out:?}",
    );
}

#[test]
fn bundled_dependencies_legacy_spelling_works() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");
    touch(root, "node_modules/legacy-bundle/package.json");

    let manifest = json!({
        "name": "x",
        "version": "0.0.0",
        "bundledDependencies": ["legacy-bundle"],
    });
    let out = packlist(root, &manifest).unwrap();

    assert!(
        out.contains(&"node_modules/legacy-bundle/package.json".to_string()),
        "`bundledDependencies` is the legacy spelling and must be accepted: {out:?}",
    );
}

#[test]
fn bundle_dependency_missing_dir_is_silently_skipped() {
    // A bundleDependencies entry that's not actually on disk should
    // not crash the fetcher — npm-packlist treats it as a no-op.
    let dir = tempdir().unwrap();
    let root = dir.path();
    touch(root, "package.json");

    let manifest = json!({
        "name": "x",
        "version": "0.0.0",
        "bundleDependencies": ["ghost"],
    });
    let out = packlist(root, &manifest).unwrap();
    assert_eq!(out, vec!["package.json".to_string()]);
}
