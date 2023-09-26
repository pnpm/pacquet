use crate::{ResolvedDependencyMap, ResolvedDependencySpec};
use pacquet_package_json::DependencyGroup;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Snapshot of a single project.
#[derive(Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub specifiers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<ResolvedDependencyMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_dependencies: Option<ResolvedDependencyMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_dependencies: Option<ResolvedDependencyMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies_meta: Option<serde_yaml::Value>, // TODO: DependenciesMeta
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_directory: Option<String>,
}

impl ProjectSnapshot {
    /// Lookup dependency map according to group.
    pub fn get_map_by_group(&self, group: DependencyGroup) -> Option<&'_ ResolvedDependencyMap> {
        match group {
            DependencyGroup::Default | DependencyGroup::Peer => self.dependencies.as_ref(),
            DependencyGroup::Optional => self.optional_dependencies.as_ref(),
            DependencyGroup::Dev => self.dev_dependencies.as_ref(),
        }
    }

    /// Iterate over combination of dependency maps according to groups.
    pub fn dependencies_by_groups(
        &self,
        groups: impl IntoIterator<Item = DependencyGroup>,
    ) -> impl Iterator<Item = (&'_ str, &'_ ResolvedDependencySpec)> {
        groups
            .into_iter()
            .flat_map(|group| self.get_map_by_group(group))
            .flat_map(|map| map.iter())
            .map(|(name, ver_peer)| (name.as_str(), ver_peer))
    }
}
