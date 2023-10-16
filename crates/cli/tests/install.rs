use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::{
    bin::pacquet_with_temp_cwd,
    fs::{get_all_folders, is_symlink_or_junction},
};
use std::fs;

#[test]
fn should_install_dependencies() {
    let (command, dir) = pacquet_with_temp_cwd();

    eprintln!("Creating package.json...");
    let manifest_path = dir.path().join("package.json");
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
    assert!(is_symlink_or_junction(&dir.path().join("node_modules/is-odd")).unwrap());
    assert!(dir.path().join("node_modules/.pacquet/is-odd@3.0.1").exists());

    eprintln!("Make sure it installs direct dependencies");
    assert!(!dir.path().join("node_modules/is-number").exists());
    assert!(dir.path().join("node_modules/.pacquet/is-number@6.0.0").exists());

    eprintln!("Make sure we install dev-dependencies as well");
    assert!(
        is_symlink_or_junction(&dir.path().join("node_modules/fast-decode-uri-component")).unwrap()
    );
    assert!(dir.path().join("node_modules/.pacquet/fast-decode-uri-component@1.0.1").is_dir());

    eprintln!("Directory list");
    insta::assert_debug_snapshot!(get_all_folders(dir.path()));
}
