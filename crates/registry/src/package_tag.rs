use derive_more::{Display, From, TryInto};
use node_semver::{SemverError, Version};
use std::str::FromStr;

/// Version or tag that is attachable to a registry URL.
#[derive(Debug, Display, From, TryInto)]
pub enum PackageTag {
    /// Literally `latest`.
    #[display("latest")]
    Latest,
    /// Pinned version.
    Version(Version),
    /// Named distribution tag (for example `next` or `beta`).
    #[display("{_0}")]
    Tag(String),
}

impl FromStr for PackageTag {
    type Err = SemverError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "latest" {
            Ok(PackageTag::Latest)
        } else {
            value.parse::<Version>().map(PackageTag::Version)
        }
    }
}
