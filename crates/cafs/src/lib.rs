#![allow(unused)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_store_dir::StoreDir;
use ssri::{Algorithm, IntegrityOpts};

#[derive(Debug, Display, Error, From, Diagnostic)]
#[non_exhaustive]
pub enum CafsError {
    #[diagnostic(code(pacquet_cafs::io_error))]
    Io(std::io::Error), // TODO: remove derive(From), split this variant
}

enum FileType {
    Exec,
    NonExec,
    Index,
}

impl FileType {
    fn file_name_suffix(&self) -> &'static str {
        match self {
            FileType::Exec => "-exec",
            FileType::NonExec => "",
            FileType::Index => "-index.json",
        }
    }
}

fn content_path_from_hex(file_type: FileType, hex: &str) -> PathBuf {
    let file_name = format!("{}{}", &hex[2..], file_type.file_name_suffix());
    Path::new(&hex[..2]).join(file_name)
}

pub fn write_sync(store_dir: &StoreDir, buffer: &[u8]) -> Result<PathBuf, CafsError> {
    let integrity = IntegrityOpts::new().algorithm(Algorithm::Sha512).chain(buffer).result();
    let file_path = store_dir.file_path_by_content_address(&integrity, None);

    if !file_path.exists() {
        let parent_dir = file_path.parent().unwrap();
        fs::create_dir_all(parent_dir)?;
        fs::write(&file_path, buffer)?;
    }

    Ok(file_path)
}

pub fn prune_sync(store_dir: &Path) -> Result<(), CafsError> {
    // TODO: This should remove unreferenced packages, not all packages.
    // Ref: https://pnpm.io/cli/store#prune
    fs::remove_dir_all(store_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{env, str::FromStr};

    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn create_content_path_from_hex() {
        assert_eq!(
            content_path_from_hex(FileType::NonExec, "1234567890abcdef1234567890abcdef12345678"),
            PathBuf::from("12/34567890abcdef1234567890abcdef12345678")
        );
        assert_eq!(
            content_path_from_hex(FileType::Exec, "1234567890abcdef1234567890abcdef12345678"),
            PathBuf::from("12/34567890abcdef1234567890abcdef12345678-exec")
        );
        assert_eq!(
            content_path_from_hex(FileType::Index, "1234567890abcdef1234567890abcdef12345678"),
            PathBuf::from("12/34567890abcdef1234567890abcdef12345678-index.json")
        );
    }

    #[test]
    fn should_write_and_clear() {
        let dir = tempdir().unwrap();
        let buffer = vec![0, 1, 2, 3, 4, 5, 6];
        let store_path = write_sync(&dir.path().to_path_buf().into(), &buffer).unwrap();
        assert!(store_path.exists());
        prune_sync(dir.path()).unwrap();
        assert!(!store_path.exists());
    }
}
