use derive_more::{From, TryInto};
use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};
use ssri::Integrity;

/// For tarball hosted remotely or locally.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TarballResolution {
    pub tarball: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity: Option<Integrity>,
}

/// For standard package specification, with package name and version range.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RegistryResolution {
    pub integrity: Integrity,
}

/// For local directory on a filesystem.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DirectoryResolution {
    pub directory: String,
}

/// For git repository.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitResolution {
    pub repo: String,
    pub commit: String,
}

/// Represent the resolution object.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, From, TryInto)]
#[serde(from = "ResolutionSerde", into = "ResolutionSerde")]
pub enum LockfileResolution {
    Tarball(TarballResolution),
    Registry(RegistryResolution),
    Directory(DirectoryResolution),
    Git(GitResolution),
}

impl LockfileResolution {
    /// Get the integrity field if available.
    pub fn integrity(&self) -> Option<&'_ Integrity> {
        match self {
            LockfileResolution::Tarball(resolution) => resolution.integrity.as_ref(),
            LockfileResolution::Registry(resolution) => resolution.integrity.pipe_ref(Some),
            LockfileResolution::Directory(_) | LockfileResolution::Git(_) => None,
        }
    }
}

/// Intermediate helper type for serde.
#[derive(Deserialize, Serialize, From, TryInto)]
#[serde(tag = "type", rename_all = "camelCase")]
enum TaggedResolution {
    Directory(DirectoryResolution),
    Git(GitResolution),
}

/// Intermediate helper type for serde.
#[derive(Deserialize, Serialize, From, TryInto)]
#[serde(untagged)]
enum ResolutionSerde {
    Tarball(TarballResolution),
    Registry(RegistryResolution),
    Tagged(TaggedResolution),
}

impl From<ResolutionSerde> for LockfileResolution {
    fn from(value: ResolutionSerde) -> Self {
        match value {
            ResolutionSerde::Tarball(resolution) => resolution.into(),
            ResolutionSerde::Registry(resolution) => resolution.into(),
            ResolutionSerde::Tagged(TaggedResolution::Directory(resolution)) => resolution.into(),
            ResolutionSerde::Tagged(TaggedResolution::Git(resolution)) => resolution.into(),
        }
    }
}

impl From<LockfileResolution> for ResolutionSerde {
    fn from(value: LockfileResolution) -> Self {
        match value {
            LockfileResolution::Tarball(resolution) => resolution.into(),
            LockfileResolution::Registry(resolution) => resolution.into(),
            LockfileResolution::Directory(resolution) => {
                resolution.pipe(TaggedResolution::from).into()
            }
            LockfileResolution::Git(resolution) => resolution.pipe(TaggedResolution::from).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
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
        let received: LockfileResolution = serde_yaml::from_str(yaml).unwrap();
        dbg!(&received);
        let expected = LockfileResolution::Tarball(TarballResolution {
            tarball: "file:ts-pipe-compose-0.2.1.tgz".to_string(),
            integrity: None,
        });
        assert_eq!(received, expected);

        eprintln!("CASE: with integrity");
        let yaml = text_block! {
            "tarball: file:ts-pipe-compose-0.2.1.tgz"
            "integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
        };
        let received: LockfileResolution = serde_yaml::from_str(yaml).unwrap();
        dbg!(&received);
        let expected = LockfileResolution::Tarball(TarballResolution {
            tarball: "file:ts-pipe-compose-0.2.1.tgz".to_string(),
            integrity: integrity("sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==").into()
        });
        assert_eq!(received, expected);
    }

    #[test]
    fn serialize_tarball_resolution() {
        eprintln!("CASE: without integrity");
        let resolution = LockfileResolution::Tarball(TarballResolution {
            tarball: "file:ts-pipe-compose-0.2.1.tgz".to_string(),
            integrity: None,
        });
        let received = serde_yaml::to_string(&resolution).unwrap();
        let received = received.trim();
        eprintln!("RECEIVED:\n{received}");
        let expected = text_block! {
            "tarball: file:ts-pipe-compose-0.2.1.tgz"
        };
        assert_eq!(received, expected);

        eprintln!("CASE: with integrity");
        let resolution = LockfileResolution::Tarball(TarballResolution {
            tarball: "file:ts-pipe-compose-0.2.1.tgz".to_string(),
            integrity: integrity("sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==").into()
        });
        let received = serde_yaml::to_string(&resolution).unwrap();
        let received = received.trim();
        eprintln!("RECEIVED:\n{received}");
        let expected = text_block! {
            "tarball: file:ts-pipe-compose-0.2.1.tgz"
            "integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
        };
        assert_eq!(received, expected);
    }

    #[test]
    fn deserialize_registry_resolution() {
        let yaml = text_block! {
            "integrity: sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg=="
        };
        let received: LockfileResolution = serde_yaml::from_str(yaml).unwrap();
        dbg!(&received);
        let expected = LockfileResolution::Registry(RegistryResolution {
            integrity: integrity("sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==")
        });
        assert_eq!(received, expected);
    }

    #[test]
    fn serialize_registry_resolution() {
        let resolution = LockfileResolution::Registry(RegistryResolution {
            integrity: integrity("sha512-gf6ZldcfCDyNXPRiW3lQjEP1Z9rrUM/4Cn7BZbv3SdTA82zxWRP8OmLwvGR974uuENhGCFgFdN11z3n1Ofpprg==")
        });
        let received = serde_yaml::to_string(&resolution).unwrap();
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
        let received: LockfileResolution = serde_yaml::from_str(yaml).unwrap();
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
        let received = serde_yaml::to_string(&resolution).unwrap();
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
        let received: LockfileResolution = serde_yaml::from_str(yaml).unwrap();
        dbg!(&received);
        let expected = LockfileResolution::Git(GitResolution {
            repo: "https://github.com/ksxnodemodules/ts-pipe-compose.git".to_string(),
            commit: "e63c09e460269b0c535e4c34debf69bb91d57b22".to_string(),
        });
        assert_eq!(received, expected);
    }

    #[test]
    fn serialize_git_resolution() {
        let resolution = LockfileResolution::Git(GitResolution {
            repo: "https://github.com/ksxnodemodules/ts-pipe-compose.git".to_string(),
            commit: "e63c09e460269b0c535e4c34debf69bb91d57b22".to_string(),
        });
        let received = serde_yaml::to_string(&resolution).unwrap();
        let received = received.trim();
        eprintln!("RECEIVED:\n{received}");
        let expected = text_block! {
            "type: git"
            "repo: https://github.com/ksxnodemodules/ts-pipe-compose.git"
            "commit: e63c09e460269b0c535e4c34debf69bb91d57b22"
        };
        assert_eq!(received, expected);
    }
}
