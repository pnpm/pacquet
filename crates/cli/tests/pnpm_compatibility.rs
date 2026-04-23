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

    // Filter out pnpm-only artifacts whose presence is orthogonal to whether
    // the two tools agree on the CAFS layout:
    //   * `v11/projects/<hash>` — pnpm-11-only per-project metadata tracking
    //     which packages in the store are linked from which project. Pacquet
    //     doesn't yet populate this, and sharing the store doesn't require it.
    //   * `v11/index.db-wal` / `v11/index.db-shm` — SQLite WAL sidecars that
    //     only exist while a connection is open; their presence at comparison
    //     time depends on whether the checkpoint ran before we measured.
    let normalize = |files: Vec<String>| -> Vec<String> {
        files
            .into_iter()
            // Per-project metadata that pnpm 11 populates and pacquet doesn't.
            // Doesn't affect the shared-cafs story.
            .filter(|p| !p.starts_with("v11/projects/"))
            // Hoisted-symlinks layout introduced in pnpm 11 — pnpm stores
            // one `node_modules` tree per `<name>/<version>/<hash>/` under
            // `v11/links/` and links the project's `node_modules/X` into there.
            // Pacquet still uses the older per-project `.pnpm/` virtual store,
            // so these paths exist only on the pnpm side.
            .filter(|p| !p.starts_with("v11/links/"))
            // SQLite WAL sidecars exist only while a connection holds the
            // journal open. Their presence at compare-time depends on timing.
            .filter(|p| p != "v11/index.db-wal" && p != "v11/index.db-shm")
            .collect()
    };

    eprintln!("Installing with pacquet...");
    pacquet.with_arg("install").assert().success();
    let pacquet_store_files = normalize(get_all_files(&store_dir));

    cleanup();

    eprintln!("Installing with pnpm...");
    pnpm.with_args(["install", "--ignore-scripts"]).assert().success();
    let pnpm_store_files = normalize(get_all_files(&store_dir));

    cleanup();

    eprintln!("Produce the same store dir structure");
    assert_eq!(&pacquet_store_files, &pnpm_store_files);

    drop((root, mock_instance)); // cleanup
}

// pnpm writes its `index.db` values with msgpackr `useRecords: true`, which
// uses extension-typed records that rmp-serde can't decode. Pacquet-written
// entries round-trip fine (we use `to_vec_named`), but reading entries that
// pnpm wrote is blocked on msgpackr-records decoding support — tracked in
// #244 as a follow-up after the v11 store cutover.
#[ignore = "requires msgpackr useRecords decoding to read pnpm-written entries (#244)"]
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
