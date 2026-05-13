use crate::{Lockfile, serialize_yaml};
use derive_more::{Display, Error};
use pacquet_diagnostics::miette::{self, Diagnostic};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Error when writing the lockfile to the filesystem.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum SaveLockfileError {
    #[display("Failed to get current_dir: {_0}")]
    #[diagnostic(code(pacquet_lockfile::current_dir))]
    CurrentDir(io::Error),

    #[display("Failed to serialize lockfile to YAML: {_0}")]
    #[diagnostic(code(pacquet_lockfile::serialize_yaml))]
    SerializeYaml(serde_saphyr::ser::Error),

    #[display("Failed to write lockfile content: {_0}")]
    #[diagnostic(code(pacquet_lockfile::write_file))]
    WriteFile(io::Error),

    #[display("Failed to create virtual-store directory {dir:?}: {error}")]
    #[diagnostic(code(pacquet_lockfile::create_dir))]
    CreateDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to remove existing current-lockfile at {path:?}: {error}")]
    #[diagnostic(code(pacquet_lockfile::remove_file))]
    RemoveFile {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to rename temp file {tmp:?} over {target:?}: {error}")]
    #[diagnostic(code(pacquet_lockfile::rename_file))]
    RenameFile {
        tmp: PathBuf,
        target: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

impl Lockfile {
    /// Save lockfile to a specific path.
    pub fn save_to_path(&self, path: &Path) -> Result<(), SaveLockfileError> {
        let content = serialize_yaml::to_string(self).map_err(SaveLockfileError::SerializeYaml)?;
        fs::write(path, content).map_err(SaveLockfileError::WriteFile)
    }

    /// Save lockfile to `pnpm-lock.yaml` in the current directory.
    pub fn save_to_current_dir(&self) -> Result<(), SaveLockfileError> {
        let file_path =
            env::current_dir().map_err(SaveLockfileError::CurrentDir)?.join(Lockfile::FILE_NAME);
        self.save_to_path(&file_path)
    }

    /// Save the *current* lockfile under
    /// `<virtual_store_dir>/lock.yaml` at end-of-install. Mirrors
    /// upstream's `writeCurrentLockfile` at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/fs/src/write.ts#L41-L51>:
    ///
    /// - When the lockfile is empty ([`Lockfile::is_empty`]) the
    ///   existing file is removed and no new content is written.
    ///   Mirrors upstream's `rimraf` short-circuit so an empty install
    ///   doesn't leave stale state behind.
    /// - Otherwise the directory is created if missing and the file
    ///   is written atomically: serialize → write next-to + rename.
    ///   The rename is the only step an observer can race against,
    ///   so a partial install will never leave a torn lockfile.
    pub fn save_current_to_virtual_store_dir(
        &self,
        virtual_store_dir: &Path,
    ) -> Result<(), SaveLockfileError> {
        let target = virtual_store_dir.join(Lockfile::CURRENT_FILE_NAME);

        if self.is_empty() {
            match fs::remove_file(&target) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(SaveLockfileError::RemoveFile { path: target, error }),
            }
        } else {
            fs::create_dir_all(virtual_store_dir).map_err(|error| {
                SaveLockfileError::CreateDir { dir: virtual_store_dir.to_path_buf(), error }
            })?;
            let content =
                serialize_yaml::to_string(self).map_err(SaveLockfileError::SerializeYaml)?;
            write_atomic(&target, content.as_bytes())
        }
    }
}

/// Write `content` to `target` via a temp file in the same directory
/// followed by `rename`. The rename is atomic on Unix and replaces
/// in-place on Windows, so an observer never sees a torn file.
fn write_atomic(target: &Path, content: &[u8]) -> Result<(), SaveLockfileError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();

    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("lock.yaml"));
    let tmp = parent.join(format!(".{file_name}.{pid}.{counter}.tmp"));

    fs::write(&tmp, content).map_err(SaveLockfileError::WriteFile)?;
    fs::rename(&tmp, target).map_err(|error| {
        // Best-effort cleanup so a failed rename doesn't leak temp
        // files in the virtual store.
        let _ = fs::remove_file(&tmp);
        SaveLockfileError::RenameFile { tmp, target: target.to_path_buf(), error }
    })
}

#[cfg(test)]
mod tests;
