use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::fs;

#[test]
fn should_run_a_script() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let build_txt_path = workspace.join("build.txt");
    let build_txt_content = "Build output";

    let package_json_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "scripts": {
            "build": format!("echo -n \"{build_txt_content}\" > {build_txt_path:?}"),
        },
    })
    .to_string();

    fs::write(package_json_path, package_json_content).expect("write to package.json");

    pacquet.with_arg("build").assert().success();

    let output = fs::read_to_string(build_txt_path).expect("read build.txt");

    assert_eq!(output, build_txt_content);

    drop(root);
}

#[test]
fn should_run_a_script_with_arguments() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let build_txt_path = workspace.join("build.txt");

    let record_args_script = r#"const fs = require("node:fs");
    fs.writeFileSync("build.txt", process.argv.slice(2).join("\n"));
    "#;
    let record_args_script_path = workspace.join("record_args.js");

    let package_json_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "scripts": {
            "build": "node ./record_args.js",
        },
    })
    .to_string();

    fs::write(package_json_path, package_json_content).expect("write to package.json");

    fs::write(record_args_script_path, record_args_script).expect("write to record_args.js");

    pacquet.with_arg("build").arg("arg1").arg("arg2").arg("--").arg("--flag1").assert().success();

    let output = fs::read_to_string(build_txt_path).expect("read build.txt");

    assert_eq!(output, "arg1\narg2\n--flag1");

    drop(root);
}

#[test]
fn should_fail_if_script_does_not_exist() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let package_json_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "scripts": {}
    })
    .to_string();

    fs::write(package_json_path, package_json_content).expect("write to package.json");

    pacquet.with_arg("build").assert().failure();

    drop(root);
}

#[test]
fn should_not_fail_if_script_does_not_exist_but_if_present_flag_is_set() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let package_json_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "scripts": {}
    })
    .to_string();

    fs::write(package_json_path, package_json_content).expect("write to package.json");

    pacquet.with_arg("--if-present").arg("build").assert().success();

    drop(root);
}

#[test]
fn should_fail_if_no_command_is_specified() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let package_json_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "scripts": {}
    })
    .to_string();

    fs::write(package_json_path, package_json_content).expect("write to package.json");

    pacquet.with_arg("--this-flag-does-not-exist").assert().failure();

    drop(root);
}
