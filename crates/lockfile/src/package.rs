use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::LockfilePeerDependencyMeta;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct LockfilePackageResolution {
    integrity: String,
}

// Reference: https://github.com/pnpm/pnpm/blob/main/lockfile/lockfile-file/src/sortLockfileKeys.ts#L5
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct LockfilePackage {
    resolution: LockfilePackageResolution,
    id: Option<String>,

    name: Option<String>,
    version: Option<String>,

    engines: Option<HashMap<String, String>>,
    cpu: Option<Vec<String>>,
    os: Option<Vec<String>>,
    // TODO: Add `libc`
    deprecated: Option<bool>,
    #[serde(alias = "hasBin")]
    has_bin: Option<bool>,
    // TODO: Add `prepare`
    #[serde(alias = "requiresBuild")]
    requires_build: Option<bool>,

    // TODO: Add `bundleDependencies`
    #[serde(alias = "peerDependencies")]
    peer_dependencies: Option<HashMap<String, String>>,
    #[serde(alias = "peerDependenciesMeta")]
    peer_dependencies_meta: Option<HashMap<String, LockfilePeerDependencyMeta>>,

    dependencies: Option<HashMap<String, String>>,
    optional_dependencies: Option<HashMap<String, String>>,

    #[serde(alias = "transitivePeerDependencies")]
    transitive_peer_dependencies: Option<Vec<String>>,
    dev: bool,
    optional: Option<bool>,
}
