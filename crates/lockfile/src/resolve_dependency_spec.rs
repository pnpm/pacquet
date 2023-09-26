use crate::PkgVerPeer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type ResolvedDependencyMap = HashMap<String, ResolvedDependencySpec>;

#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResolvedDependencySpec {
    pub specifier: String,
    pub version: PkgVerPeer,
}
