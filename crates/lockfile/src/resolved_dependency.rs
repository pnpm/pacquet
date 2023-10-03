use crate::{PkgName, PkgVerPeer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Map of resolved dependencies stored in a [`ProjectSnapshot`](crate::ProjectSnapshot).
///
/// The keys are package names.
pub type ResolvedDependencyMap = HashMap<PkgName, ResolvedDependencySpec>;

/// Value type of [`ResolvedDependencyMap`].
#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResolvedDependencySpec {
    pub specifier: String,
    pub version: PkgVerPeer,
}
