use std::{collections::HashMap, fs, path::PathBuf};

use pacquet_npmrc::PackageImportMethod;

use crate::{symlink::symlink_dir, PackageManager, PackageManagerError};

impl PackageManager {
    pub async fn import_packages(
        &self,
        cas_files: &HashMap<String, Vec<u8>>,
        save_path: &PathBuf,
        symlink_to: &PathBuf,
    ) -> Result<(), PackageManagerError> {
        match self.config.package_import_method {
            PackageImportMethod::Auto => {
                for (cleaned_entry, buffer) in cas_files {
                    let save_with_cleaned_entry = save_path.join(cleaned_entry);

                    // Create parent folder
                    fs::create_dir_all(save_with_cleaned_entry.parent().unwrap()).unwrap();

                    fs::write(&save_with_cleaned_entry, buffer)?;
                }

                if !symlink_to.exists() {
                    fs::create_dir_all(symlink_to.parent().unwrap())?;
                    symlink_dir(save_path, symlink_to)?;
                }
            }
            _ => panic!("Not implemented yet"),
        }
        Ok(())
    }
}
