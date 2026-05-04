use super::{LinkBinsError, PackageBinSource, link_bins, link_bins_of_packages};
use crate::{
    capabilities::{
        FsCreateDirAll, FsReadDir, FsReadFile, FsReadHead, FsReadString, FsSetPermissions,
        FsWalkFiles, FsWrite, RealApi,
    },
    shim::is_shim_pointing_at,
};
use serde_json::{Value, json};
use std::{fs, path::Path};
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
    link_bins_of_packages::<RealApi>(
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
    link_bins_of_packages::<RealApi>(
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

/// [`link_bins::<RealApi>`](link_bins) walks every package and its scoped
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
    link_bins::<RealApi>(&modules, &bins).unwrap();

    assert!(bins.join("foo").exists(), "foo shim must exist");
    assert!(bins.join("bar").exists(), "scoped @s/bar shim must use bare name `bar`");
}

/// [`link_bins`] on a missing `node_modules` directory must be a no-op
/// (Ok with empty result), not an error. Real fs returns `NotFound`
/// which the implementation already degrades.
#[test]
fn link_bins_handles_missing_modules_dir() {
    let tmp = tempdir().unwrap();
    let bins_dir = tmp.path().join(".bin");
    link_bins::<RealApi>(&tmp.path().join("missing"), &bins_dir)
        .expect("missing modules dir is Ok");
    assert!(!bins_dir.exists(), "no shims means no bin dir created");
}

/// [`link_bins_of_packages`] with no bins to link is a complete no-op —
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
    link_bins_of_packages::<RealApi>(&[PackageBinSource { location: pkg, manifest }], &bins)
        .unwrap();
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
    link_bins_of_packages::<RealApi>(
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
/// surface as a [`LinkBinsError::ParseManifest`] error, not silently skip.
#[test]
fn link_bins_propagates_parse_manifest_error() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    fs::create_dir_all(modules.join("broken")).unwrap();
    fs::write(modules.join("broken/package.json"), "{ this is not json").unwrap();

    let bins = modules.join(".bin");
    let err = link_bins::<RealApi>(&modules, &bins).expect_err("invalid manifest must surface");
    assert!(
        matches!(err, LinkBinsError::ParseManifest { .. }),
        "expected ParseManifest, got {err:?}",
    );
}

/// [`link_bins`] must idempotently short-circuit when an existing shim
/// already targets the same bin file. Pins [`is_shim_pointing_at`]'s
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
    link_bins::<RealApi>(&modules, &bins).unwrap();
    let original = fs::read_to_string(bins.join("foo")).unwrap();
    // Append a sentinel — if the second pass rewrites the shim, the
    // sentinel disappears.
    let sentinel = format!("{original}\n# SENTINEL");
    fs::write(bins.join("foo"), &sentinel).unwrap();

    link_bins::<RealApi>(&modules, &bins).unwrap();
    assert_eq!(fs::read_to_string(bins.join("foo")).unwrap(), sentinel);
}

/// [`link_bins`] must NOT skip when only the canonical `.sh` shim exists
/// — the `.cmd` and `.ps1` siblings could be missing because an older
/// pacquet wrote `.sh`-only or a partial-write crash interrupted the
/// writer mid-batch. Gating on the `.sh` marker alone (an earlier
/// version of [`super::write_shim`]) caused those upgrade paths to leave
/// the missing siblings permanently absent.
#[test]
fn link_bins_rewrites_when_only_sh_flavor_exists() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    fs::create_dir_all(modules.join("foo")).unwrap();
    fs::write(modules.join("foo/package.json"), json!({"name": "foo", "bin": "f.js"}).to_string())
        .unwrap();
    fs::write(modules.join("foo/f.js"), "#!/usr/bin/env node\n").unwrap();

    let bins = modules.join(".bin");
    link_bins::<RealApi>(&modules, &bins).unwrap();

    // Simulate the partial-write / older-pacquet state: delete the
    // .cmd and .ps1 siblings, leaving only the `.sh` shim with its
    // (still correct) target marker.
    fs::remove_file(bins.join("foo.cmd")).unwrap();
    fs::remove_file(bins.join("foo.ps1")).unwrap();

    link_bins::<RealApi>(&modules, &bins).unwrap();

    assert!(bins.join("foo").exists(), ".sh shim must remain");
    assert!(bins.join("foo.cmd").exists(), ".cmd sibling must be re-created on second pass");
    assert!(bins.join("foo.ps1").exists(), ".ps1 sibling must be re-created on second pass");
}

