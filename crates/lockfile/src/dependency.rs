use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct LockfileDependency {
    specifier: String,
    version: String, // TODO: LockfileDependencyVersion syntax: 10.9.1(@types/node@18.7.19)(typescript@5.1.6)
}
