use super::{
    DirectoryResolution, GitResolution, LockfileResolution, RegistryResolution, TarballResolution,
};
use crate::serialize_yaml;
use pretty_assertions::assert_eq;
use ssri::Integrity;
use text_block_macros::text_block;

fn integrity(integrity_str: &str) -> Integrity {
    integrity_str.parse().expect("parse integrity string")
}

#[test]
fn deserialize_tarball_resolution() {
    eprintln!("CASE: without integrity");
    let yaml = text_block! {
        "tarball: file:ts-pipe-compose-0.2.1.tgz"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    dbg!(&received);
    let expected = LockfileResolution::Tarball(TarballResolution {
        tarball: "file:ts-pipe-compose-0.2.1.tgz".to_string(),
        integrity: None,
        git_hosted: None,
    });
    assert_eq!(received, expected);

    eprintln!("CASE: with integrity");
    let yaml = text_block! {
        "tarball: file:ts-pipe-compose-0.2.1.tgz"
        "integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    dbg!(&received);
    let expected = LockfileResolution::Tarball(TarballResolution {
        tarball: "file:ts-pipe-compose-0.2.1.tgz".to_string(),
        integrity: integrity("sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==").into(),
        git_hosted: None,
    });
    assert_eq!(received, expected);
}

#[test]
fn deserialize_tarball_resolution_with_git_hosted() {
    eprintln!("CASE: explicit gitHosted: true");
    let yaml = text_block! {
        "tarball: https://codeload.github.com/foo/bar/tar.gz/abc1234"
        "gitHosted: true"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    dbg!(&received);
    let expected = LockfileResolution::Tarball(TarballResolution {
        tarball: "https://codeload.github.com/foo/bar/tar.gz/abc1234".to_string(),
        integrity: None,
        git_hosted: Some(true),
    });
    assert_eq!(received, expected);
}

#[test]
fn deserialize_tarball_resolution_backfills_git_hosted() {
    // Lockfiles written by older pnpm versions don't carry `gitHosted`; the
    // loader back-fills it for entries whose URL matches a known git host.
    // Mirrors upstream's `enrichGitHostedFlag`.
    eprintln!("CASE: codeload.github.com");
    let yaml = text_block! {
        "tarball: https://codeload.github.com/foo/bar/tar.gz/abc1234"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    dbg!(&received);
    let expected = LockfileResolution::Tarball(TarballResolution {
        tarball: "https://codeload.github.com/foo/bar/tar.gz/abc1234".to_string(),
        integrity: None,
        git_hosted: Some(true),
    });
    assert_eq!(received, expected);

    eprintln!("CASE: gitlab.com archive");
    let yaml = text_block! {
        "tarball: https://gitlab.com/foo/bar/-/archive/abc1234/bar-abc1234.tar.gz"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    let expected = LockfileResolution::Tarball(TarballResolution {
        tarball: "https://gitlab.com/foo/bar/-/archive/abc1234/bar-abc1234.tar.gz".to_string(),
        integrity: None,
        git_hosted: Some(true),
    });
    assert_eq!(received, expected);

    eprintln!("CASE: bitbucket.org archive");
    let yaml = text_block! {
        "tarball: https://bitbucket.org/foo/bar/get/abc1234.tar.gz"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    let expected = LockfileResolution::Tarball(TarballResolution {
        tarball: "https://bitbucket.org/foo/bar/get/abc1234.tar.gz".to_string(),
        integrity: None,
        git_hosted: Some(true),
    });
    assert_eq!(received, expected);

    eprintln!("CASE: registry URL (must not back-fill)");
    let yaml = text_block! {
        "tarball: https://registry.npmjs.org/foo/-/foo-1.0.0.tgz"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    let expected = LockfileResolution::Tarball(TarballResolution {
        tarball: "https://registry.npmjs.org/foo/-/foo-1.0.0.tgz".to_string(),
        integrity: None,
        git_hosted: None,
    });
    assert_eq!(received, expected);

    eprintln!("CASE: github.com without tar.gz (must not back-fill)");
    // Upstream's prefix check requires both the host prefix *and* a `tar.gz`
    // substring — release pages aren't tarballs.
    let yaml = text_block! {
        "tarball: https://codeload.github.com/foo/bar/zip/abc1234"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    let expected = LockfileResolution::Tarball(TarballResolution {
        tarball: "https://codeload.github.com/foo/bar/zip/abc1234".to_string(),
        integrity: None,
        git_hosted: None,
    });
    assert_eq!(received, expected);
}

#[test]
fn serialize_tarball_resolution() {
    eprintln!("CASE: without integrity");
    let resolution = LockfileResolution::Tarball(TarballResolution {
        tarball: "file:ts-pipe-compose-0.2.1.tgz".to_string(),
        integrity: None,
        git_hosted: None,
    });
    let received = serialize_yaml::to_string(&resolution).unwrap();
    let received = received.trim();
    eprintln!("RECEIVED:\n{received}");
    let expected = text_block! {
        "tarball: file:ts-pipe-compose-0.2.1.tgz"
    };
    assert_eq!(received, expected);

    eprintln!("CASE: with integrity");
    let resolution = LockfileResolution::Tarball(TarballResolution {
        tarball: "file:ts-pipe-compose-0.2.1.tgz".to_string(),
        integrity: integrity("sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==").into(),
        git_hosted: None,
    });
    let received = serialize_yaml::to_string(&resolution).unwrap();
    let received = received.trim();
    eprintln!("RECEIVED:\n{received}");
    let expected = text_block! {
        "tarball: file:ts-pipe-compose-0.2.1.tgz"
        "integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
    };
    assert_eq!(received, expected);
}

#[test]
fn serialize_tarball_resolution_with_git_hosted() {
    let resolution = LockfileResolution::Tarball(TarballResolution {
        tarball: "https://codeload.github.com/foo/bar/tar.gz/abc1234".to_string(),
        integrity: integrity("sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==").into(),
        git_hosted: Some(true),
    });
    let received = serialize_yaml::to_string(&resolution).unwrap();
    let received = received.trim();
    eprintln!("RECEIVED:\n{received}");
    let expected = text_block! {
        "tarball: https://codeload.github.com/foo/bar/tar.gz/abc1234"
        "integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
        "gitHosted: true"
    };
    assert_eq!(received, expected);
}

#[test]
fn deserialize_registry_resolution() {
    let yaml = text_block! {
        "integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    dbg!(&received);
    let expected = LockfileResolution::Registry(RegistryResolution {
        integrity: integrity(
            "sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==",
        ),
    });
    assert_eq!(received, expected);
}

#[test]
fn serialize_registry_resolution() {
    let resolution = LockfileResolution::Registry(RegistryResolution {
        integrity: integrity(
            "sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==",
        ),
    });
    let received = serialize_yaml::to_string(&resolution).unwrap();
    let received = received.trim();
    eprintln!("RECEIVED:\n{received}");
    let expected = text_block! {
        "integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
    };
    assert_eq!(received, expected);
}

#[test]
fn deserialize_directory_resolution() {
    let yaml = text_block! {
        "type: directory"
        "directory: ts-pipe-compose-0.2.1/package"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    dbg!(&received);
    let expected = LockfileResolution::Directory(DirectoryResolution {
        directory: "ts-pipe-compose-0.2.1/package".to_string(),
    });
    assert_eq!(received, expected);
}

#[test]
fn serialize_directory_resolution() {
    let resolution = LockfileResolution::Directory(DirectoryResolution {
        directory: "ts-pipe-compose-0.2.1/package".to_string(),
    });
    let received = serialize_yaml::to_string(&resolution).unwrap();
    let received = received.trim();
    eprintln!("RECEIVED:\n{received}");
    let expected = text_block! {
        "type: directory"
        "directory: ts-pipe-compose-0.2.1/package"
    };
    assert_eq!(received, expected);
}

#[test]
fn deserialize_git_resolution() {
    let yaml = text_block! {
        "type: git"
        "repo: https://github.com/ksxnodemodules/ts-pipe-compose.git"
        "commit: e63c09e460269b0c535e4c34debf69bb91d57b22"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    dbg!(&received);
    let expected = LockfileResolution::Git(GitResolution {
        repo: "https://github.com/ksxnodemodules/ts-pipe-compose.git".to_string(),
        commit: "e63c09e460269b0c535e4c34debf69bb91d57b22".to_string(),
        path: None,
    });
    assert_eq!(received, expected);
}

#[test]
fn deserialize_git_resolution_with_path() {
    let yaml = text_block! {
        "type: git"
        "repo: https://github.com/ksxnodemodules/ts-pipe-compose.git"
        "commit: e63c09e460269b0c535e4c34debf69bb91d57b22"
        "path: packages/sub"
    };
    let received: LockfileResolution = serde_saphyr::from_str(yaml).unwrap();
    let expected = LockfileResolution::Git(GitResolution {
        repo: "https://github.com/ksxnodemodules/ts-pipe-compose.git".to_string(),
        commit: "e63c09e460269b0c535e4c34debf69bb91d57b22".to_string(),
        path: Some("packages/sub".to_string()),
    });
    assert_eq!(received, expected);
}

#[test]
fn serialize_git_resolution() {
    let resolution = LockfileResolution::Git(GitResolution {
        repo: "https://github.com/ksxnodemodules/ts-pipe-compose.git".to_string(),
        commit: "e63c09e460269b0c535e4c34debf69bb91d57b22".to_string(),
        path: None,
    });
    let received = serialize_yaml::to_string(&resolution).unwrap();
    let received = received.trim();
    eprintln!("RECEIVED:\n{received}");
    let expected = text_block! {
        "type: git"
        "repo: https://github.com/ksxnodemodules/ts-pipe-compose.git"
        "commit: e63c09e460269b0c535e4c34debf69bb91d57b22"
    };
    assert_eq!(received, expected);
}

#[test]
fn serialize_git_resolution_with_path() {
    let resolution = LockfileResolution::Git(GitResolution {
        repo: "https://github.com/ksxnodemodules/ts-pipe-compose.git".to_string(),
        commit: "e63c09e460269b0c535e4c34debf69bb91d57b22".to_string(),
        path: Some("packages/sub".to_string()),
    });
    let received = serialize_yaml::to_string(&resolution).unwrap();
    let received = received.trim();
    eprintln!("RECEIVED:\n{received}");
    let expected = text_block! {
        "type: git"
        "repo: https://github.com/ksxnodemodules/ts-pipe-compose.git"
        "commit: e63c09e460269b0c535e4c34debf69bb91d57b22"
        "path: packages/sub"
    };
    assert_eq!(received, expected);
}
