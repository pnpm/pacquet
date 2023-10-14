use assert_cmd::prelude::*;
use pacquet_testing_utils::fs::{get_all_folders, get_filenames_in_folder};
use pretty_assertions::assert_eq;
use std::{env, fs, process::Command};
use tempfile::tempdir;

#[test]
fn should_install_all_dependencies() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("pacquet")
        .expect("find pacquet binary")
        .current_dir(dir.path())
        .arg("add")
        .arg("is-even")
        .assert()
        .success();

    eprintln!("Snapshot");
    insta::assert_debug_snapshot!(get_all_folders(dir.path()));

    let package_json_path = dir.path().join("package.json");

    eprintln!("Ensure the manifest file ({package_json_path:?}) exists");
    assert!(package_json_path.exists());

    let virtual_store_dir = dir.path().join("node_modules").join(".pacquet");

    eprintln!("Ensure virtual store dir ({virtual_store_dir:?}) exists");
    assert!(virtual_store_dir.exists());

    eprintln!("Ensure that is-buffer does not have any dependencies");
    let is_buffer_path = virtual_store_dir.join("is-buffer@1.1.6/node_modules");
    assert_eq!(get_filenames_in_folder(&is_buffer_path), vec!["is-buffer"]);

    eprintln!("Ensure that is-even have correct dependencies");
    let is_even_path = virtual_store_dir.join("is-even@1.0.0/node_modules");
    assert_eq!(get_filenames_in_folder(&is_even_path), vec!["is-even", "is-odd"]);

    eprintln!("Ensure that is-number does not have any dependencies");
    let is_number_path = virtual_store_dir.join("is-number@3.0.0/node_modules");
    assert_eq!(get_filenames_in_folder(&is_number_path), vec!["is-number", "kind-of"]);
}

#[test]
#[cfg(unix)]
pub fn should_symlink_correctly() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("pacquet")
        .expect("find pacquet binary")
        .current_dir(dir.path())
        .arg("add")
        .arg("is-odd")
        .assert()
        .success();

    eprintln!("Snapshot");
    insta::assert_debug_snapshot!(get_all_folders(dir.path()));

    let package_json_path = dir.path().join("package.json");

    eprintln!("Ensure the manifest file ({package_json_path:?}) exists");
    assert!(package_json_path.exists());

    let virtual_store_dir = dir.path().join("node_modules").join(".pacquet");

    eprintln!("Ensure virtual store dir ({virtual_store_dir:?}) exists");
    assert!(virtual_store_dir.exists());

    // Make sure the symlinks are correct
    assert_eq!(
        fs::read_link(virtual_store_dir.join("is-odd@3.0.1/node_modules/is-number")).unwrap(),
        fs::canonicalize(virtual_store_dir.join("is-number@6.0.0/node_modules/is-number")).unwrap(),
    );
}
