use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use std::{fs, path::PathBuf, process::Command};
use tempfile::{tempdir, TempDir};
use text_block_macros::text_block_fnl;

const DEFAULT_NPMRC: &str = text_block_fnl! {
    "store-dir=../pacquet-store"
    "cache-dir=../pacquet-cache"
};

pub fn pacquet_with_temp_cwd(create_npmrc: bool) -> (Command, TempDir, PathBuf) {
    let root = tempdir().expect("create temporary directory");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("create temporary workspace for pacquet");
    if create_npmrc {
        fs::write(workspace.join(".npmrc"), DEFAULT_NPMRC).expect("write to .npmrc");
    }
    let command = Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(&workspace);
    (command, root, workspace)
}
