pub mod _utils;
pub use _utils::*;

use command_extra::CommandExtra;
use pacquet_testing_utils::{bin::pacquet_with_temp_cwd, fs::get_filenames_in_folder};
use pretty_assertions::assert_eq;
use std::{env, fs};

#[test]
fn should_create_package_json() {
    let dir = exec_pacquet_in_temp_cwd(["init"]);

    let package_json_path = dir.path().join("package.json");
    dbg!(&package_json_path);

    eprintln!("Content of package.json");
    let package_json_content =
        fs::read_to_string(&package_json_path).expect("read from package.json");
    insta::assert_snapshot!(package_json_content);

    eprintln!("Created files");
    assert_eq!(get_filenames_in_folder(dir.path()), ["package.json"]);
}

#[test]
fn should_throw_on_existing_file() {
    let (command, dir) = pacquet_with_temp_cwd();

    let package_json_path = dir.path().join("package.json");
    dbg!(&package_json_path);

    eprintln!("Creating package.json...");
    fs::write(&package_json_path, "{}").expect("write to package.json");

    eprintln!("Executing pacquet init...");
    let output = command.with_arg("init").output().expect("execute pacquet init");
    dbg!(&output);

    eprintln!("Exit status code");
    assert!(!output.status.success());

    eprintln!("Stderr");
    insta::assert_snapshot!(String::from_utf8_lossy(&output.stderr).trim_end());
}
