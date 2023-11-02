use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_store_dir::{PackageFileInfo, PackageFilesIndex};
use pacquet_testing_utils::bin::CommandTempCwd;
use pipe_trait::Pipe;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::File,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use walkdir::{DirEntry, WalkDir};

pub fn exec_pacquet_in_temp_cwd<Args>(create_npmrc: bool, args: Args) -> (TempDir, PathBuf)
where
    Args: IntoIterator,
    Args::Item: AsRef<OsStr>,
{
    let env = CommandTempCwd::create();
    let (command, root, workspace) = if create_npmrc {
        let env = env.add_default_npmrc();
        (env.pacquet, env.root, env.workspace)
    } else {
        (env.pacquet, env.root, env.workspace)
    };
    command.with_args(args).assert().success();
    (root, workspace)
}

pub fn index_file_contents(
    store_dir: &Path,
) -> BTreeMap<String, BTreeMap<String, PackageFileInfo>> {
    // TODO: refactor the functions in pacquet_testing_utils::fs to be more functional
    // TODO: this function and ones from pacquet_testing_utils::fs can share the suffix code

    let suffix = |entry: &DirEntry| -> String {
        entry
            .path()
            .strip_prefix(store_dir)
            .expect("strip store dir prefix from entry path to create suffix")
            .to_str()
            .expect("convert entry suffix to UTF-8")
            .replace('\\', "/")
    };

    let sanitize = |mut value: PackageFileInfo| {
        value.checked_at = None; // this value depends on time, therefore not deterministic
        value
    };

    let content = |entry: &DirEntry| -> BTreeMap<_, _> {
        entry
            .path()
            .pipe(File::open)
            .expect("open file to read")
            .pipe(serde_json::from_reader::<_, PackageFilesIndex>)
            .expect("read and parse file")
            .files
            .into_iter()
            .map(|(key, value)| (key, sanitize(value)))
            .collect()
    };

    WalkDir::new(store_dir)
        .into_iter()
        .map(|entry| entry.expect("get entry"))
        .filter(|entry| entry.file_name().to_string_lossy().ends_with("-index.json"))
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| (suffix(&entry), content(&entry)))
        .collect()
}
