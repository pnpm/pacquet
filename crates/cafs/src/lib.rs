use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_store_dir::{FileSuffix, StoreDir};
use sha2::{Digest, Sha512};
use std::{fs, path::PathBuf};

#[derive(Debug, Display, Error, From, Diagnostic)]
#[non_exhaustive]
pub enum CafsError {
    #[diagnostic(code(pacquet_cafs::io_error))]
    Io(std::io::Error), // TODO: remove derive(From), split this variant
}

pub fn write_sync(
    store_dir: &StoreDir,
    buffer: &[u8],
    suffix: Option<FileSuffix>,
) -> Result<PathBuf, CafsError> {
    let file_hash = Sha512::digest(buffer);
    let file_path = store_dir.file_path_by_content_address(file_hash, suffix);

    if file_path.exists() {
        return Ok(file_path);
    }

    let parent_dir = file_path.parent().unwrap();
    fs::create_dir_all(parent_dir)?;
    fs::write(&file_path, buffer)?;

    #[cfg(unix)]
    {
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};
        if suffix == Some(FileSuffix::Exec) {
            let permissions = Permissions::from_mode(0o777);
            fs::set_permissions(&file_path, permissions).expect("make the file executable");
        }
    }

    Ok(file_path)
}

pub fn prune_sync(_store_dir: &StoreDir) -> Result<(), CafsError> {
    // Ref: https://pnpm.io/cli/store#prune
    todo!("remove orphaned files")
}
