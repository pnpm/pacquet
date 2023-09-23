use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{LockfilePeerDependencyMeta, LockfileResolution};

// Reference: https://github.com/pnpm/pnpm/blob/main/lockfile/lockfile-file/src/sortLockfileKeys.ts#L5
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageSnapshot {
    resolution: LockfileResolution,
    id: Option<String>,

    name: Option<String>,
    version: Option<String>,

    engines: Option<HashMap<String, String>>,
    cpu: Option<Vec<String>>,
    os: Option<Vec<String>>,
    // TODO: Add `libc`
    deprecated: Option<bool>,
    has_bin: Option<bool>,
    // TODO: Add `prepare`
    requires_build: Option<bool>,

    // TODO: Add `bundleDependencies`
    peer_dependencies: Option<HashMap<String, String>>,
    peer_dependencies_meta: Option<HashMap<String, LockfilePeerDependencyMeta>>,

    dependencies: Option<HashMap<String, String>>,
    optional_dependencies: Option<HashMap<String, String>>,

    transitive_peer_dependencies: Option<Vec<String>>,
    dev: bool,
    optional: Option<bool>,
}
