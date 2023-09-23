use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct LockfilePackageResolution {
    integrity: String,
}
