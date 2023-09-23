use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct LockfileDependency {
    specifier: String,
    version: String,
}
