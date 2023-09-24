use crate::LockfileDependency;
use derive_more::{From, TryInto};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Snapshot of a single project.
#[derive(Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub specifiers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<HashMap<String, LockfileDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_dependencies: Option<HashMap<String, LockfileDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_dependencies: Option<HashMap<String, LockfileDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies_meta: Option<serde_yaml::Value>, // TODO: DependenciesMeta
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_directory: Option<String>,
}

/// Snapshot of a multi-project monorepo.
#[derive(Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MultiProjectSnapshot {
    pub importers: HashMap<String, ProjectSnapshot>,
}

/// Snapshot of the root project.
#[derive(Debug, PartialEq, Deserialize, Serialize, From, TryInto)]
#[serde(untagged)]
pub enum RootProjectSnapshot {
    Multi(MultiProjectSnapshot),
    Single(ProjectSnapshot),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    macro_rules! test_deserialization {
        ($name:ident: $input:expr => $output:expr) => {
            #[test]
            fn $name() {
                let yaml = $input;
                let received: RootProjectSnapshot = serde_yaml::from_str(yaml).unwrap();
                let expected: RootProjectSnapshot = $output;
                assert_eq!(received, expected);
            }
        };
    }

    test_deserialization!(empty_object_is_considered_single: "{}" => RootProjectSnapshot::Single(Default::default()));
    test_deserialization!(empty_importers_is_considered_multi: "{ importers: {} }" => RootProjectSnapshot::Multi(Default::default()));

    macro_rules! test_serialization {
        ($name:ident: $input:expr => $output:expr) => {
            #[test]
            fn $name() {
                let snapshot: RootProjectSnapshot = $input;
                let received = serde_yaml::to_string(&snapshot).unwrap();
                let received = received.trim();
                let expected = $output;
                assert_eq!(received, expected);
            }
        };
    }

    test_serialization!(default_single_becomes_empty_object: RootProjectSnapshot::Single(Default::default()) => "{}");
    test_serialization!(default_multi_gives_empty_importers: RootProjectSnapshot::Multi(Default::default()) => "importers: {}");
}
