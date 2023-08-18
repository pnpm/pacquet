use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use crate::package_manager::{AutoImportError, PackageManagerError};
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;

pub trait ImportMethodImpl {
    fn import(
        &self,
        cas_files: &HashMap<String, PathBuf>,
        save_path: &Path,
        symlink_to: &Path,
    ) -> Result<(), PackageManagerError>;
}

impl ImportMethodImpl for PackageImportMethod {
    fn import(
        &self,
        cas_files: &HashMap<String, PathBuf>,
        save_path: &Path,
        symlink_to: &Path,
    ) -> Result<(), PackageManagerError> {
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
                    crate::fs::symlink_dir(save_path, symlink_to)?;
                }
            }
            _ => panic!("Not implemented yet"),
        }

        Ok(())
    }
}

fn auto_import(source_file: &Path, target_link: &Path) -> Result<(), AutoImportError> {
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
