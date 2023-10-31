use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_store_dir::{FileHash, FileSuffix, StoreDir};
use sha2::{Digest, Sha512};
use ssri::Integrity;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Display, Error, From, Diagnostic)]
#[non_exhaustive]
pub enum CafsError {
    #[diagnostic(code(pacquet_cafs::io_error))]
    Io(std::io::Error), // TODO: remove derive(From), split this variant
}

fn write_file_if_not_exist(file_path: &Path, content: &[u8]) -> io::Result<()> {
    if file_path.exists() {
        return Ok(());
    }

    let parent_dir = file_path.parent().unwrap();
    fs::create_dir_all(parent_dir)?;
    fs::write(file_path, content)
}

pub fn write_non_index_file(
    store_dir: &StoreDir,
    buffer: &[u8],
    suffix: Option<FileSuffix>,
) -> Result<(PathBuf, FileHash), CafsError> {
    let file_hash = Sha512::digest(buffer);
    let file_path = store_dir.file_path_by_content_address(file_hash, suffix);

    write_file_if_not_exist(&file_path, buffer)?;

    #[cfg(unix)]
    {
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};
        if suffix == Some(FileSuffix::Exec) {
            let permissions = Permissions::from_mode(0o777);
            fs::set_permissions(&file_path, permissions).expect("make the file executable");
        }
    }

    Ok((file_path, file_hash))
}

pub fn write_tarball_index_file(
    store_dir: &StoreDir,
    tarball_integrity: &Integrity,
    index_content: &str,
) -> Result<(), CafsError> {
    let file_path = store_dir.tarball_index_file_path(tarball_integrity);
    write_file_if_not_exist(&file_path, index_content.as_bytes())?;
    Ok(())
}

pub fn prune_sync(_store_dir: &StoreDir) -> Result<(), CafsError> {
    // Ref: https://pnpm.io/cli/store#prune
    todo!("remove orphaned files")
}
