use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::{tempdir, TempDir};
use text_block_macros::text_block_fnl;

const DEFAULT_NPMRC: &str = text_block_fnl! {
    "store-dir=../pacquet-store"
    "cache-dir=../pacquet-cache"
};

fn create_default_npmrc(workspace: &Path) {
    fs::write(workspace.join(".npmrc"), DEFAULT_NPMRC).expect("write to .npmrc");
}

pub fn pacquet_with_temp_cwd(create_npmrc: bool) -> (Command, TempDir, PathBuf) {
    let root = tempdir().expect("create temporary directory");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("create temporary workspace for pacquet");
    if create_npmrc {
        create_default_npmrc(&workspace)
    }
    let command = Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(&workspace);
    (command, root, workspace)
}

pub fn pacquet_and_pnpm_with_temp_cwd(create_npmrc: bool) -> (Command, Command, TempDir, PathBuf) {
    let root = tempdir().expect("create temporary directory");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("create temporary workspace for pacquet");
    if create_npmrc {
        create_default_npmrc(&workspace)
    }
    let pacquet = Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(&workspace);
    let pnpm = Command::new("pnpm").with_current_dir(&workspace);
    (pacquet, pnpm, root, workspace)
}
