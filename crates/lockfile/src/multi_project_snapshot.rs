use crate::ProjectSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Snapshot of a multi-project monorepo.
#[derive(Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MultiProjectSnapshot {
    pub importers: HashMap<String, ProjectSnapshot>,
}
