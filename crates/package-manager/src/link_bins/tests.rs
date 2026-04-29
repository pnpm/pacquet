use super::*;
use serde_json::json;
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
#[test]
fn skips_slot_own_package_when_walking_children() {
    let tmp = tempdir().unwrap();
    let virtual_dir = tmp.path().join(".pacquet");

    let slot = virtual_dir.join("tsc@5.0.0");
    let modules = slot.join("node_modules");
    let pkg_dir = modules.join("tsc");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("package.json"),
        json!({"name": "tsc", "version": "5.0.0", "bin": "tsc.js"}).to_string(),
    )
    .unwrap();
    fs::write(pkg_dir.join("tsc.js"), "#!/usr/bin/env node\n").unwrap();

    LinkVirtualStoreBins { virtual_store_dir: &virtual_dir }.run().unwrap();

    let bin_dir = pkg_dir.join("node_modules/.bin");
    // No children → bin dir should not exist at all (`link_bins_of_packages`
    // is a no-op when the package set is empty).
    assert!(!bin_dir.exists(), "self-bin must not be linked into own slot");
}
