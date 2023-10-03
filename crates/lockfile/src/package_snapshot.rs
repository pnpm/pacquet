use crate::{LockfileResolution, PackageSnapshotDependency, PkgName};
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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>, // TODO: name and version are required on non-default registry, create a struct for it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>, // TODO: name and version are required on non-default registry, create a struct for it

    #[serde(skip_serializing_if = "Option::is_none")]
    pub engines: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub libc: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_bin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_build: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundled_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_dependencies: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_dependencies_meta: Option<HashMap<String, LockfilePeerDependencyMetaValue>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<HashMap<PkgName, PackageSnapshotDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_dependencies: Option<HashMap<String, String>>,

    pub transitive_peer_dependencies: Option<Vec<String>>,
    pub dev: Option<bool>,
    pub optional: Option<bool>,
}
