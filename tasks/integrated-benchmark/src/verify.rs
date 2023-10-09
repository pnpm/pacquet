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

pub fn ensure_program(program: &str) {
    match which(program) {
        Ok(_) => (),
        Err(which::Error::CannotFindBinaryPath) => panic!("Cannot find {program} in $PATH"),
        Err(error) => panic!("{error}"),
    }
}

pub fn validate_revision_list<List>(list: List)
where
    List: IntoIterator,
    List::Item: AsRef<str>,
{
    for revision in list {
        let revision = revision.as_ref();
        let throw = |reason: &str| {
            eprintln!("Revision {revision:?} is invalid");
            panic!("{reason}");
        };
        if revision.starts_with('.') {
            throw("Revision cannot start with a dot");
        }
        if revision == "PNPM" {
            throw("PNPM is a reserved name");
        }
    }
}
