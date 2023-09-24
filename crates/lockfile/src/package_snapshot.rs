use crate::LockfileResolution;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct LockfilePeerDependencyMetaValue {
    optional: bool,
}

// Reference: https://github.com/pnpm/pnpm/blob/main/lockfile/lockfile-file/src/sortLockfileKeys.ts#L5
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageSnapshot {
    resolution: LockfileResolution,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,

    name: Option<String>,
    version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    engines: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os: Option<Vec<String>>,
    // TODO: Add `libc`
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_bin: Option<bool>,
    // TODO: Add `prepare`
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_build: Option<bool>,

    // TODO: Add `bundleDependencies`
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_dependencies: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_dependencies_meta: Option<HashMap<String, LockfilePeerDependencyMetaValue>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    dependencies: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    optional_dependencies: Option<HashMap<String, String>>,

    transitive_peer_dependencies: Option<Vec<String>>,
    dev: bool,
    optional: Option<bool>,
}
