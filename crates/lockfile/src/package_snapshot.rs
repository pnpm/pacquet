use crate::{LockfileResolution, PackageSnapshotDependency};
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
    pub resolution: LockfileResolution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    pub name: Option<String>,
    pub version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub engines: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<Vec<String>>,
    // TODO: Add `libc`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_bin: Option<bool>,
    // TODO: Add `prepare`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_build: Option<bool>,

    // TODO: Add `bundleDependencies`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_dependencies: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_dependencies_meta: Option<HashMap<String, LockfilePeerDependencyMetaValue>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<HashMap<String, PackageSnapshotDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_dependencies: Option<HashMap<String, String>>,

    pub transitive_peer_dependencies: Option<Vec<String>>,
    pub dev: bool,
    pub optional: Option<bool>,
}
