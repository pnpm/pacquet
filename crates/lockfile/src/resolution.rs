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
    /// `true` for tarballs sourced from a git host (codeload.github.com /
    /// gitlab.com / bitbucket.org). Such tarballs need preparation
    /// (preparePackage / packlist) on extraction, and their cached content
    /// depends on whether build scripts ran, so they are addressed by a
    /// git-hosted store-index key rather than the integrity-based key.
    ///
    /// The git resolver sets this when it produces the resolution; the
    /// lockfile loader back-fills it on entries whose URL matches a known
    /// git host for backward compatibility with lockfiles written before
    /// this field existed. Mirrors pnpm's `TarballResolution.gitHosted`
    /// at <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/types/src/index.ts#L88-L107>.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_hosted: Option<bool>,
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
    /// Sub-directory inside the cloned tree to package. Mirrors pnpm's
    /// `GitRepositoryResolution.path` at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/types/src/index.ts#L120-L125>.
    /// The git fetcher passes this to `preparePackage` so the build runs
    /// inside the sub-directory rather than the repo root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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
            LockfileResolution::Registry(resolution) => Some(&resolution.integrity),
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
            ResolutionSerde::Tarball(mut resolution) => {
                // Back-fill `gitHosted` for entries written by older pnpm
                // versions that lacked the field. Mirrors upstream's
                // `enrichGitHostedFlag` at
                // <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/fs/src/lockfileFormatConverters.ts#L158-L168>.
                if resolution.git_hosted.is_none() && is_git_hosted_tarball_url(&resolution.tarball)
                {
                    resolution.git_hosted = Some(true);
                }
                resolution.into()
            }
            ResolutionSerde::Registry(resolution) => resolution.into(),
            ResolutionSerde::Tagged(TaggedResolution::Directory(resolution)) => resolution.into(),
            ResolutionSerde::Tagged(TaggedResolution::Git(resolution)) => resolution.into(),
        }
    }
}

/// Best-effort URL-prefix check used to back-fill `gitHosted` on tarball
/// resolutions written by older pnpm versions. Mirrors upstream's
/// `isGitHostedTarballUrl` at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/fs/src/lockfileFormatConverters.ts#L23-L29>.
fn is_git_hosted_tarball_url(url: &str) -> bool {
    (url.starts_with("https://codeload.github.com/")
        || url.starts_with("https://bitbucket.org/")
        || url.starts_with("https://gitlab.com/"))
        && url.contains("tar.gz")
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
mod tests;
