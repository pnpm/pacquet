use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::pacquet_with_temp_cwd;
use std::{ffi::OsStr, path::PathBuf};
use tempfile::TempDir;

pub fn exec_pacquet_in_temp_cwd<Args>(create_npmrc: bool, args: Args) -> (TempDir, PathBuf)
where
    Args: IntoIterator,
    Args::Item: AsRef<OsStr>,
{
    let (command, root, workspace) = pacquet_with_temp_cwd(create_npmrc);
    command.with_args(args).assert().success();
    (root, workspace)
}
