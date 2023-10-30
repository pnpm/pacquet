use derive_more::From;
use serde::{Deserialize, Serialize};
use std::path::{self, PathBuf};

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
    pub fn file_path_by_hash_str(&self, head: &str, tail: &str) -> PathBuf {
        self.files().join(head).join(tail)
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

    #[cfg(unix)]
    #[test]
    fn file_path_by_hash_str() {
        let received = "/home/user/.local/share/pnpm/store"
            .pipe(PathBuf::from)
            .pipe(StoreDir::from)
            .file_path_by_hash_str("3e", "f722d37b016c63ac0126cfdcec");
        let expected = PathBuf::from(
            "/home/user/.local/share/pnpm/store/v3/files/3e/f722d37b016c63ac0126cfdcec",
        );
        assert_eq!(&received, &expected);
    }

    #[cfg(unix)]
    #[test]
    fn tmp() {
        let received =
            "/home/user/.local/share/pnpm/store".pipe(PathBuf::from).pipe(StoreDir::from).tmp();
        let expected = PathBuf::from("/home/user/.local/share/pnpm/store/v3/tmp");
        assert_eq!(&received, &expected);
    }
}
