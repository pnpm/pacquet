use super::{LinkVirtualStoreBins, LinkVirtualStoreBinsError, link_direct_dep_bins};
use pacquet_cmd_shim::is_shim_pointing_at;
use serde_json::json;
use std::{fs, path::Path};
use tempfile::tempdir;

/// End-to-end exercise of [`LinkVirtualStoreBins`] against a hand-built
/// virtual store. Slot `parent@1.0.0` has one child `child` declaring a
/// bin; after the run, the child's shim must land at
/// `parent@1.0.0/node_modules/parent/node_modules/.bin/child` and *not*
/// at the slot's own `node_modules/.bin` (which is what would happen if
/// we accidentally pointed at the wrong directory).
#[test]
fn writes_child_bins_into_slot_own_package_node_modules() {
    let tmp = tempdir().unwrap();
    let virtual_dir = tmp.path().join(".pacquet");

    // The slot for `parent@1.0.0`. pnpm uses `+` for scope separator.
    let slot = virtual_dir.join("parent@1.0.0");
    let modules = slot.join("node_modules");
    let parent_dir = modules.join("parent");
    let child_dir = modules.join("child");
    fs::create_dir_all(&parent_dir).unwrap();
    fs::create_dir_all(&child_dir).unwrap();

    fs::write(
        parent_dir.join("package.json"),
        json!({"name": "parent", "version": "1.0.0"}).to_string(),
    )
    .unwrap();
    fs::write(
        child_dir.join("package.json"),
        json!({"name": "child", "version": "1.0.0", "bin": "cli.js"}).to_string(),
    )
    .unwrap();
    fs::write(child_dir.join("cli.js"), "#!/usr/bin/env node\n").unwrap();

    LinkVirtualStoreBins { virtual_store_dir: &virtual_dir }.run().unwrap();

    let shim_path = parent_dir.join("node_modules/.bin/child");
    assert!(shim_path.exists(), "expected shim at {shim_path:?}");
    let body = fs::read_to_string(&shim_path).unwrap();
    // Layout, with shim at A and target at B, relative path `A → B`:
    //
    //   <slot>/node_modules/parent/node_modules/.bin/child   (shim, A)
    //   <slot>/node_modules/child/cli.js                     (target, B)
    //
    // Common prefix is `<slot>/node_modules`. A has three extra
    // segments after that (`parent`, `node_modules`, `.bin`); B has
    // two (`child`, `cli.js`). Relative = `../../../child/cli.js`.
    assert!(
        body.contains("\"$basedir/../../../child/cli.js\""),
        "shim must reference the sibling child via the right number of `..`s, got:\n{body}",
    );
}

/// A slot whose own package also declares a bin must NOT have that bin
/// linked into its own `node_modules/.bin`. pnpm only links *children*
/// of a slot, so a tsc slot does not redundantly produce a shim for
/// its own tsc binary.
///
/// To distinguish the exclusion logic from "the slot wasn't processed
/// at all," the slot has a real child (`other`) whose bin SHOULD be
/// linked. The assertions then check both directions:
///
/// 1. The child bin appears in `<slot>/node_modules/<own>/node_modules/.bin/`.
/// 2. The slot's own bin does NOT appear there.
///
/// If [`super::find_slot_own_package_dir`] returns `None` (slot skipped),
/// (1) fails. If the exclusion logic is dropped, (2) fails. Either
/// failure surfaces the regression.
#[test]
fn skips_slot_own_package_when_walking_children() {
    let tmp = tempdir().unwrap();
    let virtual_dir = tmp.path().join(".pacquet");

    let slot = virtual_dir.join("tsc@5.0.0");
    let modules = slot.join("node_modules");
    let pkg_dir = modules.join("tsc");
    let other_dir = modules.join("other");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::create_dir_all(&other_dir).unwrap();

    fs::write(
        pkg_dir.join("package.json"),
        json!({"name": "tsc", "version": "5.0.0", "bin": "tsc.js"}).to_string(),
    )
    .unwrap();
    fs::write(pkg_dir.join("tsc.js"), "#!/usr/bin/env node\n").unwrap();

    fs::write(
        other_dir.join("package.json"),
        json!({"name": "other", "version": "1.0.0", "bin": "other.js"}).to_string(),
    )
    .unwrap();
    fs::write(other_dir.join("other.js"), "#!/usr/bin/env node\n").unwrap();

    LinkVirtualStoreBins { virtual_store_dir: &virtual_dir }.run().unwrap();

    let bin_dir = pkg_dir.join("node_modules/.bin");
    assert!(
        bin_dir.join("other").exists(),
        "child bin `other` must be linked under the slot's own package",
    );
    assert!(!bin_dir.join("tsc").exists(), "self-bin `tsc` must not be linked into own slot",);
}

