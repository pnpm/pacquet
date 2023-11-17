use crate::port_to_url::port_to_url;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct RegistryInfo {
    pub port: u16,
    pub pid: u32,
}

impl RegistryInfo {
    pub fn listen(&self) -> String {
        port_to_url(self.port)
    }
}
