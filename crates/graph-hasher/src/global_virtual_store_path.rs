//! Pacquet port of pnpm's global-virtual-store directory naming —
//! [`calcGraphNodeHash`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L122-L146)
//! and
//! [`formatGlobalVirtualStorePath`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L155-L160).
//!
//! Stage 1 of pnpm/pacquet#432 keeps the engine string unconditional —
//! the engine-agnostic gating that flips `engine` to `null` when no
//! package transitively requires a build is tracked separately. The
//! `engine` parameter on [`calc_graph_node_hash`] still threads through
//! so the follow-up that adds the gating doesn't have to change the
//! signature.

use crate::{HashEncoding, dep_state::calc_dep_graph_hash, hash_object_without_sorting};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use crate::dep_state::{DepsGraphNode, DepsStateCache};

/// Compute the hex digest that uniquely identifies one snapshot's
/// position in the global virtual store. Mirrors
/// [`calcGraphNodeHash`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L122-L146).
///
/// The output is the `hash` segment that
/// [`format_global_virtual_store_path`] places after `<name>/<version>/`.
/// Two snapshots that resolve to the same package contents (identical
/// `fullPkgId`s and identical recursive children) hash to the same
/// value and therefore share one directory under
/// `<store>/links/<name>/<version>/` — which is how pnpm and pacquet
/// avoid re-extracting the same tarball once per peer-context.
pub fn calc_graph_node_hash<K>(
    graph: &HashMap<K, DepsGraphNode<K>>,
    cache: &mut DepsStateCache<K>,
    dep_path: &K,
    engine: Option<&str>,
) -> String
where
    K: Clone + Eq + std::hash::Hash,
{
    let deps_hash = calc_dep_graph_hash(graph, cache, &mut HashSet::new(), dep_path);
    let engine_value = match engine {
        Some(s) => Value::String(s.to_owned()),
        None => Value::Null,
    };
    let payload = json!({
        "engine": engine_value,
        "deps": deps_hash,
    });
    hash_object_without_sorting(&payload, HashEncoding::Hex)
}

/// Format a global-virtual-store-relative path for a package. Mirrors
/// upstream's
/// [`formatGlobalVirtualStorePath`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L155-L160)
/// — the `@/` prefix on unscoped packages keeps every entry in the
/// shared store at the same `<scope>/<name>/<version>/<hash>` depth,
/// so a single `readdir` pass per level can enumerate the store
/// without special-casing the unscoped path layout.
pub fn format_global_virtual_store_path(name: &str, version: &str, hex_digest: &str) -> String {
    let prefix = if name.starts_with('@') { "" } else { "@/" };
    format!("{prefix}{name}/{version}/{hex_digest}")
}

#[cfg(test)]
mod tests {
    use super::{calc_graph_node_hash, format_global_virtual_store_path};
    use crate::dep_state::DepsGraphNode;
    use std::collections::HashMap;

    /// Scoped packages don't get the `@/` prefix — they already start
    /// with `@<scope>/`. Unscoped packages do.
    #[test]
    fn format_prefixes_unscoped_with_at_slash() {
        assert_eq!(
            format_global_virtual_store_path("foo", "1.2.3", "deadbeef"),
            "@/foo/1.2.3/deadbeef",
        );
        assert_eq!(
            format_global_virtual_store_path("@scope/foo", "1.2.3", "deadbeef"),
            "@scope/foo/1.2.3/deadbeef",
        );
    }

    /// Two graphs with identical structure and `full_pkg_id`s produce
    /// the same hash — same as upstream's design where the GVS path is
    /// the deduplication key.
    #[test]
    fn identical_leaves_hash_identically() {
        let mut graph: HashMap<String, DepsGraphNode<String>> = HashMap::new();
        graph.insert(
            "leaf@1.0.0".to_string(),
            DepsGraphNode {
                full_pkg_id: "leaf@1.0.0:sha512-x".to_string(),
                children: HashMap::new(),
            },
        );
        let mut cache_a = HashMap::new();
        let mut cache_b = HashMap::new();
        let a = calc_graph_node_hash(
            &graph,
            &mut cache_a,
            &"leaf@1.0.0".to_string(),
            Some("darwin-arm64-node20"),
        );
        let b = calc_graph_node_hash(
            &graph,
            &mut cache_b,
            &"leaf@1.0.0".to_string(),
            Some("darwin-arm64-node20"),
        );
        assert_eq!(a, b, "deterministic for same input");
        assert_eq!(a.len(), 64, "sha256 hex digest is 64 chars");
    }

    /// Different engines produce different hashes (the field is part
    /// of the hash payload). Mirrors the "engine flips between
    /// `null` and `ENGINE_NAME`" branch in upstream — pacquet always
    /// passes `Some(...)` today, but the param exists so a follow-up
    /// implementing the engine-agnostic gating slots in cleanly.
    #[test]
    fn engine_string_changes_hash() {
        let mut graph: HashMap<String, DepsGraphNode<String>> = HashMap::new();
        graph.insert(
            "leaf@1.0.0".to_string(),
            DepsGraphNode {
                full_pkg_id: "leaf@1.0.0:sha512-x".to_string(),
                children: HashMap::new(),
            },
        );
        let mut cache = HashMap::new();
        let with_engine = calc_graph_node_hash(
            &graph,
            &mut cache,
            &"leaf@1.0.0".to_string(),
            Some("darwin-arm64-node20"),
        );
        let mut cache_other = HashMap::new();
        let with_other_engine = calc_graph_node_hash(
            &graph,
            &mut cache_other,
            &"leaf@1.0.0".to_string(),
            Some("linux-x64-node22"),
        );
        let mut cache_null = HashMap::new();
        let with_null =
            calc_graph_node_hash(&graph, &mut cache_null, &"leaf@1.0.0".to_string(), None);
        assert_ne!(with_engine, with_other_engine);
        assert_ne!(with_engine, with_null);
        assert_ne!(with_other_engine, with_null);
    }

    /// Two snapshots whose children differ (same `full_pkg_id`,
    /// different deps) hash differently — the GVS path includes the
    /// transitive dep contribution.
    #[test]
    fn different_children_change_hash() {
        let mut graph: HashMap<String, DepsGraphNode<String>> = HashMap::new();
        graph.insert(
            "leaf@1.0.0".to_string(),
            DepsGraphNode {
                full_pkg_id: "leaf@1.0.0:sha512-x".to_string(),
                children: HashMap::new(),
            },
        );
        let mut root_a_children = HashMap::new();
        root_a_children.insert("a".to_string(), "leaf@1.0.0".to_string());
        graph.insert(
            "root@1.0.0(a)".to_string(),
            DepsGraphNode {
                full_pkg_id: "root@1.0.0:sha512-r".to_string(),
                children: root_a_children,
            },
        );
        graph.insert(
            "root@1.0.0(b)".to_string(),
            DepsGraphNode {
                full_pkg_id: "root@1.0.0:sha512-r".to_string(),
                children: HashMap::new(),
            },
        );
        let mut cache_a = HashMap::new();
        let with_dep = calc_graph_node_hash(
            &graph,
            &mut cache_a,
            &"root@1.0.0(a)".to_string(),
            Some("darwin-arm64-node20"),
        );
        let mut cache_b = HashMap::new();
        let without_dep = calc_graph_node_hash(
            &graph,
            &mut cache_b,
            &"root@1.0.0(b)".to_string(),
            Some("darwin-arm64-node20"),
        );
        assert_ne!(
            with_dep, without_dep,
            "same root, different children must not collide on GVS hash",
        );
    }
}
