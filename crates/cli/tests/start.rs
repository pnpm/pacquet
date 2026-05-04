pub mod _utils;
use std::fs;

pub use _utils::*;

use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;

#[test]
fn should_fail_if_no_script_and_no_server_dot_js() {
    let CommandTempCwd { pacquet, root, workspace, .. } =
        CommandTempCwd::init().add_mocked_registry();

    eprintln!("Creating package.json...");
    let manifest_path = workspace.join("package.json");
    let package_json_content = serde_json::json!({});
    fs::write(&manifest_path, package_json_content.to_string()).expect("write to package.json");

    eprintln!("Executing pnpm start...");
    let output = pacquet.with_arg("start").output().expect("could not start");

    dbg!(&output);

    eprintln!("Exit status code");
    assert!(!output.status.success());

    eprintln!("Stderr");
    insta::assert_snapshot!(String::from_utf8_lossy(&output.stderr).trim_end());

    assert!(!output.status.success());

    drop(root)
}
