use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use crate::package_manager::PackageManagerError;
use pacquet_diagnostics::tracing;
use pacquet_fs::symlink_dir;
use pacquet_npmrc::PackageImportMethod;
use pacquet_package_manager::auto_import;
use rayon::prelude::*;

pub trait ImportMethodImpl {
    fn import(
        &self,
        cas_files: &HashMap<OsString, PathBuf>,
        save_path: &Path,
        symlink_to: &Path,
    ) -> Result<(), PackageManagerError>;
}

impl ImportMethodImpl for PackageImportMethod {
    fn import(
        &self,
        cas_files: &HashMap<OsString, PathBuf>,
        save_path: &Path,
        symlink_to: &Path,
    ) -> Result<(), PackageManagerError> {
        tracing::info!(target: "pacquet::import", ?save_path, ?symlink_to, "Import package");
        match self {
            PackageImportMethod::Auto => {
                if !save_path.exists() {
                    cas_files
                        .into_par_iter()
                        .try_for_each(|(cleaned_entry, store_path)| {
                            auto_import(store_path, &save_path.join(cleaned_entry))
                        })
                        .expect("expected no write errors");
                }

                if !symlink_to.is_symlink() {
                    if let Some(parent_dir) = symlink_to.parent() {
                        fs::create_dir_all(parent_dir)?;
                    }
                    symlink_dir(save_path, symlink_to)?;
                }
            }
            _ => panic!("Not implemented yet"),
        }

        Ok(())
    }
}
