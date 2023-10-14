use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use std::process::Command;
use tempfile::{tempdir, TempDir};

pub fn pacquet_with_temp_cwd() -> (Command, TempDir) {
    let current_dir = tempdir().expect("create temporary working directory for pacquet");
    let command = Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(current_dir.path());
    (command, current_dir)
}
