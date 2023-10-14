use assert_cmd::prelude::*;
use pacquet_testing_utils::bin::pacquet_with_temp_cwd;
use std::ffi::OsStr;
use tempfile::TempDir;

pub fn exec_pacquet_in_temp_cwd<Args>(args: Args) -> TempDir
where
    Args: IntoIterator,
    Args::Item: AsRef<OsStr>,
{
    let (mut command, current_dir) = pacquet_with_temp_cwd();
    command.args(args).assert().success();
    current_dir
}
