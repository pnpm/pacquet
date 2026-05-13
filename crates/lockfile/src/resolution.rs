use derive_more::{From, TryInto};
use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};
use ssri::Integrity;
use std::collections::BTreeMap;

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
    /// Sub-directory inside the tarball to pack, mirroring
    /// `GitResolution.path`. Pnpm's git-hosted tarball fetcher uses it
    /// to package only one directory of a monorepo's archive. Mirrors
    /// pnpm's `TarballResolution.path` at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/types/src/index.ts#L93>.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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

/// One of the named executables a [`BinaryResolution`] exposes. Pnpm
/// writes either a single string (one binary, named after the
/// package) or a map of `{ bin_name -> path_inside_archive }` so a
/// runtime archive can expose several launchers (e.g. `node` and
/// `node-mips`). Mirrors pnpm's
/// [`BinaryResolution.bin`](https://github.com/pnpm/pnpm/blob/94240bc046/resolving/resolver-base/src/index.ts#L46-L48)
/// type union.
///
/// `BTreeMap` (not `HashMap`) keeps the serialised order stable so a
/// round-trip through pacquet doesn't churn the lockfile diff.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BinarySpec {
    /// Single executable. The bin name defaults to the package name
    /// at install time; this string is the path *inside the archive*
    /// to the executable.
    Single(String),
    /// Named map of `bin_name -> path_inside_archive`.
    Map(BTreeMap<String, String>),
}

/// Archive format for a [`BinaryResolution`].
///
/// Mirrors pnpm's `BinaryResolution.archive` discriminator at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/resolving/resolver-base/src/index.ts#L47>.
/// `tarball` is the common shape for nodejs.org's `.tar.gz` artifacts
/// (Linux / macOS); `zip` is what Windows Node ships as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BinaryArchive {
    Tarball,
    Zip,
}

/// For a downloaded binary archive (a JavaScript runtime: Node, Deno,
/// or Bun). Mirrors pnpm's
/// [`BinaryResolution`](https://github.com/pnpm/pnpm/blob/94240bc046/resolving/resolver-base/src/index.ts#L41-L49).
///
/// The install path extracts the archive into the CAS (with optional
/// per-package `ignoreFilePattern` filtering — Node strips bundled
/// `npm` / `corepack`) and links the executables named in `bin` into
/// the importer's `node_modules/.bin/`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BinaryResolution {
    pub url: String,
    pub integrity: Integrity,
    pub bin: BinarySpec,
    pub archive: BinaryArchive,
    /// Basename of the archive's top-level directory (e.g.
    /// `node-v22.0.0-darwin-arm64`). Only emitted for zip archives —
    /// see
    /// [`engine/runtime/node-resolver/src/index.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/engine/runtime/node-resolver/src/index.ts)
    /// where the resolver sets `resolution.prefix = address.basename`
    /// only for the `.zip` branch. The zip extractor strips this
    /// prefix when applying `ignoreFilePattern` and renames the
    /// resulting `<temp>/<basename>/` directory to the CAS target.
    /// Tarball entries already carry the prefix in their tar header,
    /// so this stays `None` for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// One `(os, cpu, libc?)` triple a [`PlatformAssetResolution`] covers.
/// Mirrors pnpm's
/// [`PlatformAssetTarget`](https://github.com/pnpm/pnpm/blob/94240bc046/resolving/resolver-base/src/index.ts#L60-L64).
///
/// Pnpm only writes `libc` for musl-built variants; glibc is the
/// implicit default on Linux and the field is omitted everywhere
/// else. `Option<String>` (rather than `Option<Libc>` enum) keeps
/// future libc values future-compatible without a churning serde
/// migration if upstream adds one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlatformAssetTarget {
    pub os: String,
    pub cpu: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub libc: Option<String>,
}

/// One variant of a [`VariationsResolution`]: an inner [`LockfileResolution`]
/// paired with the host triples it covers. Mirrors pnpm's
/// [`PlatformAssetResolution`](https://github.com/pnpm/pnpm/blob/94240bc046/resolving/resolver-base/src/index.ts#L66-L69).
///
/// The inner resolution is *atomic* upstream — a `BinaryResolution`,
/// `TarballResolution`, etc. — never another `VariationsResolution`.
/// Pacquet keeps it typed as the full `LockfileResolution` for
/// serde-round-trip uniformity; the variant picker checks at runtime
/// that the resolved inner is atomic.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlatformAssetResolution {
    pub resolution: LockfileResolution,
    pub targets: Vec<PlatformAssetTarget>,
}

/// For a runtime (or any platform-conditioned binary) that has more
/// than one downloadable artifact, one per `(os, cpu, libc?)` combo.
/// Mirrors pnpm's
/// [`VariationsResolution`](https://github.com/pnpm/pnpm/blob/94240bc046/resolving/resolver-base/src/index.ts#L73-L76).
///
/// At install time, the dispatcher walks `variants` in declaration
/// order and picks the first whose `targets[]` includes the host
/// triple — see `pick_variant` in `pacquet-package-manager`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VariationsResolution {
    pub variants: Vec<PlatformAssetResolution>,
}

/// Represent the resolution object.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, From, TryInto)]
#[serde(from = "ResolutionSerde", into = "ResolutionSerde")]
pub enum LockfileResolution {
    Tarball(TarballResolution),
    Registry(RegistryResolution),
    Directory(DirectoryResolution),
    Git(GitResolution),
    Binary(BinaryResolution),
    Variations(VariationsResolution),
}

impl LockfileResolution {
    /// Get the integrity field if available.
    pub fn integrity(&self) -> Option<&'_ Integrity> {
        match self {
            LockfileResolution::Tarball(resolution) => resolution.integrity.as_ref(),
            LockfileResolution::Registry(resolution) => Some(&resolution.integrity),
            LockfileResolution::Binary(resolution) => Some(&resolution.integrity),
            // Directory / Git resolutions have no integrity.
            // Variations is a meta-shape — the integrity lives on the
            // picked variant's inner resolution, so callers must
            // resolve through `pick_variant` first.
            LockfileResolution::Directory(_)
            | LockfileResolution::Git(_)
            | LockfileResolution::Variations(_) => None,
        }
    }
}

/// Intermediate helper type for serde.
#[derive(Deserialize, Serialize, From, TryInto)]
#[serde(tag = "type", rename_all = "camelCase")]
enum TaggedResolution {
    Directory(DirectoryResolution),
    Git(GitResolution),
    Binary(BinaryResolution),
    Variations(VariationsResolution),
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
            ResolutionSerde::Tagged(TaggedResolution::Binary(resolution)) => resolution.into(),
            ResolutionSerde::Tagged(TaggedResolution::Variations(resolution)) => resolution.into(),
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
            LockfileResolution::Binary(resolution) => {
                resolution.pipe(TaggedResolution::from).into()
            }
            LockfileResolution::Variations(resolution) => {
                resolution.pipe(TaggedResolution::from).into()
            }
        }
    }
}

#[cfg(test)]
mod tests;
