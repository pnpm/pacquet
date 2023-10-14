use assert_cmd::prelude::*;
use std::process::Command;
use tempfile::{tempdir, TempDir};

pub fn pacquet_with_temp_cwd() -> (Command, TempDir) {
    let current_dir = tempdir().expect("create temporary working directory for pacquet");
    let command = Command::cargo_bin("pacquet").expect("find the pacquet binary");
    (command, current_dir)
}
