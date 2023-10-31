use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::pacquet_with_temp_cwd,
    fs::{get_all_files, get_all_folders, is_symlink_or_junction},
};
use pipe_trait::Pipe;
use std::fs;

#[test]
fn should_install_dependencies() {
    let (command, root, workspace) = pacquet_with_temp_cwd(true);

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
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    command.with_arg("install").assert().success();

    eprintln!("Make sure the package is installed");
    assert!(is_symlink_or_junction(&workspace.join("node_modules/is-odd")).unwrap());
    assert!(workspace.join("node_modules/.pacquet/is-odd@3.0.1").exists());

    eprintln!("Make sure it installs direct dependencies");
    assert!(!workspace.join("node_modules/is-number").exists());
    assert!(workspace.join("node_modules/.pacquet/is-number@6.0.0").exists());

    eprintln!("Make sure we install dev-dependencies as well");
    assert!(
        is_symlink_or_junction(&workspace.join("node_modules/fast-decode-uri-component")).unwrap()
    );
    assert!(workspace.join("node_modules/.pacquet/fast-decode-uri-component@1.0.1").is_dir());

    eprintln!("Snapshot");
    let workspace_folders = get_all_folders(&workspace);
    let store_files = get_all_files(&root.path().join("pacquet-store"));
    insta::assert_debug_snapshot!((workspace_folders, store_files));

    drop(root); // cleanup
}

#[test]
fn should_install_exec_files() {
    let (command, root, workspace) = pacquet_with_temp_cwd(true);

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "dependencies": {
            "pretty-exec": "0.3.10",
        },
    });
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing command...");
    command.with_arg("install").assert().success();

    eprintln!("Listing all files in the store...");
    let store_files = root.path().join("pacquet-store").pipe_as_ref(get_all_files);
    let (exec_files, non_exec_files): (Vec<_>, Vec<_>) =
        store_files.iter().partition(|path| path.ends_with("-exec"));

    eprintln!("Snapshot");
    insta::assert_debug_snapshot!(exec_files);

    #[cfg(unix)]
    {
        use pacquet_testing_utils::fs::is_path_executable;
        use pretty_assertions::assert_eq;

        eprintln!("All files that end with '-exec' are executable, others not");
        assert_eq!(
            store_files.iter().partition(|name| {
                root.path().join("pacquet-store").join(name).pipe_as_ref(is_path_executable)
            }),
            (exec_files, non_exec_files),
        );
    }

    #[cfg(windows)]
    let _ = non_exec_files;

    drop(root); // cleanup
}
