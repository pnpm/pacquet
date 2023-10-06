use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::symlink_dir;
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;
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
                if !save_path.exists() {
                    cas_paths
                        .into_par_iter()
                        .try_for_each(|(cleaned_entry, store_path)| {
                            auto_import(store_path, &save_path.join(cleaned_entry))
                        })
                        .expect("expected no write errors");
                }

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

#[derive(Debug, Display, Error, Diagnostic)]
pub enum AutoImportError {
    #[display(fmt = "cannot create directory at {dirname:?}: {error}")]
    CreateDir {
        dirname: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display(fmt = "fail to create a link from {from:?} to {to:?}: {error}")]
    CreateLink {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

pub fn auto_import(source_file: &Path, target_link: &Path) -> Result<(), AutoImportError> {
    if target_link.exists() {
        return Ok(());
    }

    if let Some(parent_dir) = target_link.parent() {
        fs::create_dir_all(parent_dir).map_err(|error| AutoImportError::CreateDir {
            dirname: parent_dir.to_path_buf(),
            error,
        })?;
    }

    reflink_copy::reflink_or_copy(source_file, target_link).map_err(|error| {
        AutoImportError::CreateLink {
            from: source_file.to_path_buf(),
            to: target_link.to_path_buf(),
            error,
        }
    })?; // TODO: add hardlink

    Ok(())
}
