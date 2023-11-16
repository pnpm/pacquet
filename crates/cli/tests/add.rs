use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_testing_utils::{
    bin::CommandTempCwd,
    fs::{get_all_folders, get_filenames_in_folder},
};
use pretty_assertions::assert_eq;
use std::{env, ffi::OsStr, fs, path::PathBuf};
use tempfile::TempDir;

fn exec_pacquet_in_temp_cwd<Args>(args: Args) -> (TempDir, PathBuf)
where
    Args: IntoIterator,
    Args::Item: AsRef<OsStr>,
{
    let CommandTempCwd { pacquet, root, workspace, .. } =
        CommandTempCwd::init().add_default_npmrc();
    pacquet.with_args(args).assert().success();
    (root, workspace)
}

#[test]
fn should_install_all_dependencies() {
    let (root, workspace) = exec_pacquet_in_temp_cwd(["add", "is-even"]);

    eprintln!("Directory list");
    insta::assert_debug_snapshot!(get_all_folders(&workspace));

    let manifest_path = workspace.join("package.json");

    eprintln!("Ensure the manifest file ({manifest_path:?}) exists");
    assert!(manifest_path.exists());

    let virtual_store_dir = workspace.join("node_modules").join(".pnpm");

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

    drop(root); // cleanup
}

#[test]
#[cfg(unix)]
pub fn should_symlink_correctly() {
    let (root, workspace) = exec_pacquet_in_temp_cwd(["add", "is-odd"]);

    eprintln!("Directory list");
    insta::assert_debug_snapshot!(get_all_folders(&workspace));

    let manifest_path = workspace.join("package.json");

    eprintln!("Ensure the manifest file ({manifest_path:?}) exists");
    assert!(manifest_path.exists());

    let virtual_store_dir = workspace.join("node_modules").join(".pnpm");

    eprintln!("Ensure virtual store dir ({virtual_store_dir:?}) exists");
    assert!(virtual_store_dir.exists());

    eprintln!("Make sure the symlinks are correct");
    assert_eq!(
        fs::read_link(virtual_store_dir.join("is-odd@3.0.1/node_modules/is-number")).unwrap(),
        fs::canonicalize(virtual_store_dir.join("is-number@6.0.0/node_modules/is-number")).unwrap(),
    );

    drop(root); // cleanup
}

#[test]
fn should_add_to_package_json() {
    let (root, dir) = exec_pacquet_in_temp_cwd(["add", "is-odd"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    eprintln!("Ensure is-odd is added to package.json#dependencies");
    assert!(file.dependencies([DependencyGroup::Prod]).any(|(k, _)| k == "is-odd"));
    drop(root); // cleanup
}

#[test]
fn should_add_dev_dependency() {
    let (root, dir) = exec_pacquet_in_temp_cwd(["add", "is-odd", "--save-dev"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    eprintln!("Ensure is-odd is added to package.json#devDependencies");
    assert!(file.dependencies([DependencyGroup::Dev]).any(|(k, _)| k == "is-odd"));
    drop(root); // cleanup
}

#[test]
fn should_add_peer_dependency() {
    let (root, dir) = exec_pacquet_in_temp_cwd(["add", "is-odd", "--save-peer"]);
    let file = PackageManifest::from_path(dir.join("package.json")).unwrap();
    eprintln!("Ensure is-odd is added to package.json#devDependencies");
    assert!(file.dependencies([DependencyGroup::Dev]).any(|(k, _)| k == "is-odd"));
    eprintln!("Ensure is-odd is added to package.json#peerDependencies");
    assert!(file.dependencies([DependencyGroup::Peer]).any(|(k, _)| k == "is-odd"));
    drop(root); // cleanup
}
