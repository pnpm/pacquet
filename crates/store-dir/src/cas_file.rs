use crate::{FileHash, StoreDir};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{ensure_file, file_mode::is_executable, EnsureFileError};
use sha2::{Digest, Sha512};
use std::path::PathBuf;

impl StoreDir {
    /// Path to a file in the store directory.
    pub fn cas_file_path(&self, hash: FileHash, mode: u32) -> PathBuf {
        let hex = format!("{hash:x}");
        let suffix = if is_executable(mode) { "-exec" } else { "" };
        self.file_path_by_hex_str(&hex, suffix)
    }
}

/// Error type of [`StoreDir::write_cas_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum WriteCasFileError {
    WriteFile(EnsureFileError),
}

impl StoreDir {
    /// Write a file from an npm package to the store directory.
    pub fn write_cas_file(
        &self,
        buffer: &[u8],
        mode: u32,
    ) -> Result<(PathBuf, FileHash), WriteCasFileError> {
        let file_hash = Sha512::digest(buffer);
        let file_path = self.cas_file_path(file_hash, mode);
        ensure_file(&file_path, buffer, Some(mode)).map_err(WriteCasFileError::WriteFile)?;
        Ok((file_path, file_hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_fs::file_mode::{ALL_RW, ALL_RWX};

    #[test]
    fn cas_file_path() {
        fn case(file_content: &str, mode: u32, expected: &str) {
            eprintln!("CASE: {file_content:?}, {mode:?}");
            let store_dir = StoreDir::new("STORE_DIR");
            let file_hash = Sha512::digest(file_content);
            eprintln!("file_hash = {file_hash:x}");
            let received = store_dir.cas_file_path(file_hash, mode);
            let expected: PathBuf = expected.split('/').collect();
            assert_eq!(&received, &expected);
        }

        case(
            "hello world",
            ALL_RW,
            "STORE_DIR/v3/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f",
        );

        case(
            "hello world",
            ALL_RWX,
            "STORE_DIR/v3/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f-exec",
        );
    }
}
