use super::*;
use serde_json::json;
use tempfile::tempdir;

/// End-to-end exercise: a package with a `bin` field has a shim written
/// into the bins dir, the shim references the correct relative path,
/// and (on Unix) both the shim and the target are executable.
#[test]
fn writes_shim_for_bin_string() {
    let tmp = tempdir().unwrap();
    let pkg_dir = tmp.path().join("node_modules/foo");
    fs::create_dir_all(pkg_dir.join("bin")).unwrap();
    fs::write(
        pkg_dir.join("package.json"),
        json!({"name": "foo", "version": "1.0.0", "bin": "bin/cli.js"}).to_string(),
    )
    .unwrap();
    fs::write(pkg_dir.join("bin/cli.js"), "#!/usr/bin/env node\n").unwrap();

    let bins_dir = tmp.path().join("node_modules/.bin");
    let manifest_value: Value =
        serde_json::from_slice(&fs::read(pkg_dir.join("package.json")).unwrap()).unwrap();
    link_bins_of_packages(
        &[PackageBinSource { location: pkg_dir.clone(), manifest: manifest_value }],
        &bins_dir,
    )
    .unwrap();

    let shim_path = bins_dir.join("foo");
    assert!(shim_path.exists(), "shim should be created");

    let body = fs::read_to_string(&shim_path).unwrap();
    assert!(body.contains("\"$basedir/../foo/bin/cli.js\""), "shim body: {body}");
    assert!(is_shim_pointing_at(&body, &pkg_dir.join("bin/cli.js")));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&shim_path).unwrap().permissions().mode() & 0o777,
            0o755,
            "shim must be 0o755",
        );
        assert!(
            fs::metadata(pkg_dir.join("bin/cli.js")).unwrap().permissions().mode() & 0o111 != 0,
            "target must have at least one executable bit",
        );
    }
}

/// `link_bins(modulesDir, binsDir)` walks every package and its scoped
/// children. Both regular and `@scope/...` packages must contribute their
/// bins.
#[test]
fn link_bins_walks_modules_and_scopes() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    // Regular package
    fs::create_dir_all(modules.join("foo")).unwrap();
    fs::write(modules.join("foo/package.json"), json!({"name": "foo", "bin": "f.js"}).to_string())
        .unwrap();
    fs::write(modules.join("foo/f.js"), "#!/usr/bin/env node\n").unwrap();
    // Scoped package
    fs::create_dir_all(modules.join("@s/bar")).unwrap();
    fs::write(
        modules.join("@s/bar/package.json"),
        json!({"name": "@s/bar", "bin": "b.js"}).to_string(),
    )
    .unwrap();
    fs::write(modules.join("@s/bar/b.js"), "#!/usr/bin/env node\n").unwrap();
    // Non-package directory (no package.json) — must be ignored, not error.
    fs::create_dir_all(modules.join("not-a-package")).unwrap();

    let bins = modules.join(".bin");
    link_bins(&modules, &bins).unwrap();

    assert!(bins.join("foo").exists(), "foo shim must exist");
    assert!(bins.join("bar").exists(), "scoped @s/bar shim must use bare name `bar`");
}

/// Conflict resolution: when two packages declare the same bin name, the
/// owning package wins.
#[test]
fn ownership_breaks_bin_conflicts() {
    let tmp = tempdir().unwrap();
    let npm = tmp.path().join("npm");
    let other = tmp.path().join("other");
    for d in [&npm, &other] {
        fs::create_dir_all(d).unwrap();
        fs::write(d.join("npx"), "#!/usr/bin/env node\n").unwrap();
    }
    fs::write(npm.join("package.json"), json!({"name": "npm", "bin": {"npx": "npx"}}).to_string())
        .unwrap();
    fs::write(
        other.join("package.json"),
        json!({"name": "other", "bin": {"npx": "npx"}}).to_string(),
    )
    .unwrap();

    let manifest_npm: Value =
        serde_json::from_slice(&fs::read(npm.join("package.json")).unwrap()).unwrap();
    let manifest_other: Value =
        serde_json::from_slice(&fs::read(other.join("package.json")).unwrap()).unwrap();

    let bins = tmp.path().join(".bin");
    link_bins_of_packages(
        &[
            PackageBinSource { location: other.clone(), manifest: manifest_other },
            PackageBinSource { location: npm.clone(), manifest: manifest_npm },
        ],
        &bins,
    )
    .unwrap();

    let body = fs::read_to_string(bins.join("npx")).unwrap();
    // npm's `npx` lives at `<npm>/npx`; the shim must reference that path.
    assert!(
        body.contains("/npm/npx") || is_shim_pointing_at(&body, &npm.join("npx")),
        "ownership-aware resolution should pick npm's npx, body:\n{body}",
    );
}
