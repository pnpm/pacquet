use super::*;
use pacquet_testing_utils::fs::is_symlink_or_junction;
use std::{fs, str::FromStr};
use tempfile::tempdir;

/// `symlink_direct_deps_into_node_modules` creates one symlink per dep,
/// each pointing at `<virtual_store>/<name>@<ver>/node_modules/<name>`.
/// End-to-end of the symlink loop without lockfile / npmrc plumbing.
#[test]
fn creates_one_symlink_per_dep_pointing_at_virtual_store() {
    let tmp = tempdir().unwrap();
    let modules_dir = tmp.path().join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");
    fs::create_dir_all(&modules_dir).unwrap();

    // Pre-create the virtual-store target dirs so the symlink has
    // somewhere to point. The function under test only creates the
    // symlinks; populating the targets is upstream.
    let foo_target = virtual_store_dir.join("foo@1.0.0/node_modules/foo");
    let bar_target = virtual_store_dir.join("@scope+bar@2.0.0/node_modules/@scope/bar");
    fs::create_dir_all(&foo_target).unwrap();
    fs::create_dir_all(&bar_target).unwrap();

    let deps = vec![
        (PkgName::from_str("foo").unwrap(), PkgVerPeer::from_str("1.0.0").unwrap()),
        (PkgName::from_str("@scope/bar").unwrap(), PkgVerPeer::from_str("2.0.0").unwrap()),
    ];

    symlink_direct_deps_into_node_modules(&modules_dir, &virtual_store_dir, &deps);

    let foo_link = modules_dir.join("foo");
    assert!(
        is_symlink_or_junction(&foo_link).unwrap(),
        "expected foo to be a symlink/junction at {foo_link:?}",
    );

    // Scoped packages: `<modules_dir>/<scope>/<name>` is the link path.
    // The function creates the scope directory if needed (via
    // `symlink_package`).
    let bar_link = modules_dir.join("@scope/bar");
    assert!(
        is_symlink_or_junction(&bar_link).unwrap(),
        "expected scoped @scope/bar to be a symlink/junction at {bar_link:?}",
    );
}

/// Empty deps list is a no-op — no entries are created under
/// `<modules_dir>`.
#[test]
fn empty_deps_list_is_no_op() {
    let tmp = tempdir().unwrap();
    let modules_dir = tmp.path().join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");
    fs::create_dir_all(&modules_dir).unwrap();

    symlink_direct_deps_into_node_modules(&modules_dir, &virtual_store_dir, &[]);

    let entries: Vec<_> = fs::read_dir(&modules_dir).unwrap().flatten().collect();
    assert!(entries.is_empty(), "no deps means no node_modules entries");
}
