use crate::{MultiProjectSnapshot, ProjectSnapshot};
use derive_more::{From, TryInto};
use serde::{Deserialize, Serialize};

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
    test_deserialization!(empty_importers_is_considered_multi: "importers: {}" => RootProjectSnapshot::Multi(Default::default()));

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
