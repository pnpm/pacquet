use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct IntegrityResolution {
    pub integrity: String,
}
