use command_extra::CommandExtra;
use pacquet_testing_utils::bin::pacquet_with_temp_cwd;
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use std::fs;

#[test]
fn store_path_should_return_store_dir_from_npmrc() {
    let (command, dir) = pacquet_with_temp_cwd();

    eprintln!("Creating .npmrc...");
    fs::write(dir.path().join(".npmrc"), "store-dir=foo/bar").expect("write to .npmrc");

    eprintln!("Executing pacquet store path...");
    let output = command.with_args(["store", "path"]).output().expect("run pacquet store path");
    dbg!(&output);

    eprintln!("Exit status code");
    assert!(output.status.success());

    eprintln!("Stdout");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        dir.path().pipe(fs::canonicalize).unwrap().join("foo/bar").to_string_lossy(),
    );
}
