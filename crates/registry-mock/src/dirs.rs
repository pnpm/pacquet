use pipe_trait::Pipe;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

static WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();
pub fn workspace_root() -> &'static Path {
    WORKSPACE_ROOT.get_or_init(|| {
        let output = env!("CARGO")
            .pipe(Command::new)
            .arg("locate-project")
            .arg("--workspace")
            .arg("--message-format=plain")
            .output()
            .expect("cargo locate-project");
        assert!(
            output.status.success(),
            "Command `cargo locate-project` exits with non-zero status code"
        );
        output
            .stdout
            .pipe(String::from_utf8)
            .expect("convert stdout to UTF-8")
            .trim_end()
            .pipe(PathBuf::from)
            .parent()
            .expect("parent of root manifest")
            .to_path_buf()
    })
}

static REGISTRY_MOCK: OnceLock<PathBuf> = OnceLock::new();
pub fn registry_mock() -> &'static Path {
    REGISTRY_MOCK.get_or_init(|| workspace_root().join("crates").join("registry-mock"))
}
