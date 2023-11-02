#![cfg(unix)] // running this on windows result in 'program not found'
pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{bin::pacquet_and_pnpm_with_temp_cwd, fs::get_all_files};
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use std::fs;

#[test]
#[ignore = "requires metadata cache feature which pacquet doesn't yet have"]
fn store_usable_by_pnpm_offline() {
    let (pacquet, pnpm, root, workspace) = pacquet_and_pnpm_with_temp_cwd(true);

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "is-odd": "3.0.1",
        },
        "devDependencies": {
            "pretty-exec": "0.3.10",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Using pacquet to populate the store...");
    pacquet.with_arg("install").assert().success();
    fs::remove_dir_all(workspace.join("node_modules")).expect("delete node_modules");

    eprintln!("pnpm install --offline --ignore-scripts");
    pnpm.with_args(["install", "--offline", "--ignore-scripts"]).assert().success();

    drop(root); // cleanup
}

#[test]
fn same_file_structure() {
    let (pacquet, pnpm, root, workspace) = pacquet_and_pnpm_with_temp_cwd(true);

    let store_dir = root.path().join("pacquet-store");
    let modules_dir = workspace.join("node_modules");
    let cleanup = || {
        eprintln!("Cleaning up...");
        fs::remove_dir_all(&store_dir).expect("delete store dir");
        fs::remove_dir_all(&modules_dir).expect("delete node_modules");
    };

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "is-odd": "3.0.1",
        },
        "devDependencies": {
            "pretty-exec": "0.3.10",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Installing with pacquet...");
    pacquet.with_arg("install").assert().success();
    let pacquet_store_files = get_all_files(&store_dir);

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_store_files = get_all_files(&store_dir);

    cleanup();

    eprintln!("Produce the same store dir structure");
    assert_eq!(&pacquet_store_files, &pnpm_store_files);

    drop(root); // cleanup
}

#[test]
fn same_index_file_contents() {
    let (pacquet, pnpm, root, workspace) = pacquet_and_pnpm_with_temp_cwd(true);

    let store_dir = root.path().join("pacquet-store");
    let modules_dir = workspace.join("node_modules");
    let cleanup = || {
        eprintln!("Cleaning up...");
        fs::remove_dir_all(&store_dir).expect("delete store dir");
        fs::remove_dir_all(&modules_dir).expect("delete node_modules");
    };

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "is-odd": "3.0.1",
        },
        "devDependencies": {
            "fast-decode-uri-component": "1.0.1",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Installing with pacquet...");
    pacquet.with_arg("install").assert().success();
    let pacquet_index_file_contents = store_dir
        .pipe_as_ref(index_file_contents)
        .pipe(serde_json::to_value)
        .expect("serialize pacquet index file contents");

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_index_file_contents = store_dir
        .pipe_as_ref(index_file_contents)
        .pipe(serde_json::to_value)
        .expect("serialize pnpm index file contents");

    cleanup();

    eprintln!("Produce the same store dir structure");
    assert_eq!(&pacquet_index_file_contents, &pnpm_index_file_contents);

    drop(root); // cleanup
}
