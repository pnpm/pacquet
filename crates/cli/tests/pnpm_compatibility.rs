#![cfg(unix)] // running this on windows result in 'program not found'
pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::{AddMockedRegistry, CommandTempCwd},
    fs::get_all_files,
};
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use std::fs;

#[test]
#[ignore = "requires metadata cache feature which pacquet doesn't yet have"]
fn store_usable_by_pnpm_offline() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Using pacquet to populate the store...");
    pacquet.with_arg("install").assert().success();
    fs::remove_dir_all(workspace.join("node_modules")).expect("delete node_modules");

    eprintln!("pnpm install --offline --ignore-scripts");
    pnpm.with_args(["install", "--offline", "--ignore-scripts"]).assert().success();

    drop((root, mock_instance)); // cleanup
}

// Ignored until pacquet adopts the pnpm v11 store format (SQLite + `store/v11/`
// directory layout). See https://github.com/pnpm/pacquet/issues/244. Until then
// pacquet writes `store/v3/files/XX/…-index.json` and pnpm 11 writes
// `store/v11/files/XX/…` + `index.db`, so the two stores can never have the same
// file structure. Also, pnpm 11 moved `store-dir` out of `.npmrc` into
// `pnpm-workspace.yaml`, so the current test setup's `.npmrc`-based store-dir
// config is ignored by pnpm 11 entirely.
#[ignore = "requires pnpm v11 store format in pacquet (#244)"]
#[test]
fn same_file_structure() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

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
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
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

    drop((root, mock_instance)); // cleanup
}

#[ignore = "requires pnpm v11 store format in pacquet (#244)"]
#[test]
fn same_index_file_contents() {
    let CommandTempCwd { pacquet, pnpm, root, workspace, npmrc_info } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

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
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
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

    drop((root, mock_instance)); // cleanup
}
