use derive_more::{From, TryInto};
use serde::{Deserialize, Serialize};

macro_rules! tag {
    ($(#[$attributes:meta])* $name:ident = $value:literal) => {
        $(#[$attributes])*
        #[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
        #[serde(try_from = "&'de str", into = "&str")]
        struct $name;

        impl $name {
            /// Name of the tag.
            pub const NAME: &str = $value;
        }

        impl<'a> TryFrom<&'a str> for $name {
            type Error = &'a str;
            fn try_from(value: &'a str) -> Result<Self, Self::Error> {
                (value == Self::NAME).then_some($name).ok_or(value)
            }
        }

        impl From<$name> for &'static str {
            fn from(_: $name) -> Self {
                $name::NAME
            }
        }
    };
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct TarballResolution {
    #[serde(rename = "type")]
    tag: Option<()>,
    pub tarball: String,
    pub integrity: Option<String>,
}

tag! {
    /// Tag of [`DirectoryResolution`].
    DirectoryResolutionTag = "directory"
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct DirectoryResolution {
    #[serde(rename = "type")]
    tag: DirectoryResolutionTag,
    pub directory: String,
}

tag! {
    /// Tag of [`GitRepositoryResolution`].
    GitResolutionTag = "git"
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct GitResolution {
    #[serde(rename = "type")]
    tag: GitResolutionTag,
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
