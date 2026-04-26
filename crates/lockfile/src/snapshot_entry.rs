use crate::{PkgName, SnapshotDepRef};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-instance snapshot information stored in the v9 `snapshots:` map.
///
/// An entry describes the wiring of one concrete installation of a package:
/// which versions its dependencies were resolved to, plus any optional /
/// transitive-peer metadata needed to recreate the install.
///
/// Specification: <https://github.com/pnpm/spec/blob/834f2815cc/lockfile/9.0.md>
#[derive(Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<HashMap<PkgName, SnapshotDepRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_dependencies: Option<HashMap<PkgName, SnapshotDepRef>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub transitive_peer_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patched: Option<bool>,
}