/// [`LinkVirtualStoreBins`] with a non-existent virtual-store directory
/// must be a no-op (`Ok`). A fresh install where the dir doesn't exist
/// yet must not error out.
#[test]
fn link_virtual_store_bins_no_op_when_dir_missing() {
    let tmp = tempdir().unwrap();
    let nonexistent = tmp.path().join("does-not-exist");
    LinkVirtualStoreBins { virtual_store_dir: &nonexistent }.run().expect("missing dir is Ok");
}

/// Slot whose name has a `+` (scope separator) resolves to
/// `node_modules/<scope>/<name>`. Pins [`super::find_slot_own_package_dir`]'s
/// scoped branch. The un-scoped branch is exercised by the existing
/// `writes_child_bins_into_slot_own_package_node_modules` test.
#[test]
fn link_virtual_store_bins_handles_scoped_slot_name() {
    let tmp = tempdir().unwrap();
    let virtual_dir = tmp.path().join(".pacquet");
    let slot = virtual_dir.join("@scope+parent@1.0.0");
    let modules = slot.join("node_modules");
    let parent_dir = modules.join("@scope/parent");
    let child_dir = modules.join("child");
    fs::create_dir_all(&parent_dir).unwrap();
    fs::create_dir_all(&child_dir).unwrap();

    fs::write(
        parent_dir.join("package.json"),
        json!({"name": "@scope/parent", "version": "1.0.0"}).to_string(),
    )
    .unwrap();
    fs::write(
        child_dir.join("package.json"),
        json!({"name": "child", "version": "1.0.0", "bin": "cli.js"}).to_string(),
    )
    .unwrap();
    fs::write(child_dir.join("cli.js"), "#!/usr/bin/env node\n").unwrap();

    LinkVirtualStoreBins { virtual_store_dir: &virtual_dir }.run().unwrap();

    let shim = parent_dir.join("node_modules/.bin/child");
    assert!(shim.exists(), "scoped-slot bin linking must produce a shim at {shim:?}");
}

/// Peer-resolved slots have version segments that contain additional
/// `@` characters (one per peer spec). [`super::find_slot_own_package_dir`]
/// must parse the package-name boundary from the **left**, not the
/// right, otherwise it splits inside a peer spec and silently fails
/// to locate the own package. Bins of children of the slot then
/// never get linked.
///
/// Slot name shape verified against
/// `pacquet_lockfile::pkg_name_ver_peer::tests::to_virtual_store_name`.
/// Pins review finding #5.
#[test]
fn link_virtual_store_bins_handles_peer_resolved_slot_name() {
    let tmp = tempdir().unwrap();
    let virtual_dir = tmp.path().join(".pacquet");

    // pnpm format: `ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)`.
    // Pacquet's `to_virtual_store_name`: `_` joins peers, `(` and `)`
    // are stripped, `/` becomes `+`.
    let slot = virtual_dir.join("ts-node@10.9.1_@types+node@18.7.19_typescript@5.1.6");
    let modules = slot.join("node_modules");
    let pkg_dir = modules.join("ts-node");
    let child_dir = modules.join("child");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::create_dir_all(&child_dir).unwrap();

    fs::write(
        pkg_dir.join("package.json"),
        json!({"name": "ts-node", "version": "10.9.1"}).to_string(),
    )
    .unwrap();
    fs::write(
        child_dir.join("package.json"),
        json!({"name": "child", "version": "1.0.0", "bin": "cli.js"}).to_string(),
    )
    .unwrap();
    fs::write(child_dir.join("cli.js"), "#!/usr/bin/env node\n").unwrap();

    LinkVirtualStoreBins { virtual_store_dir: &virtual_dir }.run().unwrap();

    let shim = pkg_dir.join("node_modules/.bin/child");
    assert!(
        shim.exists(),
        "peer-resolved slot's child bin must be linked under the own package's `node_modules/.bin`, \
         not silently dropped because the slot-name parser split inside a peer spec",
    );
}

/// A virtual-store slot whose `node_modules/` is missing must be skipped
/// without error.
#[test]
fn link_virtual_store_bins_skips_slot_without_node_modules() {
    let tmp = tempdir().unwrap();
    let virtual_dir = tmp.path().join(".pacquet");
    fs::create_dir_all(virtual_dir.join("incomplete@1.0.0")).unwrap();
    LinkVirtualStoreBins { virtual_store_dir: &virtual_dir }.run().unwrap();
}

/// [`link_direct_dep_bins`] walks the project's `node_modules/<dep>`
/// symlinks and writes a shim per declared bin. End-to-end exercise of
/// the path that runs after [`crate::SymlinkDirectDependencies`].
#[test]
fn link_direct_dep_bins_writes_shims_for_each_dep() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    let foo_dir = modules.join("foo");
    fs::create_dir_all(&foo_dir).unwrap();
    fs::write(foo_dir.join("package.json"), json!({"name": "foo", "bin": "cli.js"}).to_string())
        .unwrap();
    fs::write(foo_dir.join("cli.js"), "#!/usr/bin/env node\n").unwrap();

    link_direct_dep_bins(&modules, &["foo".to_string()]).unwrap();

    let shim = modules.join(".bin/foo");
    assert!(shim.exists(), "shim should be created at {shim:?}");
    let body = fs::read_to_string(&shim).unwrap();
    assert!(is_shim_pointing_at(&body, &foo_dir.join("cli.js")));
}

