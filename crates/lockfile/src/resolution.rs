use derive_more::{From, TryInto};
use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TarballResolution {
    pub tarball: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct DirectoryResolution {
    pub directory: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GitResolution {
    pub repo: String,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, From, TryInto)]
#[serde(from = "ResolutionSerde", into = "ResolutionSerde")]
pub enum Resolution {
    Tarball(TarballResolution),
    Directory(DirectoryResolution),
    Git(GitResolution),
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct LockfileResolution {
    #[serde(flatten)]
    pub resolution: Resolution,
    pub integrity: String,
}

#[derive(Deserialize, Serialize, From, TryInto)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum TaggedResolution {
    Directory(DirectoryResolution),
    Git(GitResolution),
}

#[derive(Deserialize, Serialize, From, TryInto)]
#[serde(untagged)]
enum ResolutionSerde {
    Tarball(TarballResolution),
    Tagged(TaggedResolution),
}

impl From<ResolutionSerde> for Resolution {
    fn from(value: ResolutionSerde) -> Self {
        match value {
            ResolutionSerde::Tarball(resolution) => resolution.into(),
            ResolutionSerde::Tagged(TaggedResolution::Directory(resolution)) => resolution.into(),
            ResolutionSerde::Tagged(TaggedResolution::Git(resolution)) => resolution.into(),
        }
    }
}

impl From<Resolution> for ResolutionSerde {
    fn from(value: Resolution) -> Self {
        match value {
            Resolution::Tarball(resolution) => resolution.into(),
            Resolution::Directory(resolution) => resolution.pipe(TaggedResolution::from).into(),
            Resolution::Git(resolution) => resolution.pipe(TaggedResolution::from).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn deserialize_tarball_resolution() {
        let yaml = [
            "tarball: file:react-18.2.0.tgz",
            "integrity: sha512-/3IjMdb2L9QbBdWiW5e3P2/npwMBaU9mHCSCUzNln0ZCYbcfTsGbTJrU/kGemdH2IWmB2ioZ+zkxtmq6g09fGQ==",
        ].join("\n");
        let received: LockfileResolution = serde_yaml::from_str(&yaml).unwrap();
        dbg!(&received);
        let expected = LockfileResolution {
            resolution: Resolution::Tarball(TarballResolution {
                tarball: "file:react-18.2.0.tgz".to_string(),
             }),
            integrity: "sha512-/3IjMdb2L9QbBdWiW5e3P2/npwMBaU9mHCSCUzNln0ZCYbcfTsGbTJrU/kGemdH2IWmB2ioZ+zkxtmq6g09fGQ==".to_string()
        };
        assert_eq!(received, expected);
    }

    #[test]
    fn serialize_tarball_resolution() {
        let resolution = LockfileResolution {
            resolution: Resolution::Tarball(TarballResolution {
                tarball: "file:react-18.2.0.tgz".to_string(),
             }),
            integrity: "sha512-/3IjMdb2L9QbBdWiW5e3P2/npwMBaU9mHCSCUzNln0ZCYbcfTsGbTJrU/kGemdH2IWmB2ioZ+zkxtmq6g09fGQ==".to_string()
        };
        let received = serde_yaml::to_string(&resolution).unwrap();
        let received = received.trim();
        eprintln!("RECEIVED:\n{received}");
        let expected = [
            "tarball: file:react-18.2.0.tgz",
            "integrity: sha512-/3IjMdb2L9QbBdWiW5e3P2/npwMBaU9mHCSCUzNln0ZCYbcfTsGbTJrU/kGemdH2IWmB2ioZ+zkxtmq6g09fGQ==",
        ].join("\n");
        assert_eq!(received, expected);
    }
}