use super::*;
use serde_json::json;
use tempfile::tempdir;

/// All three shim flavors (`.sh` / no-extension, `.cmd`, `.ps1`) must
/// be written for every linked bin so a project installed on Linux
/// remains usable on Windows after a `git clone`. Mirrors pnpm's
/// always-write-all-flavors behavior.
#[test]
fn writes_all_three_shim_flavors_per_bin() {
    let tmp = tempdir().unwrap();
    let pkg_dir = tmp.path().join("node_modules/foo");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("package.json"),
        json!({"name": "foo", "version": "1.0.0", "bin": "cli.js"}).to_string(),
    )
    .unwrap();
    fs::write(pkg_dir.join("cli.js"), "#!/usr/bin/env node\n").unwrap();

    let bins_dir = tmp.path().join("node_modules/.bin");
    let manifest_value: Value =
        serde_json::from_slice(&fs::read(pkg_dir.join("package.json")).unwrap()).unwrap();
    link_bins_of_packages(
        &[PackageBinSource { location: pkg_dir.clone(), manifest: manifest_value }],
        &bins_dir,
    )
    .unwrap();

    let sh = bins_dir.join("foo");
    let cmd = bins_dir.join("foo.cmd");
    let ps1 = bins_dir.join("foo.ps1");
    assert!(sh.exists(), "missing .sh shim");
    assert!(cmd.exists(), "missing .cmd shim");
    assert!(ps1.exists(), "missing .ps1 shim");

    let cmd_body = fs::read_to_string(&cmd).unwrap();
    assert!(cmd_body.starts_with("@SETLOCAL\r\n"), "cmd shim must use CRLF SETLOCAL");
    assert!(cmd_body.contains("\"%~dp0\\..\\foo\\cli.js\""), "cmd target should be windows-style");

    let ps1_body = fs::read_to_string(&ps1).unwrap();
    assert!(ps1_body.starts_with("#!/usr/bin/env pwsh\n"));
    assert!(ps1_body.contains("\"$basedir/../foo/cli.js\""));
}

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

/// `link_bins` on a missing `node_modules` directory must be a no-op
/// (Ok with empty result), not an error. Real fs returns `NotFound`
/// which the implementation already degrades.
#[test]
fn link_bins_handles_missing_modules_dir() {
    let tmp = tempdir().unwrap();
    let bins_dir = tmp.path().join(".bin");
    link_bins(&tmp.path().join("missing"), &bins_dir).expect("missing modules dir is Ok");
    assert!(!bins_dir.exists(), "no shims means no bin dir created");
}

/// `link_bins_of_packages` with no bins to link is a complete no-op —
/// it must not even create the bins directory. The empty-`chosen`
/// short-circuit guards a slot whose children have no bin field.
#[test]
fn link_bins_of_packages_no_op_when_no_bins() {
    let tmp = tempdir().unwrap();
    let pkg = tmp.path().join("pkg");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("package.json"), json!({"name": "pkg"}).to_string()).unwrap();
    let bins = tmp.path().join(".bin");
    let manifest: Value =
        serde_json::from_slice(&fs::read(pkg.join("package.json")).unwrap()).unwrap();
    link_bins_of_packages(&[PackageBinSource { location: pkg, manifest }], &bins).unwrap();
    assert!(!bins.exists(), "bins dir must not be created when nothing to link");
}

/// Same-name bin from two non-owner packages: lexical-compare picks the
/// alphabetically smaller package name. Pins the
/// `resolveCommandConflicts` fallback shape.
#[test]
fn lexical_compare_breaks_tie_when_neither_owns() {
    let tmp = tempdir().unwrap();
    let alpha = tmp.path().join("alpha");
    let beta = tmp.path().join("beta");
    for d in [&alpha, &beta] {
        fs::create_dir_all(d).unwrap();
        fs::write(d.join("cmd.js"), "#!/usr/bin/env node\n").unwrap();
    }
    fs::write(
        alpha.join("package.json"),
        json!({"name": "alpha", "bin": {"shared": "cmd.js"}}).to_string(),
    )
    .unwrap();
    fs::write(
        beta.join("package.json"),
        json!({"name": "beta", "bin": {"shared": "cmd.js"}}).to_string(),
    )
    .unwrap();

    let manifest_alpha: Value =
        serde_json::from_slice(&fs::read(alpha.join("package.json")).unwrap()).unwrap();
    let manifest_beta: Value =
        serde_json::from_slice(&fs::read(beta.join("package.json")).unwrap()).unwrap();

    let bins = tmp.path().join(".bin");
    // Order beta-then-alpha to verify the choice doesn't depend on
    // discovery order.
    link_bins_of_packages(
        &[
            PackageBinSource { location: beta.clone(), manifest: manifest_beta },
            PackageBinSource { location: alpha.clone(), manifest: manifest_alpha },
        ],
        &bins,
    )
    .unwrap();

    let body = fs::read_to_string(bins.join("shared")).unwrap();
    assert!(
        body.contains("/alpha/cmd.js"),
        "lexically smaller package name `alpha` must win, got body:\n{body}",
    );
}

/// A malformed `package.json` (invalid JSON) under `<modules_dir>` must
/// surface as a `ParseManifest` error, not silently skip.
#[test]
fn link_bins_propagates_parse_manifest_error() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    fs::create_dir_all(modules.join("broken")).unwrap();
    fs::write(modules.join("broken/package.json"), "{ this is not json").unwrap();

    let bins = modules.join(".bin");
    let err = link_bins(&modules, &bins).expect_err("invalid manifest must surface");
    assert!(
        matches!(err, LinkBinsError::ParseManifest { .. }),
        "expected ParseManifest, got {err:?}",
    );
}

/// `link_bins` must idempotently short-circuit when an existing shim
/// already targets the same bin file. Pins `is_shim_pointing_at`'s
/// integration with the writer. Mirrors pnpm's
/// "linkBins() skips bins that already reference the correct target":
/// <https://github.com/pnpm/pnpm/blob/4750fd370c/bins/linker/test/index.ts#L79-L99>.
#[test]
fn link_bins_skips_existing_shim_with_matching_marker() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    fs::create_dir_all(modules.join("foo")).unwrap();
    fs::write(modules.join("foo/package.json"), json!({"name": "foo", "bin": "f.js"}).to_string())
        .unwrap();
    fs::write(modules.join("foo/f.js"), "#!/usr/bin/env node\n").unwrap();

    let bins = modules.join(".bin");
    link_bins(&modules, &bins).unwrap();
    let original = fs::read_to_string(bins.join("foo")).unwrap();
    // Append a sentinel — if the second pass rewrites the shim, the
    // sentinel disappears.
    let sentinel = format!("{original}\n# SENTINEL");
    fs::write(bins.join("foo"), &sentinel).unwrap();

    link_bins(&modules, &bins).unwrap();
    assert_eq!(fs::read_to_string(bins.join("foo")).unwrap(), sentinel);
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
