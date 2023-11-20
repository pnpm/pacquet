use crate::{FileHash, PackageFilesIndex, StoreDir};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{ensure_file, file_mode, EnsureFileError};
use sha2::{Digest, Sha512};
use ssri::{Algorithm, Integrity};
use std::path::PathBuf;

impl StoreDir {
    /// Path to a file in the store directory.
    pub fn cas_file_path(&self, hash: FileHash, executable: bool) -> PathBuf {
        let hex = format!("{hash:x}");
        let suffix = if executable { "-exec" } else { "" };
        self.file_path_by_hex_str(&hex, suffix)
    }

    /// List maps from index entry to real or would-be file path in the store directory.
    pub fn cas_file_paths_by_index<'a>(
        &'a self,
        index: &'a PackageFilesIndex,
    ) -> impl Iterator<Item = (&'a str, PathBuf)> + 'a {
        index.files.iter().map(|(entry_path, info)| {
            let entry_path = entry_path.as_str();
            let (algorithm, hex) = info
                .integrity
                .parse::<Integrity>()
                .expect("parse integrity") // TODO: parse integrity before this
                .to_hex();
            assert!(
                matches!(algorithm, Algorithm::Sha512),
                "Only Sha512 is supported. {algorithm} isn't",
            ); // TODO: write a custom parser and remove this
            let suffix = if file_mode::is_all_exec(info.mode) { "-exec" } else { "" };
            let cas_path = self.file_path_by_hex_str(&hex, suffix);
            (entry_path, cas_path)
        })
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
        executable: bool,
    ) -> Result<(PathBuf, FileHash), WriteCasFileError> {
        let file_hash = Sha512::digest(buffer);
        let file_path = self.cas_file_path(file_hash, executable);
        let mode = executable.then_some(file_mode::EXEC_MODE);
        ensure_file(&file_path, buffer, mode).map_err(WriteCasFileError::WriteFile)?;
        Ok((file_path, file_hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cas_file_path() {
        fn case(file_content: &str, executable: bool, expected: &str) {
            eprintln!("CASE: {file_content:?}, {executable:?}");
            let store_dir = StoreDir::new("STORE_DIR");
            let file_hash = Sha512::digest(file_content);
            eprintln!("file_hash = {file_hash:x}");
            let received = store_dir.cas_file_path(file_hash, executable);
            let expected: PathBuf = expected.split('/').collect();
            assert_eq!(&received, &expected);
        }

        case(
            "hello world",
            false,
            "STORE_DIR/v3/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f",
        );

        case(
            "hello world",
            true,
            "STORE_DIR/v3/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f-exec",
        );
    }
}
