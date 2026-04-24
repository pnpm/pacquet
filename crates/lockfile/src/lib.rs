mod comver;
mod load_lockfile;
mod lockfile_version;
mod package_metadata;
mod pkg_name;
mod pkg_name_suffix;
mod pkg_name_ver;
mod pkg_name_ver_peer;
mod pkg_ver_peer;
mod project_snapshot;
mod resolution;
mod resolved_dependency;
mod save_lockfile;
mod snapshot_dep_ref;
mod snapshot_entry;

pub use comver::*;
pub use load_lockfile::*;
pub use lockfile_version::*;
pub use package_metadata::*;
pub use pkg_name::*;
pub use pkg_name_suffix::*;
pub use pkg_name_ver::*;
pub use pkg_name_ver_peer::*;
pub use pkg_ver_peer::*;
pub use project_snapshot::*;
pub use resolution::*;
pub use resolved_dependency::*;
pub use save_lockfile::*;
pub use snapshot_dep_ref::*;
pub use snapshot_entry::*;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Package key used by the `packages:` and `snapshots:` maps in a v9 lockfile.
///
/// Example: `react-dom@17.0.2(react@17.0.2)`.
pub type PackageKey = PkgNameVerPeer;

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockfileSettings {
    pub auto_install_peers: bool,
    pub exclude_links_from_lockfile: bool,
}

/// A pnpm v9 lockfile.
///
/// Specification: <https://github.com/pnpm/spec/blob/master/lockfile/9.0.md>
/// Reference: <https://github.com/pnpm/pnpm/blob/main/lockfile/lockfile-types/src/index.ts>
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub lockfile_version: LockfileVersion<9>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<LockfileSettings>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<HashMap<String, String>>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub importers: HashMap<String, ProjectSnapshot>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub packages: Option<HashMap<PackageKey, PackageMetadata>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<HashMap<PackageKey, SnapshotEntry>>,
}

impl Lockfile {
    /// Base file name of the lockfile.
    pub const FILE_NAME: &str = "pnpm-lock.yaml";

    /// The key used to refer to the root project inside `importers`.
    pub const ROOT_IMPORTER_KEY: &str = ".";

    /// Convenience accessor for the root project's snapshot.
    pub fn root_project(&self) -> Option<&'_ ProjectSnapshot> {
        self.importers.get(Lockfile::ROOT_IMPORTER_KEY)
    }
}
