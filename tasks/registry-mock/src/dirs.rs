use pipe_trait::Pipe;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

pub fn workspace_root() -> &'static Path {
    static WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();
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
        let stdout = String::from_utf8(output.stdout).expect("convert stdout to UTF-8");
        Path::new(stdout.trim_end()).parent().expect("parent of root manifest").to_path_buf()
    })
}

pub fn registry_mock() -> &'static Path {
    static REGISTRY_MOCK: OnceLock<PathBuf> = OnceLock::new();
    REGISTRY_MOCK.get_or_init(|| workspace_root().join("tasks").join("registry-mock"))
}
