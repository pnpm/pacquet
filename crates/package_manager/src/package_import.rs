use std::{collections::HashMap, fs, path::PathBuf};

use pacquet_npmrc::PackageImportMethod;

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
                for (cleaned_entry, store_path) in cas_files {
                    let original_path = self.config.store_dir.join(store_path);
                    let save_with_cleaned_entry = save_path.join(cleaned_entry);

                    if !save_with_cleaned_entry.exists() {
                        // Create parent folder
                        if let Some(parent_folder) = save_with_cleaned_entry.parent() {
                            if !parent_folder.exists() {
                                fs::create_dir_all(parent_folder)?;
                            }
                        }

                        reflink_copy::reflink_or_copy(original_path, &save_with_cleaned_entry)?;
                    }
                }

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
