use derive_more::From;
use serde::{Deserialize, Serialize};
use sha2::{digest, Sha512};
use std::path::{self, PathBuf};
use strum::IntoStaticStr;

/// Content hash of a file.
pub type FileHash = digest::Output<Sha512>;

/// Optional suffix of a content address of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
pub enum FileSuffix {
    #[strum(serialize = "-exec")]
    Exec,
    #[strum(serialize = "-index.json")]
    Index,
}

/// Represent a store directory.
///
/// * The store directory stores all files that were acquired by installing packages with pacquet or pnpm.
/// * The files in `node_modules` directories are hardlinks or reflinks to the files in the store directory.
/// * The store directory can and often act as a global shared cache of all installation of different workspaces.
/// * The location of the store directory can be customized by `store-dir` field.
#[derive(Debug, PartialEq, Eq, From, Deserialize, Serialize)]
#[serde(transparent)]
pub struct StoreDir {
    /// Path to the root of the store directory from which all sub-paths are derived.
    ///
    /// Consumer of this struct should interact with the sub-paths instead of this path.
    root: PathBuf,
}

impl StoreDir {
    /// Construct an instance of [`StoreDir`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        root.into().into()
    }

    /// Create an object that [displays](std::fmt::Display) the root of the store directory.
    pub fn display(&self) -> path::Display {
        self.root.display()
    }

    /// Get `{store}/v3`.
    fn v3(&self) -> PathBuf {
        self.root.join("v3")
    }

    /// The directory that contains all files from the once-installed packages.
    fn files(&self) -> PathBuf {
        self.v3().join("files")
    }

    /// Path to a file in the store directory.
    ///
    /// **Parameters:**
    /// * `head` is the first 2 hexadecimal digit of the file address.
    /// * `tail` is the rest of the address and an optional suffix.
    fn file_path_by_head_tail(&self, head: &str, tail: &str) -> PathBuf {
        self.files().join(head).join(tail)
    }

    /// Path to a file in the store directory.
    pub fn file_path_by_hex_str(&self, hex: &str, suffix: Option<FileSuffix>) -> PathBuf {
        let head = &hex[..2];
        let middle = &hex[2..];
        let suffix = suffix.map_or("", <&str>::from);
        let tail = format!("{middle}{suffix}");
        self.file_path_by_head_tail(head, &tail)
    }

    /// Path to a file in the store directory.
    pub fn file_path_by_content_address(
        &self,
        hash: FileHash,
        suffix: Option<FileSuffix>,
    ) -> PathBuf {
        let hex = format!("{hash:x}");
        self.file_path_by_hex_str(&hex, suffix)
    }

    /// Path to the temporary directory inside the store.
    pub fn tmp(&self) -> PathBuf {
        self.v3().join("tmp")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use sha2::{Digest, Sha512};

    #[test]
    fn file_path_by_head_tail() {
        let received = "/home/user/.local/share/pnpm/store"
            .pipe(StoreDir::new)
            .file_path_by_head_tail("3e", "f722d37b016c63ac0126cfdcec");
        let expected = PathBuf::from(
            "/home/user/.local/share/pnpm/store/v3/files/3e/f722d37b016c63ac0126cfdcec",
        );
        assert_eq!(&received, &expected);
    }

    #[test]
    fn file_path_by_content_address() {
        fn case(file_content: &str, suffix: Option<FileSuffix>, expected: &str) {
            eprintln!("CASE: {file_content:?}, {suffix:?}");
            let store_dir = StoreDir::new("STORE_DIR");
            let file_hash = Sha512::digest(file_content);
            eprintln!("file_hash = {file_hash:x}");
            let received = store_dir.file_path_by_content_address(file_hash, suffix);
            let expected: PathBuf = expected.split('/').collect();
            assert_eq!(&received, &expected);
        }

        case(
            "hello world",
            None,
            "STORE_DIR/v3/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f",
        );

        case(
            "hello world",
            Some(FileSuffix::Exec),
            "STORE_DIR/v3/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f-exec",
        );

        case(
            "hello world",
            Some(FileSuffix::Index),
            "STORE_DIR/v3/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f-index.json",
        );
    }

    #[test]
    fn tmp() {
        let received = StoreDir::new("/home/user/.local/share/pnpm/store").tmp();
        let expected = PathBuf::from("/home/user/.local/share/pnpm/store/v3/tmp");
        assert_eq!(&received, &expected);
    }
}
