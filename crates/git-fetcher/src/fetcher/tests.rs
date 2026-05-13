use super::{GitFetcher, exec_git, extract_host, should_use_shallow};
use crate::error::GitFetcherError;
use pacquet_executor::ScriptsPrependNodePath;
use pacquet_reporter::SilentReporter;
use pacquet_store_dir::StoreDir;
use std::{fs, path::Path, path::PathBuf};
use tempfile::tempdir;

fn skip_if_no_git() -> bool {
    let probe = std::process::Command::new("git").arg("--version").output();
    if probe.is_err() {
        eprintln!("skipping: `git` not on PATH");
        return true;
    }
    false
}

/// Create a tiny bare git repo whose single commit ships a
/// `package.json` and `index.js`. Returns `(bare_repo_path,
/// commit_sha)`. The caller passes the bare-path as the fetcher's
/// `repo` (with a `file://` URL prefix so `extract_host` sees it as
/// non-shallow-eligible).
fn make_bare_repo(tmp: &Path) -> (PathBuf, String) {
    let work = tmp.join("work");
    let bare = tmp.join("repo.git");
    fs::create_dir_all(&work).unwrap();

    exec_git(&["init", "-q", "-b", "main"], Some(&work)).unwrap();
    exec_git(&["config", "user.email", "test@example.invalid"], Some(&work)).unwrap();
    exec_git(&["config", "user.name", "Test"], Some(&work)).unwrap();
    fs::write(work.join("package.json"), r#"{"name":"pkg","version":"1.0.0","main":"index.js"}"#)
        .unwrap();
    fs::write(work.join("index.js"), "module.exports = 42;\n").unwrap();
    exec_git(&["add", "-A"], Some(&work)).unwrap();
    // `-c commit.gpgsign=false` neutralises a user-global `gpgsign=true`
    // setting that would otherwise demand a real signing key in CI.
    exec_git(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"], Some(&work)).unwrap();
    let commit = exec_git(&["rev-parse", "HEAD"], Some(&work)).unwrap().trim().to_string();
    exec_git(&["clone", "--bare", "-q", &work.to_string_lossy(), &bare.to_string_lossy()], None)
        .unwrap();
    (bare, commit)
}

fn deny_all_builds<'a>() -> &'a (dyn Fn(&str, &str) -> bool + Send + Sync) {
    &|_, _| false
}

#[test]
fn should_use_shallow_returns_false_for_empty_host_list() {
    assert!(!should_use_shallow("https://github.com/x/y.git", &[]));
}

#[test]
fn should_use_shallow_matches_known_host() {
    let hosts = vec!["github.com".to_string(), "gitlab.com".to_string()];
    assert!(should_use_shallow("https://github.com/x/y.git", &hosts));
    assert!(should_use_shallow("git+ssh://git@github.com/x/y.git", &hosts));
    assert!(!should_use_shallow("https://example.com/x/y.git", &hosts));
}

#[test]
fn extract_host_handles_user_authority_and_port() {
    assert_eq!(extract_host("https://github.com/foo/bar"), Some("github.com"));
    assert_eq!(extract_host("git+ssh://git@github.com/foo/bar.git"), Some("github.com"));
    assert_eq!(extract_host("https://host.example:443/foo"), Some("host.example"));
    assert_eq!(extract_host("file:///tmp/x"), None);
    assert_eq!(extract_host("relative/path"), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn fetcher_imports_package_into_cas() {
    if skip_if_no_git() {
        return;
    }
    let tmp = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(tmp.path());
    let store_root = tempdir().unwrap();
    let store_dir = StoreDir::from(store_root.path().to_path_buf());

    let repo_url = format!("file://{}", bare.display());
    let received = GitFetcher {
        repo: &repo_url,
        commit: &commit,
        path: None,
        git_shallow_hosts: &[],
        allow_build: deny_all_builds(),
        ignore_scripts: false,
        unsafe_perm: true,
        user_agent: None,
        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
        script_shell: None,
        node_execpath: None,
        npm_execpath: None,
        store_dir: &store_dir,
        package_id: "pkg@1.0.0",
        requester: "/test",
        store_index_writer: None,
        files_index_file: "pkg@1.0.0\tbuilt",
    }
    .run::<SilentReporter>()
    .await
    .unwrap();

    assert!(!received.built, "package without scripts should not be 'built'");
    assert!(received.cas_paths.contains_key("package.json"));
    assert!(received.cas_paths.contains_key("index.js"));
    let cas_path = &received.cas_paths["package.json"];
    assert!(cas_path.exists(), "CAS entry must exist on disk");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetcher_rejects_commit_mismatch() {
    if skip_if_no_git() {
        return;
    }
    let tmp = tempdir().unwrap();
    let (bare, _commit) = make_bare_repo(tmp.path());
    let store_root = tempdir().unwrap();
    let store_dir = StoreDir::from(store_root.path().to_path_buf());

    let repo_url = format!("file://{}", bare.display());
    // A SHA that doesn't exist in the repo — `git checkout` will fail
    // before we even reach `rev-parse`, producing a `GitExec` rather
    // than `CheckoutMismatch`. Either path is a hard failure, which is
    // the contract we care about: never silently install a wrong
    // commit.
    let bogus = "0000000000000000000000000000000000000000";
    let err = GitFetcher {
        repo: &repo_url,
        commit: bogus,
        path: None,
        git_shallow_hosts: &[],
        allow_build: deny_all_builds(),
        ignore_scripts: false,
        unsafe_perm: true,
        user_agent: None,
        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
        script_shell: None,
        node_execpath: None,
        npm_execpath: None,
        store_dir: &store_dir,
        package_id: "pkg@1.0.0",
        requester: "/test",
        store_index_writer: None,
        files_index_file: "pkg@1.0.0\tbuilt",
    }
    .run::<SilentReporter>()
    .await
    .unwrap_err();

    assert!(
        matches!(err, GitFetcherError::GitExec { .. } | GitFetcherError::CheckoutMismatch { .. }),
        "expected GitExec or CheckoutMismatch, got {err:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetcher_blocks_build_when_not_allowed() {
    if skip_if_no_git() {
        return;
    }
    let tmp = tempdir().unwrap();
    // A repo whose manifest declares a `prepare` script — exercises
    // the `allow_build` gate without actually spawning the script
    // (the policy is denying-all here).
    let work = tmp.path().join("work");
    let bare = tmp.path().join("repo.git");
    fs::create_dir_all(&work).unwrap();
    exec_git(&["init", "-q", "-b", "main"], Some(&work)).unwrap();
    exec_git(&["config", "user.email", "test@example.invalid"], Some(&work)).unwrap();
    exec_git(&["config", "user.name", "Test"], Some(&work)).unwrap();
    fs::write(
        work.join("package.json"),
        r#"{"name":"naughty","version":"2.0.0","main":"index.js","scripts":{"prepare":"tsc"}}"#,
    )
    .unwrap();
    fs::write(work.join("index.js"), "module.exports = 1;\n").unwrap();
    exec_git(&["add", "-A"], Some(&work)).unwrap();
    // `-c commit.gpgsign=false` neutralises a user-global `gpgsign=true`
    // setting that would otherwise demand a real signing key in CI.
    exec_git(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"], Some(&work)).unwrap();
    let commit = exec_git(&["rev-parse", "HEAD"], Some(&work)).unwrap().trim().to_string();
    exec_git(&["clone", "--bare", "-q", &work.to_string_lossy(), &bare.to_string_lossy()], None)
        .unwrap();

    let store_root = tempdir().unwrap();
    let store_dir = StoreDir::from(store_root.path().to_path_buf());
    let repo_url = format!("file://{}", bare.display());
    let err = GitFetcher {
        repo: &repo_url,
        commit: &commit,
        path: None,
        git_shallow_hosts: &[],
        allow_build: deny_all_builds(),
        ignore_scripts: false,
        unsafe_perm: true,
        user_agent: None,
        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
        script_shell: None,
        node_execpath: None,
        npm_execpath: None,
        store_dir: &store_dir,
        package_id: "naughty@2.0.0",
        requester: "/test",
        store_index_writer: None,
        files_index_file: "naughty@2.0.0\tbuilt",
    }
    .run::<SilentReporter>()
    .await
    .unwrap_err();

    match err {
        GitFetcherError::Prepare(crate::error::PreparePackageError::NotAllowed {
            name,
            version,
        }) => {
            assert_eq!(name, "naughty");
            assert_eq!(version, "2.0.0");
        }
        other => panic!("expected Prepare::NotAllowed, got {other:?}"),
    }
}
