use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use std::fs;

#[test]
fn should_run_a_script() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let build_txt_path = workspace.join("build.txt");
    let build_txt_content = "Build output";

    let write_script =
        format!(r#"require("fs").writeFileSync({build_txt_path:?}, "{build_txt_content}")"#);

    let package_json_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
          "scripts": {
       "build": format!("node -e '{write_script}'"),
         },
    })
    .to_string();

    fs::write(package_json_path, package_json_content).expect("write to package.json");

    pacquet.with_arg("run").arg("build").assert().success();

    let output = fs::read_to_string(build_txt_path).expect("read build.txt");

    assert_eq!(output, build_txt_content);

    drop(root);
}

#[test]
fn should_run_a_script_with_arguments() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    let build_txt_path = workspace.join("build.txt");

    let record_args_script = format!(
        r#"require("fs").writeFileSync({build_txt_path:?}, process.argv.slice(1).join(" "))"#
    );

    let package_json_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({
        "scripts": {
            "build": format!("node -e '{record_args_script}'"),
        },
    })
    .to_string();

    fs::write(package_json_path, package_json_content).expect("write to package.json");

    pacquet
        .with_arg("run")
        .arg("build")
        .arg("arg1")
        .arg("arg2")
        .arg("--")
        .arg("--flag1")
        .assert()
        .success();

    let output = fs::read_to_string(build_txt_path).expect("read build.txt");

    assert_eq!(output, "arg1 arg2 --flag1");

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

    pacquet.with_arg("run").arg("build").assert().failure();

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

    pacquet.with_arg("run").arg("build").arg("--if-present").assert().success();

    drop(root);
}
