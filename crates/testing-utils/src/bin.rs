use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use std::{fs, path::PathBuf, process::Command};
use tempfile::{tempdir, TempDir};
use text_block_macros::text_block_fnl;

pub fn pacquet_with_temp_cwd() -> (Command, TempDir) {
    let current_dir = tempdir().expect("create temporary working directory for pacquet");
    let command = Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(current_dir.path());
    (command, current_dir)
}

pub fn pacquet_with_temp_npmrc() -> (Command, TempDir, PathBuf) {
    let root = tempdir().expect("create temporary directory");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("create temporary workspace for pacquet");
    fs::write(
        workspace.join(".npmrc"),
        text_block_fnl! {
            "store-dir=../pacquet-store"
            "cache-dir=../pacquet-cache"
        },
    )
    .expect("write to .npmrc");
    let command = Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(&workspace);
    (command, root, workspace)
}
