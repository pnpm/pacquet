use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;

use crate::{symlink::symlink_dir, PackageManager, PackageManagerError};

impl PackageManager {
    pub async fn import_packages(
        &self,
        cas_files: &HashMap<String, PathBuf>,
        save_path: &PathBuf,
        symlink_to: &PathBuf,
    ) -> Result<(), PackageManagerError> {
        match self.config.package_import_method {
            PackageImportMethod::Auto => {
                cas_files
                    .into_par_iter()
                    .try_for_each(|(cleaned_entry, store_path)| {
                        auto_import(
                            self.config.store_dir.join(store_path),
                            save_path.join(cleaned_entry),
                        )
                    })
                    .expect("expected no write errors");

                if !symlink_to.is_symlink() {
                    fs::create_dir_all(symlink_to.parent().unwrap())?;
                    symlink_dir(save_path, symlink_to)?;
                }
            }
            _ => panic!("Not implemented yet"),
        }
        Ok(())
    }
}

fn auto_import<P: AsRef<Path>>(
    original_path: P,
    save_with_cleaned_entry: P,
) -> Result<(), PackageManagerError> {
    if !save_with_cleaned_entry.as_ref().exists() {
        // Create parent folder
        if let Some(parent_folder) = &save_with_cleaned_entry.as_ref().parent() {
            if !parent_folder.exists() {
                fs::create_dir_all(parent_folder)?;
            }
        }

        reflink_copy::reflink_or_copy(original_path, &save_with_cleaned_entry)?;
    }

    Ok(())
}
