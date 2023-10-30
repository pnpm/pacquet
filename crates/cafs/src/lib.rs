use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_store_dir::StoreDir;
use ssri::{Algorithm, IntegrityOpts};
use std::{fs, path::PathBuf};

#[derive(Debug, Display, Error, From, Diagnostic)]
#[non_exhaustive]
pub enum CafsError {
    #[diagnostic(code(pacquet_cafs::io_error))]
    Io(std::io::Error), // TODO: remove derive(From), split this variant
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

pub fn prune_sync(_store_dir: &StoreDir) -> Result<(), CafsError> {
    // Ref: https://pnpm.io/cli/store#prune
    todo!("remove orphaned files")
}
