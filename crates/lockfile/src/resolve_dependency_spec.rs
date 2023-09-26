use crate::PkgVerPeer;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResolvedDependencySpec {
    pub specifier: String,
    pub version: PkgVerPeer,
}
