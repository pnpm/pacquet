use crate::Lockfile;
use derive_more::{Display, Error};
use pacquet_diagnostics::miette::{self, Diagnostic};
use std::{env, fs, io, path::Path};

/// Error when writing the lockfile to the filesystem.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum SaveLockfileError {
    #[display("Failed to get current_dir: {_0}")]
    #[diagnostic(code(pacquet_lockfile::current_dir))]
    CurrentDir(io::Error),

    #[display("Failed to serialize lockfile to YAML: {_0}")]
    #[diagnostic(code(pacquet_lockfile::serialize_yaml))]
    SerializeYaml(serde_yaml::Error),

    #[display("Failed to write lockfile content: {_0}")]
    #[diagnostic(code(pacquet_lockfile::write_file))]
    WriteFile(io::Error),
}

impl Lockfile {
    /// Save lockfile to a specific path.
    pub fn save_to_path(&self, path: &Path) -> Result<(), SaveLockfileError> {
        let content = serde_yaml::to_string(self).map_err(SaveLockfileError::SerializeYaml)?;
        fs::write(path, content).map_err(SaveLockfileError::WriteFile)
    }

    /// Save lockfile to `pnpm-lock.yaml` in the current directory.
    pub fn save_to_current_dir(&self) -> Result<(), SaveLockfileError> {
        let file_path =
            env::current_dir().map_err(SaveLockfileError::CurrentDir)?.join(Lockfile::FILE_NAME);
        self.save_to_path(&file_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;
    use text_block_macros::text_block;

    /// A compact lockfile fixture exercising the root settings, the three
    /// direct-dependency groups, and the `packages` map with both tarball and
    /// registry resolutions.
    const LOCKFILE_YAML: &str = text_block! {
        "lockfileVersion: '6.0'"
        ""
        "settings:"
        "  autoInstallPeers: false"
        "  excludeLinksFromLockfile: false"
        ""
        "dependencies:"
        "  react:"
        "    specifier: ^17.0.2"
        "    version: 17.0.2"
        "  react-dom:"
        "    specifier: ^17.0.2"
        "    version: 17.0.2(react@17.0.2)"
        ""
        "devDependencies:"
        "  typescript:"
        "    specifier: ^5.1.6"
        "    version: 5.1.6"
        ""
        "packages:"
        ""
        "  /react@17.0.2:"
        "    resolution: {integrity: sha512-TIE61hcgbI/SlJh/0c1sT1SZbBlpg7WiZcs65WPJhoIZQPhH1SCpcGA7LgrVXT15lwN3HV4GQM/MJ9aKEn3Qfg==}"
        "    engines: {node: '>=0.10.0'}"
        "    dev: false"
        ""
        "  /react-dom@17.0.2(react@17.0.2):"
        "    resolution: {integrity: sha512-s4h96KtLDUQlsENhMn1ar8t2bEa+q/YAtj8pPPdIjPDGBDIVNsrD9aXNWqspUe6AzKCIG0C1HZZLqLV7qpOBGA==}"
        "    peerDependencies:"
        "      react: 17.0.2"
        "    dependencies:"
        "      react: 17.0.2"
        "    dev: false"
        ""
        "  /typescript@5.1.6:"
        "    resolution: {integrity: sha512-zaWCozRZ6DLEWAWFrVDz1H6FVXzUSfTy5FUMWsQlU8Ym5JP9eO4xkTIROFCQvhQf61z6O/G6ugw3SgAnvvm+HA==}"
        "    engines: {node: '>=14.17'}"
        "    hasBin: true"
        "    dev: true"
    };

    #[test]
    fn round_trip_parse_save_parse_preserves_lockfile() {
        let original: Lockfile =
            serde_yaml::from_str(LOCKFILE_YAML).expect("parse fixture lockfile");

        let tmp = tempdir().expect("create tempdir");
        let path = tmp.path().join("pnpm-lock.yaml");
        original.save_to_path(&path).expect("save lockfile");

        let saved_bytes = std::fs::read_to_string(&path).expect("read saved lockfile");
        let reparsed: Lockfile = serde_yaml::from_str(&saved_bytes).expect("reparse lockfile");

        assert_eq!(original, reparsed);
    }

    #[test]
    fn save_fails_with_wrapped_io_error_when_path_is_invalid() {
        let empty_lockfile: Lockfile =
            serde_yaml::from_str("lockfileVersion: '6.0'\n").expect("parse minimal lockfile");

        // Attempt to write under a non-existent directory; fs::write returns NotFound.
        let bad_path = std::path::Path::new("/nonexistent-pacquet-dir/pnpm-lock.yaml");
        let err = empty_lockfile.save_to_path(bad_path).expect_err("should fail");
        assert!(matches!(err, SaveLockfileError::WriteFile(_)));
    }
}