/// [`link_bins_of_packages`] propagates a non-`NotFound` `read_dir`
/// error from the calling context. Use a fake `Api` that fails the
/// initial `create_dir_all` to cover the [`LinkBinsError::CreateBinDir`]
/// error variant that real fs can't trigger portably.
#[test]
fn link_bins_propagates_create_bin_dir_error_via_di() {
    use std::io;
    struct FailingCreateDir;
    impl FsReadDir for FailingCreateDir {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Ok(std::iter::empty())
        }
    }
    impl FsReadFile for FailingCreateDir {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            unreachable!("not called when chosen is empty")
        }
    }
    impl FsReadString for FailingCreateDir {
        fn read_to_string(_: &Path) -> io::Result<String> {
            unreachable!()
        }
    }
    impl FsReadHead for FailingCreateDir {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            unreachable!()
        }
    }
    impl FsCreateDirAll for FailingCreateDir {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    }
    impl FsWrite for FailingCreateDir {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsSetPermissions for FailingCreateDir {
        fn set_executable(_: &Path) -> io::Result<()> {
            unreachable!()
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWalkFiles for FailingCreateDir {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    // A package with a bin so `chosen` is non-empty.
    let manifest = serde_json::json!({"name": "foo", "bin": "cli.js"});
    let tmp = tempdir().unwrap();
    let pkg = tmp.path().join("foo");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("cli.js"), "#!/usr/bin/env node\n").unwrap();
    let err = link_bins_of_packages::<FailingCreateDir>(
        &[PackageBinSource { location: pkg, manifest }],
        Path::new("/anything"),
    )
    .expect_err("create_dir_all error must propagate");
    assert!(matches!(err, LinkBinsError::CreateBinDir { .. }));
}

/// [`link_bins_of_packages`] propagates a write failure for the `.sh`
/// shim. Inject a fake [`FsWrite`] that always fails.
#[test]
fn link_bins_propagates_write_shim_error_via_di() {
    use std::io;
    struct FailingWrite;
    impl FsReadDir for FailingWrite {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Ok(std::iter::empty())
        }
    }
    impl FsReadFile for FailingWrite {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            unreachable!()
        }
    }
    impl FsReadString for FailingWrite {
        fn read_to_string(_: &Path) -> io::Result<String> {
            // Pretend no existing shim — forces the writer path.
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }
    impl FsReadHead for FailingWrite {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            // Empty content → no shebang, fall through to extension.
            Ok(0)
        }
    }
    impl FsCreateDirAll for FailingWrite {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsWrite for FailingWrite {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    }
    impl FsSetPermissions for FailingWrite {
        fn set_executable(_: &Path) -> io::Result<()> {
            unreachable!()
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWalkFiles for FailingWrite {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    let manifest = serde_json::json!({"name": "foo", "bin": "cli.js"});
    let tmp = tempdir().unwrap();
    let pkg = tmp.path().join("foo");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("cli.js"), "").unwrap();
    let err = link_bins_of_packages::<FailingWrite>(
        &[PackageBinSource { location: pkg, manifest }],
        &tmp.path().join(".bin"),
    )
    .expect_err("write error must propagate");
    assert!(matches!(err, LinkBinsError::WriteShim { .. }));
}

/// [`link_bins_of_packages`] propagates a chmod failure on the shim.
#[test]
fn link_bins_propagates_chmod_error_via_di() {
    use std::io;
    struct FailingChmod;
    impl FsReadDir for FailingChmod {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Ok(std::iter::empty())
        }
    }
    impl FsReadFile for FailingChmod {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            unreachable!()
        }
    }
    impl FsReadString for FailingChmod {
        fn read_to_string(_: &Path) -> io::Result<String> {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }
    impl FsReadHead for FailingChmod {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    impl FsCreateDirAll for FailingChmod {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsWrite for FailingChmod {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsSetPermissions for FailingChmod {
        fn set_executable(_: &Path) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWalkFiles for FailingChmod {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    let manifest = serde_json::json!({"name": "foo", "bin": "cli.js"});
    let tmp = tempdir().unwrap();
    let pkg = tmp.path().join("foo");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("cli.js"), "").unwrap();
    let err = link_bins_of_packages::<FailingChmod>(
        &[PackageBinSource { location: pkg, manifest }],
        &tmp.path().join(".bin"),
    )
    .expect_err("chmod error must propagate");
    assert!(matches!(err, LinkBinsError::Chmod { .. }));
}

/// [`super::write_shim`] propagates a non-`NotFound` IO error from
/// [`FsSetPermissions::ensure_executable_bits`] (chmod on the *target*
/// binary, not the shim). `NotFound` is swallowed by design — the
/// target may have been removed concurrently — but `PermissionDenied`
/// and friends must surface as [`LinkBinsError::Chmod`]. Pins the
/// guard added in this PR (review finding #4).
#[test]
fn link_bins_propagates_target_chmod_error_via_di() {
    use std::io;
    struct FailingTargetChmod;
    impl FsReadDir for FailingTargetChmod {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Ok(std::iter::empty())
        }
    }
    impl FsReadFile for FailingTargetChmod {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            unreachable!()
        }
    }
    impl FsReadString for FailingTargetChmod {
        fn read_to_string(_: &Path) -> io::Result<String> {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }
    impl FsReadHead for FailingTargetChmod {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    impl FsCreateDirAll for FailingTargetChmod {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsWrite for FailingTargetChmod {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsSetPermissions for FailingTargetChmod {
        fn set_executable(_: &Path) -> io::Result<()> {
            Ok(())
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            // The target chmod returns a non-`NotFound` error; the
            // implementation must surface it rather than silently
            // dropping it.
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    }
    impl FsWalkFiles for FailingTargetChmod {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    let manifest = serde_json::json!({"name": "foo", "bin": "cli.js"});
    let tmp = tempdir().unwrap();
    let pkg = tmp.path().join("foo");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("cli.js"), "").unwrap();
    let err = link_bins_of_packages::<FailingTargetChmod>(
        &[PackageBinSource { location: pkg, manifest }],
        &tmp.path().join(".bin"),
    )
    .expect_err("non-NotFound target chmod error must propagate as Chmod");
    assert!(matches!(err, LinkBinsError::Chmod { .. }));
}

/// [`super::write_shim`] swallows `NotFound` from
/// [`FsSetPermissions::ensure_executable_bits`] because the target may
/// legitimately be missing (concurrent removal, race with another
/// install). Pins this distinction so a future regression that
/// propagates `NotFound` here would fail the test.
#[test]
fn link_bins_swallows_target_chmod_not_found_via_di() {
    use std::io;
    struct NotFoundTargetChmod;
    impl FsReadDir for NotFoundTargetChmod {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Ok(std::iter::empty())
        }
    }
    impl FsReadFile for NotFoundTargetChmod {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            unreachable!()
        }
    }
    impl FsReadString for NotFoundTargetChmod {
        fn read_to_string(_: &Path) -> io::Result<String> {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }
    impl FsReadHead for NotFoundTargetChmod {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    impl FsCreateDirAll for NotFoundTargetChmod {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsWrite for NotFoundTargetChmod {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsSetPermissions for NotFoundTargetChmod {
        fn set_executable(_: &Path) -> io::Result<()> {
            Ok(())
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }
    impl FsWalkFiles for NotFoundTargetChmod {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    let manifest = serde_json::json!({"name": "foo", "bin": "cli.js"});
    let tmp = tempdir().unwrap();
    let pkg = tmp.path().join("foo");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("cli.js"), "").unwrap();
    link_bins_of_packages::<NotFoundTargetChmod>(
        &[PackageBinSource { location: pkg, manifest }],
        &tmp.path().join(".bin"),
    )
    .expect("NotFound on target chmod must be swallowed silently");
}

/// [`link_bins_of_packages`] propagates a non-`NotFound` IO error from
/// [`search_script_runtime`] (the [`LinkBinsError::ProbeShimSource`]
/// variant). Forced via a fake [`FsReadHead`] that fails with
/// permission-denied — the wider [`super::write_shim`] →
/// [`search_script_runtime`] chain remains unchanged.
#[test]
fn link_bins_propagates_probe_shim_source_error_via_di() {
    use std::io;
    struct FailingProbe;
    impl FsReadDir for FailingProbe {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Ok(std::iter::empty())
        }
    }
    impl FsReadFile for FailingProbe {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            unreachable!()
        }
    }
    impl FsReadString for FailingProbe {
        fn read_to_string(_: &Path) -> io::Result<String> {
            unreachable!()
        }
    }
    impl FsReadHead for FailingProbe {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    }
    impl FsCreateDirAll for FailingProbe {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsWrite for FailingProbe {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsSetPermissions for FailingProbe {
        fn set_executable(_: &Path) -> io::Result<()> {
            unreachable!()
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWalkFiles for FailingProbe {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    let manifest = serde_json::json!({"name": "foo", "bin": "cli.js"});
    let tmp = tempdir().unwrap();
    let pkg = tmp.path().join("foo");
    fs::create_dir_all(&pkg).unwrap();
    let err = link_bins_of_packages::<FailingProbe>(
        &[PackageBinSource { location: pkg, manifest }],
        &tmp.path().join(".bin"),
    )
    .expect_err("probe error must propagate");
    assert!(matches!(err, LinkBinsError::ProbeShimSource { .. }));
}

/// [`link_bins`] propagates a non-`NotFound` IO error from reading a
/// child `package.json` (the [`LinkBinsError::ReadManifest`] variant).
/// Forced via a fake [`FsReadFile`] that always returns
/// `PermissionDenied`.
#[test]
fn link_bins_propagates_read_manifest_error_via_di() {
    use std::io;
    struct DenyManifestRead;
    impl FsReadDir for DenyManifestRead {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Ok(vec!["foo".into()].into_iter())
        }
    }
    impl FsReadFile for DenyManifestRead {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    }
    impl FsReadString for DenyManifestRead {
        fn read_to_string(_: &Path) -> io::Result<String> {
            unreachable!()
        }
    }
    impl FsReadHead for DenyManifestRead {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            unreachable!()
        }
    }
    impl FsCreateDirAll for DenyManifestRead {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWrite for DenyManifestRead {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsSetPermissions for DenyManifestRead {
        fn set_executable(_: &Path) -> io::Result<()> {
            unreachable!()
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWalkFiles for DenyManifestRead {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    let err = link_bins::<DenyManifestRead>(Path::new("/x"), Path::new("/x/.bin"))
        .expect_err("read_manifest error must propagate");
    assert!(matches!(err, LinkBinsError::ReadManifest { .. }));
}

/// [`super::pick_winner`] `(true, false)` arm — existing owns, candidate
/// doesn't → existing wins. The other arm (`(false, true)`) is
/// covered by `ownership_breaks_bin_conflicts` further down.
///
/// Uses `aaa-other` (lexically less than `npm`) as the non-owner so
/// the test fails when ownership is broken: with the rule disabled
/// the lexical fallback picks `aaa-other`, the assertion observes
/// `/aaa-other/npx` instead of `/npm/npx`. A package named `other`
/// would lexically lose to `npm` regardless, masking the regression.
#[test]
fn ownership_breaks_bin_conflicts_when_existing_owns() {
    let tmp = tempdir().unwrap();
    let aaa_other = tmp.path().join("aaa-other");
    let npm = tmp.path().join("npm");
    for d in [&aaa_other, &npm] {
        fs::create_dir_all(d).unwrap();
        fs::write(d.join("npx"), "#!/usr/bin/env node\n").unwrap();
    }
    fs::write(npm.join("package.json"), json!({"name": "npm", "bin": {"npx": "npx"}}).to_string())
        .unwrap();
    fs::write(
        aaa_other.join("package.json"),
        json!({"name": "aaa-other", "bin": {"npx": "npx"}}).to_string(),
    )
    .unwrap();

    let manifest_other: Value =
        serde_json::from_slice(&fs::read(aaa_other.join("package.json")).unwrap()).unwrap();
    let manifest_npm: Value =
        serde_json::from_slice(&fs::read(npm.join("package.json")).unwrap()).unwrap();

    // Order npm-first; this exercises the (true, false) arm because
    // `npm` (existing) owns and `aaa-other` (candidate) doesn't.
    let bins = tmp.path().join(".bin");
    link_bins_of_packages::<RealApi>(
        &[
            PackageBinSource { location: npm.clone(), manifest: manifest_npm },
            PackageBinSource { location: aaa_other.clone(), manifest: manifest_other },
        ],
        &bins,
    )
    .unwrap();

    let body = fs::read_to_string(bins.join("npx")).unwrap();
    assert!(body.contains("/npm/npx"), "existing-owns winner must be `npm`, body:\n{body}");
}

/// [`link_bins`] propagates a non-`NotFound` `read_dir` error on
/// `<modules_dir>` itself. Real fs can't trigger this portably; the
/// fake forces the variant.
#[test]
fn link_bins_propagates_modules_dir_read_error_via_di() {
    use std::io;
    struct FailingModulesRead;
    impl FsReadDir for FailingModulesRead {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Err::<std::iter::Empty<std::path::PathBuf>, _>(io::Error::from(
                io::ErrorKind::PermissionDenied,
            ))
        }
    }
    impl FsReadFile for FailingModulesRead {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            unreachable!()
        }
    }
    impl FsReadString for FailingModulesRead {
        fn read_to_string(_: &Path) -> io::Result<String> {
            unreachable!()
        }
    }
    impl FsReadHead for FailingModulesRead {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            unreachable!()
        }
    }
    impl FsCreateDirAll for FailingModulesRead {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWrite for FailingModulesRead {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsSetPermissions for FailingModulesRead {
        fn set_executable(_: &Path) -> io::Result<()> {
            unreachable!()
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWalkFiles for FailingModulesRead {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    let err = link_bins::<FailingModulesRead>(Path::new("/x"), Path::new("/x/.bin"))
        .expect_err("read_dir error must propagate");
    assert!(matches!(err, LinkBinsError::CreateBinDir { .. }));
}

/// Conflict resolution: when two packages declare the same bin name, the
/// owning package wins.
///
/// Uses `aaa-other` (lexically less than `npm`) as the non-owner so the
/// test fails when ownership is broken: with the rule disabled the
/// lexical fallback picks `aaa-other`, the assertion observes
/// `/aaa-other/npx` instead of `/npm/npx`. A package named `other`
/// would lexically lose to `npm` regardless, masking the regression.
#[test]
fn ownership_breaks_bin_conflicts() {
    let tmp = tempdir().unwrap();
    let npm = tmp.path().join("npm");
    let aaa_other = tmp.path().join("aaa-other");
    for d in [&npm, &aaa_other] {
        fs::create_dir_all(d).unwrap();
        fs::write(d.join("npx"), "#!/usr/bin/env node\n").unwrap();
    }
    fs::write(npm.join("package.json"), json!({"name": "npm", "bin": {"npx": "npx"}}).to_string())
        .unwrap();
    fs::write(
        aaa_other.join("package.json"),
        json!({"name": "aaa-other", "bin": {"npx": "npx"}}).to_string(),
    )
    .unwrap();

    let manifest_npm: Value =
        serde_json::from_slice(&fs::read(npm.join("package.json")).unwrap()).unwrap();
    let manifest_other: Value =
        serde_json::from_slice(&fs::read(aaa_other.join("package.json")).unwrap()).unwrap();

    let bins = tmp.path().join(".bin");
    link_bins_of_packages::<RealApi>(
        &[
            PackageBinSource { location: aaa_other.clone(), manifest: manifest_other },
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
