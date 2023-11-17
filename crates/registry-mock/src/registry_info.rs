use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct RegistryInfo {
    pub port: u16,
    pub listen: String,
    pub pid: u32,
}
