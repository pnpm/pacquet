use std::path::Path;
use which::which;

pub async fn ensure_virtual_registry(registry: &str) {
    if let Err(error) = reqwest::Client::new().head(registry).send().await {
        eprintln!("HEAD request to {registry} returned an error");
        eprintln!("Make sure the registry server is operational");
        panic!("{error}");
    };
}

pub fn ensure_git_repo(path: &Path) {
    assert!(path.is_dir());
    assert!(path.join(".git").is_dir());
    assert!(path.join("Cargo.toml").is_file());
    assert!(path.join("Cargo.lock").is_file());
}

pub fn ensure_pnpm() {
    match which("pnpm") {
        Ok(_) => (),
        Err(which::Error::CannotFindBinaryPath) => panic!("Cannot find pnpm in $PATH"),
        Err(error) => panic!("{error}"),
    }
}
