use derive_more::{From, TryInto};
use serde::{Deserialize, Serialize};

macro_rules! tag {
    ($name:ident = $value:literal) => {
        #[derive(Default, Clone, Eq, PartialEq, Deserialize, Serialize)]
        #[serde(try_from = "&'de str", into = "&str")]
        struct $name;

        impl<'a> TryFrom<&'a str> for $name {
            type Error = &'a str;
            fn try_from(value: &'a str) -> Result<Self, Self::Error> {
                (value == $value).then_some($name).ok_or(value)
            }
        }

        impl From<$name> for &'static str {
            fn from(_: $name) -> Self {
                $value
            }
        }
    };
}

macro_rules! middle {
    ($wrapper:ident for $tag:ty, $target:ident) => {
        #[derive(Deserialize, Serialize)]
        struct $wrapper {
            #[serde(rename = "type")]
            tag: $tag,
            #[serde(flatten)]
            value: $target,
        }

        impl From<$wrapper> for $target {
            fn from(value: $wrapper) -> Self {
                value.value
            }
        }

        impl From<$target> for $wrapper {
            fn from(value: $target) -> Self {
                $wrapper { tag: <$tag>::default(), value }
            }
        }
    };
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TarballResolution {
    pub tarball: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
}

tag!(DirectoryResolutionTag = "directory");
middle!(DirectoryResolutionSerde for DirectoryResolutionTag, DirectoryResolution);
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(from = "DirectoryResolutionSerde", into = "DirectoryResolutionSerde")]
pub struct DirectoryResolution {
    pub directory: String,
}

tag!(GitResolutionTag = "git");
middle!(GitResolutionSerde for GitResolutionTag, GitResolution);
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(from = "GitResolutionSerde", into = "GitResolutionSerde")]
pub struct GitResolution {
    pub repo: String,
    pub commit: String,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, From, TryInto)]
#[serde(untagged)]
pub enum LockfileResolution {
    Tarball(TarballResolution),
    // Directory(DirectoryResolution),
    // Git(GitResolution),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn deserialize_tarball_resolution() {
        eprintln!("CASE: without integrity");
        let yaml = ["tarball: file:react-18.2.0.tgz"].join("\n");
        let received: LockfileResolution = serde_yaml::from_str(&yaml).unwrap();
        dbg!(&received);
        let expected = LockfileResolution::Tarball(TarballResolution {
            tarball: "file:react-18.2.0.tgz".to_string(),
            integrity: None,
        });
        assert_eq!(received, expected);

        eprintln!("CASE: with integrity");
        let yaml = [
            "tarball: file:react-18.2.0.tgz",
            "integrity: sha512-/3IjMdb2L9QbBdWiW5e3P2/npwMBaU9mHCSCUzNln0ZCYbcfTsGbTJrU/kGemdH2IWmB2ioZ+zkxtmq6g09fGQ==",
        ].join("\n");
        let received: LockfileResolution = serde_yaml::from_str(&yaml).unwrap();
        dbg!(&received);
        let expected = LockfileResolution::Tarball(TarballResolution {
            tarball: "file:react-18.2.0.tgz".to_string(),
            integrity: "sha512-/3IjMdb2L9QbBdWiW5e3P2/npwMBaU9mHCSCUzNln0ZCYbcfTsGbTJrU/kGemdH2IWmB2ioZ+zkxtmq6g09fGQ==".to_string().into()
        });
        assert_eq!(received, expected);
    }

    #[test]
    fn serialize_tarball_resolution() {
        eprintln!("CASE: without integrity");
        let resolution = LockfileResolution::Tarball(TarballResolution {
            tarball: "file:react-18.2.0.tgz".to_string(),
            integrity: None,
        });
        let received = serde_yaml::to_string(&resolution).unwrap();
        let received = received.trim();
        eprintln!("RECEIVED:\n{received}");
        let expected = ["tarball: file:react-18.2.0.tgz"].join("\n");
        assert_eq!(received, expected);
    }
}
