use command_extra::CommandExtra;
use pacquet_testing_utils::bin::CommandTempCwd;
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Handle the slight difference between OSes.
///
/// **TODO:** may be we should have handle them in the production code instead?
fn canonicalize(path: &Path) -> PathBuf {
    if cfg!(windows) {
        path.to_path_buf()
    } else {
        dunce::canonicalize(path).expect("canonicalize path")
    }
}

#[test]
fn store_path_should_return_store_dir_from_npmrc() {
    let CommandTempCwd { pacquet, root, workspace, .. } = CommandTempCwd::init();

    eprintln!("Creating .npmrc...");
    fs::write(workspace.join(".npmrc"), "store-dir=foo/bar").expect("write to .npmrc");

    eprintln!("Executing pacquet store path...");
    let output = pacquet.with_args(["store", "path"]).output().expect("run pacquet store path");
    dbg!(&output);

    eprintln!("Exit status code");
    assert!(output.status.success());

    eprintln!("Stdout");
    let normalize = |path: &str| path.replace('\\', "/");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end().pipe(normalize),
        canonicalize(&workspace).join("foo/bar").to_string_lossy().pipe_as_ref(normalize),
    );

    drop(root); // cleanup
}
