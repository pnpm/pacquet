use assert_cmd::prelude::*;
use pacquet_package_json::{DependencyGroup, PackageJson};
use pacquet_testing_utils::{
    bin::pacquet_with_temp_cwd,
    fs::{get_all_folders, get_filenames_in_folder},
};
use pretty_assertions::assert_eq;
use std::{env, ffi::OsStr, fs};
use tempfile::TempDir;

pub fn exec_pacquet_in_temp_cwd<Args>(args: Args) -> TempDir
where
    Args: IntoIterator,
    Args::Item: AsRef<OsStr>,
{
    let (mut command, current_dir) = pacquet_with_temp_cwd();
    command.current_dir(current_dir.path()).args(args).assert().success();
    current_dir
}

#[test]
fn should_install_all_dependencies() {
    let dir = exec_pacquet_in_temp_cwd(["add", "is-even"]);

    eprintln!("Directory list");
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
    let dir = exec_pacquet_in_temp_cwd(["add", "is-odd"]);

    eprintln!("Directory list");
    insta::assert_debug_snapshot!(get_all_folders(dir.path()));

    let package_json_path = dir.path().join("package.json");

    eprintln!("Ensure the manifest file ({package_json_path:?}) exists");
    assert!(package_json_path.exists());

    let virtual_store_dir = dir.path().join("node_modules").join(".pacquet");

    eprintln!("Ensure virtual store dir ({virtual_store_dir:?}) exists");
    assert!(virtual_store_dir.exists());

    eprintln!("Make sure the symlinks are correct");
    assert_eq!(
        fs::read_link(virtual_store_dir.join("is-odd@3.0.1/node_modules/is-number")).unwrap(),
        fs::canonicalize(virtual_store_dir.join("is-number@6.0.0/node_modules/is-number")).unwrap(),
    );
}

#[test]
fn should_add_to_package_json() {
    let dir = exec_pacquet_in_temp_cwd(["add", "is-odd"]);
    let file = PackageJson::from_path(dir.path().join("package.json")).unwrap();
    assert!(file.dependencies([DependencyGroup::Default]).any(|(k, _)| k == "is-odd"));
}
