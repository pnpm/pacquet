use crate::create_cas_files;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::symlink_dir;
use pacquet_npmrc::PackageImportMethod;
use std::{
    collections::HashMap,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Display, Error, Diagnostic)]
pub enum ImportPackageError {
    #[display(fmt = "cannot create parent dir for {symlink_path:?}: {error}")]
    CreateParentDir {
        symlink_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display(fmt = "cannot create symlink at {symlink_path:?}: {error}")]
    SymlinkDir {
        symlink_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// This subroutine does 2 things:
/// 1. Populate files into [`save_path`](Self::save_path) according to [`cas_paths`](Self::cas_paths) and [`method`](Self::method).
/// 2. Create at [`symlink_path`](Self::symlink_path) which points to [`save_path`](Self::save_path).
#[must_use]
pub struct ImportPackage<'a> {
    pub method: PackageImportMethod,
    pub cas_paths: &'a HashMap<OsString, PathBuf>,
    pub save_path: &'a Path,
    pub symlink_path: &'a Path,
}

impl<'a> ImportPackage<'a> {
    pub fn import_pkg(self) -> Result<(), ImportPackageError> {
        let ImportPackage { method, cas_paths, save_path, symlink_path } = self;

        tracing::info!(target: "pacquet::import", ?save_path, ?symlink_path, "Import package");
        match method {
            PackageImportMethod::Auto => {
                create_cas_files(save_path, cas_paths).expect("no write errors"); // TODO: properly propagate the error

                if !symlink_path.is_symlink() {
                    if let Some(parent_dir) = symlink_path.parent() {
                        fs::create_dir_all(parent_dir).map_err(|error| {
                            ImportPackageError::CreateParentDir {
                                symlink_path: symlink_path.to_path_buf(),
                                error,
                            }
                        })?;
                    }
                    symlink_dir(save_path, symlink_path).map_err(|error| {
                        ImportPackageError::SymlinkDir {
                            symlink_path: symlink_path.to_path_buf(),
                            error,
                        }
                    })?;
                }
            }
            _ => panic!("Not implemented yet"),
        }

        Ok(())
    }
}