/// [`link_direct_dep_bins`] with no deps is a no-op. It must not even
/// create the `.bin` directory. Mirrors the early-return of
/// [`pacquet_cmd_shim::link_bins_of_packages`].
#[test]
fn link_direct_dep_bins_no_op_for_empty_dep_list() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    fs::create_dir_all(&modules).unwrap();
    link_direct_dep_bins(&modules, &[]).unwrap();
    assert!(!modules.join(".bin").exists());
}

/// [`link_direct_dep_bins`] resolves the dep name through the symlink
/// pacquet creates under `<modules_dir>/<name>`. Pin that the manifest
/// is read from the symlink's *target*, not the symlink path itself.
#[test]
fn link_direct_dep_bins_follows_symlink_to_real_package() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    fs::create_dir_all(&modules).unwrap();

    // The "real" package contents live elsewhere (mimics pacquet's
    // virtual-store layout).
    let real_pkg = tmp.path().join("virtual/foo@1.0.0/node_modules/foo");
    fs::create_dir_all(&real_pkg).unwrap();
    fs::write(real_pkg.join("package.json"), json!({"name": "foo", "bin": "cli.js"}).to_string())
        .unwrap();
    fs::write(real_pkg.join("cli.js"), "#!/usr/bin/env node\n").unwrap();

    // Use the same approach pacquet uses in production: symlink on
    // Unix, junction on Windows. Plain `std::os::windows::fs::symlink_dir`
    // requires the `SeCreateSymbolicLinkPrivilege` (off by default on
    // CI runners), so the test would fail there even though production
    // never hits that code path.
    let symlink = modules.join("foo");
    pacquet_fs::symlink_dir(&real_pkg, &symlink).unwrap();

    link_direct_dep_bins(&modules, &["foo".to_string()]).unwrap();

    assert!(modules.join(".bin/foo").exists(), "symlinked dep must produce a shim");
}

/// Skip dep names whose symlink points at a non-existent target.
/// [`link_direct_dep_bins`] filters those silently because the
/// surrounding install pipeline has already populated whatever it could.
#[test]
fn link_direct_dep_bins_skips_dep_with_missing_manifest() {
    let tmp = tempdir().unwrap();
    let modules = tmp.path().join("node_modules");
    fs::create_dir_all(&modules).unwrap();
    // No `<modules>/foo` directory at all.
    link_direct_dep_bins(&modules, &["foo".to_string()]).unwrap();
    assert!(!modules.join(".bin").exists());
}

/// [`LinkVirtualStoreBins::run_with`] propagates a non-`NotFound`
/// `read_dir` error on the virtual-store directory itself. Real fs
/// can't trigger this portably; the fake forces the
/// [`LinkVirtualStoreBinsError::ReadVirtualStore`] variant.
#[test]
fn link_virtual_store_bins_propagates_read_error_via_di() {
    use pacquet_cmd_shim::{
        FsCreateDirAll, FsReadDir, FsReadFile, FsReadHead, FsReadString, FsSetPermissions,
        FsWalkFiles, FsWrite,
    };
    use std::io;

    struct DenyVirtualStore;
    impl FsReadDir for DenyVirtualStore {
        fn read_dir(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            Err::<std::iter::Empty<std::path::PathBuf>, _>(io::Error::from(
                io::ErrorKind::PermissionDenied,
            ))
        }
    }
    impl FsReadFile for DenyVirtualStore {
        fn read_file(_: &Path) -> io::Result<Vec<u8>> {
            unreachable!()
        }
    }
    impl FsReadString for DenyVirtualStore {
        fn read_to_string(_: &Path) -> io::Result<String> {
            unreachable!()
        }
    }
    impl FsReadHead for DenyVirtualStore {
        fn read_head(_: &Path, _: u64, _: &mut [u8]) -> io::Result<usize> {
            unreachable!()
        }
    }
    impl FsCreateDirAll for DenyVirtualStore {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWrite for DenyVirtualStore {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsSetPermissions for DenyVirtualStore {
        fn set_executable(_: &Path) -> io::Result<()> {
            unreachable!()
        }
        fn ensure_executable_bits(_: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    impl FsWalkFiles for DenyVirtualStore {
        fn walk_files(_: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
            unreachable!("directories.bin not exercised by this test");
            #[expect(unreachable_code)]
            Ok(std::iter::empty())
        }
    }

    let err = LinkVirtualStoreBins { virtual_store_dir: Path::new("/anything") }
        .run_with::<DenyVirtualStore>()
        .expect_err("read_dir error must propagate");
    assert!(matches!(err, LinkVirtualStoreBinsError::ReadVirtualStore { .. }));
}
