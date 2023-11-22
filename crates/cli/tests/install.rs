pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::{AddMockedRegistry, CommandTempCwd},
    fixtures::{BIG_LOCKFILE, BIG_MANIFEST},
    fs::{get_all_files, get_all_folders, is_symlink_or_junction},
};
use pipe_trait::Pipe;
use std::{
    fs::{self, OpenOptions},
    io::Write, path::Path,
};

#[test]
fn should_install_dependencies() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    pacquet.with_arg("install").assert().success();

    eprintln!("Make sure the package is installed");
    let symlink_path = workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin-parent");
    assert!(is_symlink_or_junction(&symlink_path).unwrap());
    let virtual_path =
        workspace.join("node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin-parent@1.0.0");
    assert!(virtual_path.exists());

    eprintln!("Make sure it installs direct dependencies");
    assert!(!workspace.join("node_modules/@pnpm.e2e/hello-world-js-bin").exists());
    assert!(workspace.join("node_modules/.pnpm/@pnpm.e2e+hello-world-js-bin@1.0.0").exists());

    eprintln!("Snapshot");
    let workspace_folders = get_all_folders(&workspace);
    let store_files = get_all_files(&store_dir);
    insta::assert_debug_snapshot!((workspace_folders, store_files));

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_install_exec_files() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    pacquet.with_arg("install").assert().success();

    eprintln!("Listing all files in the store...");
    let store_files = get_all_files(&store_dir);

    #[cfg(unix)]
    {
        use pacquet_testing_utils::fs::is_path_executable;
        use pretty_assertions::assert_eq;
        use std::{fs::File, iter::repeat, os::unix::fs::MetadataExt};

        eprintln!("All files that end with '-exec' are executable, others not");
        let (suffix_exec, suffix_other) =
            store_files.iter().partition::<Vec<_>, _>(|path| path.ends_with("-exec"));
        let (mode_exec, mode_other) = store_files
            .iter()
            .partition::<Vec<_>, _>(|name| store_dir.join(name).as_path().pipe(is_path_executable));
        assert_eq!((&suffix_exec, &suffix_other), (&mode_exec, &mode_other));

        eprintln!("All executable files have mode 755");
        let actual_modes: Vec<_> = mode_exec
            .iter()
            .map(|name| {
                let mode = store_dir
                    .join(name)
                    .pipe(File::open)
                    .expect("open file to get mode")
                    .metadata()
                    .expect("get metadata")
                    .mode();
                (name.as_str(), mode & 0o777)
            })
            .collect();
        let expected_modes: Vec<_> =
            mode_exec.iter().map(|name| name.as_str()).zip(repeat(0o755)).collect();
        assert_eq!(&actual_modes, &expected_modes);
    }

    eprintln!("Snapshot");
    insta::assert_debug_snapshot!(store_files);

    drop((root, mock_instance)); // cleanup
}

#[test]
fn should_install_index_files() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { store_dir, mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    pacquet.with_arg("install").assert().success();

    eprintln!("Snapshot");
    let index_file_contents = index_file_contents(&store_dir);
    insta::assert_yaml_snapshot!(index_file_contents);

    drop((root, mock_instance)); // cleanup
}

#[cfg(not(target_os = "windows"))] // It causes ConnectionAborted on CI
#[cfg(not(target_os = "macos"))] // It causes ConnectionReset on CI
#[test]
fn frozen_lockfile_should_be_able_to_handle_big_lockfile() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    fs::write(manifest_path, BIG_MANIFEST).expect("write to package.json");

    eprintln!("Creating pnpm-lock.yaml...");
    let lockfile_path = workspace.join("pnpm-lock.yaml");
    fs::write(lockfile_path, BIG_LOCKFILE).expect("write to pnpm-lock.yaml");

    eprintln!("Patching .npmrc...");
    let npmrc_path = workspace.join(".npmrc");
    OpenOptions::new()
        .append(true)
        .write(true)
        .open(npmrc_path)
        .expect("open .npmrc to append")
        .write_all(b"\nlockfile=true\n")
        .expect("append to .npmrc");

    eprintln!("Executing command...");
    pacquet.with_args(["install", "--frozen-lockfile"]).assert().success();
}

#[test]
fn should_install_circular_dependencies() {
    let CommandTempCwd { pacquet, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "@pnpm.e2e/circular-deps-1-of-2": "1.0.2",
        },
    });
    fs::write(manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    pacquet.with_arg("install").assert().success();
    
    assert!(workspace.join("./node_modules/@pnpm.e2e/circular-deps-1-of-2").exists());
    assert!(workspace.join("./node_modules/.pnpm/@pnpm.e2e+circular-deps-1-of-2@1.0.2").exists());
    assert!(workspace.join("./node_modules/.pnpm/@pnpm.e2e+circular-deps-2-of-2@1.0.2").exists());

    drop((root, mock_instance)); // cleanup
}
