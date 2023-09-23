use derive_more::{From, TryInto};
use serde::{Deserialize, Serialize};

macro_rules! tag {
    ($name:ident = $value:literal) => {
        #[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
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

middle!(TarballResolutionSerde for Option<()>, TarballResolution);

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(from = "TarballResolutionSerde", into = "TarballResolutionSerde")]
pub struct TarballResolution {
    pub tarball: String,
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

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct IntegrityResolution {
    pub integrity: String,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, From, TryInto)]
#[serde(untagged)]
pub enum LockfileResolution {
    Tarball(TarballResolution),
    Directory(DirectoryResolution),
    Git(GitResolution),
    Integrity(IntegrityResolution),
}
