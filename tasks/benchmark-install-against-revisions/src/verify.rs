use std::path::Path;

pub async fn ensure_virtual_registry(registry: &str) {
    reqwest::Client::new().head(registry).send().await.expect("local registry is set up");
}

pub fn ensure_git_repo(path: &Path) {
    assert!(path.is_dir());
    assert!(path.join(".git").is_dir());
    assert!(path.join("Cargo.toml").is_file());
    assert!(path.join("Cargo.lock").is_file());
}
