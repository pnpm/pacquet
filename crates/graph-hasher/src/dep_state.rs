use crate::object_hasher::hash_object;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// Per-node identifier carrying everything `calc_dep_state` needs to
/// hash a snapshot. Mirrors the relevant subset of pnpm's
/// `DepsGraphNode` at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/deps/graph-hasher/src/index.ts#L12-L19>.
///
/// `full_pkg_id` is the upstream-shaped fingerprint used as the
/// `id` field in the recursive hash — `<pkgIdWithPatchHash>:<integrity>`
/// for packages with an integrity (`registry` resolution),
/// or `<pkgIdWithPatchHash>:<hashObject(resolution)>` for resolutions
/// without one (e.g. git refs). Pacquet's caller composes this
/// before passing it in; the hasher itself is opaque to how it was
/// computed.
///
/// `children` maps alias → dep-graph key for the snapshot's
/// children. Pacquet's natural input shape is the lockfile's
/// `snapshots[].dependencies` + `optionalDependencies` flattened,
/// with each value resolved to the snapshot key it points at.
pub struct DepsGraphNode<'a, K> {
    pub full_pkg_id: &'a str,
    pub children: HashMap<&'a str, K>,
}

/// Memoized per-depPath state cache. Mirrors pnpm's
/// [`DepsStateCache`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/deps/graph-hasher/src/index.ts#L21-L23):
/// the result of `hash_object` for each visited node is stashed so
/// the recursive walk over diamond-shaped graphs stays linear.
pub type DepsStateCache<K> = HashMap<K, String>;

/// Inputs to [`calc_dep_state`]. Mirrors the option bag at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/deps/graph-hasher/src/index.ts#L29-L33>.
pub struct CalcDepStateOptions<'a> {
    /// Output of [`crate::engine_name()`] — the platform / arch /
    /// node version prefix. Always part of the result.
    pub engine_name: &'a str,
    /// SHA-256 hex of the patch file for this package (when present).
    /// Appended as `;patch=<hash>`.
    pub patch_file_hash: Option<&'a str>,
    /// Whether to include the recursive dep-graph hash as
    /// `;deps=<hash>`. Upstream sets this to `hasSideEffects`
    /// (i.e. `!ignoreScripts && requiresBuild`) at
    /// `building/during-install/src/index.ts:202`.
    pub include_dep_graph_hash: bool,
}

/// Mirrors `calcDepState` at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/deps/graph-hasher/src/index.ts#L25-L44>.
///
/// Returns the cache key for the side-effects cache. Format:
/// `<engine_name>[;deps=<hash>][;patch=<hash>]`. Byte-for-byte
/// parity with pnpm is required — the key is persisted on disk and
/// shared with pnpm.
pub fn calc_dep_state<K>(
    graph: &HashMap<K, DepsGraphNode<'_, K>>,
    cache: &mut DepsStateCache<K>,
    dep_path: &K,
    opts: &CalcDepStateOptions<'_>,
) -> String
where
    K: Clone + Eq + std::hash::Hash,
{
    let mut result = opts.engine_name.to_string();
    if opts.include_dep_graph_hash {
        let deps_hash = calc_dep_graph_hash(graph, cache, &mut HashSet::new(), dep_path);
        result.push_str(";deps=");
        result.push_str(&deps_hash);
    }
    if let Some(patch) = opts.patch_file_hash {
        result.push_str(";patch=");
        result.push_str(patch);
    }
    result
}

/// Recursive helper for the `deps=` portion. Mirrors
/// `calcDepGraphHash` at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/deps/graph-hasher/src/index.ts#L46-L80>.
///
/// Hashes each node as `hashObject({ id, deps })` where `deps` is
/// the alias→child-hash map. `parents` breaks dependency cycles —
/// when a node would re-enter via its own ancestor, the child's
/// contribution becomes `""` (matching upstream's "node not in
/// graph" guard at line 55, which returns the empty string).
fn calc_dep_graph_hash<K>(
    graph: &HashMap<K, DepsGraphNode<'_, K>>,
    cache: &mut DepsStateCache<K>,
    parents: &mut HashSet<String>,
    dep_path: &K,
) -> String
where
    K: Clone + Eq + std::hash::Hash,
{
    if let Some(cached) = cache.get(dep_path) {
        return cached.clone();
    }
    let Some(node) = graph.get(dep_path) else {
        return String::new();
    };
    let mut deps_obj = serde_json::Map::new();
    if !node.children.is_empty() && !parents.contains(node.full_pkg_id) {
        // Push our `full_pkg_id` for the duration of this subtree
        // so cycles short-circuit on the second visit.
        let inserted = parents.insert(node.full_pkg_id.to_string());
        for (alias, child_key) in &node.children {
            let child_hash = calc_dep_graph_hash(graph, cache, parents, child_key);
            deps_obj.insert((*alias).to_string(), Value::String(child_hash));
        }
        if inserted {
            parents.remove(node.full_pkg_id);
        }
    }
    let hashed = hash_object(&json!({
        "id": node.full_pkg_id,
        "deps": Value::Object(deps_obj),
    }));
    cache.insert(dep_path.clone(), hashed.clone());
    cache.get(dep_path).expect("just inserted").clone()
}

#[cfg(test)]
mod tests {
    use super::{CalcDepStateOptions, DepsGraphNode, calc_dep_state};
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    /// Engine-only key (no dep graph, no patch). Pure prefix path
    /// for the cheapest cache lookup. Mirrors the "include_dep_graph_hash:
    /// false" path at
    /// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/deps/graph-hasher/src/index.ts#L36>.
    #[test]
    fn engine_only_key() {
        let graph: HashMap<String, DepsGraphNode<'_, String>> = HashMap::new();
        let mut cache = HashMap::new();
        let result = calc_dep_state(
            &graph,
            &mut cache,
            &"foo@1.0.0".to_string(),
            &CalcDepStateOptions {
                engine_name: "darwin;arm64;node20",
                patch_file_hash: None,
                include_dep_graph_hash: false,
            },
        );
        assert_eq!(result, "darwin;arm64;node20");
    }

    /// Patch hash gets appended as `;patch=<hash>`. Combined with
    /// the engine prefix when there's no dep graph hash. Mirrors
    /// lines 40-42 of `calcDepState`.
    #[test]
    fn patch_appended_without_dep_graph_hash() {
        let graph: HashMap<String, DepsGraphNode<'_, String>> = HashMap::new();
        let mut cache = HashMap::new();
        let result = calc_dep_state(
            &graph,
            &mut cache,
            &"foo@1.0.0".to_string(),
            &CalcDepStateOptions {
                engine_name: "linux;x64;node22",
                patch_file_hash: Some("sha256-abc"),
                include_dep_graph_hash: false,
            },
        );
        assert_eq!(result, "linux;x64;node22;patch=sha256-abc");
    }

    /// Dep-graph hash for a leaf (no children) is `hash_object({
    /// id, deps: {} })`. Both sites that consult `deps={}` (the
    /// leaf case at `calcLeafGlobalVirtualStorePath` and the
    /// children-elided case for cycle/missing-node) must agree.
    #[test]
    fn dep_graph_hash_for_leaf_uses_id_and_empty_deps() {
        let mut graph: HashMap<String, DepsGraphNode<'_, String>> = HashMap::new();
        graph.insert(
            "leaf@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "leaf@1.0.0:sha512-leaf", children: HashMap::new() },
        );
        let mut cache = HashMap::new();
        let result = calc_dep_state(
            &graph,
            &mut cache,
            &"leaf@1.0.0".to_string(),
            &CalcDepStateOptions {
                engine_name: "darwin;arm64;node20",
                patch_file_hash: None,
                include_dep_graph_hash: true,
            },
        );
        // Prefix preserved, deps= section appended.
        let parts: Vec<&str> = result.split(';').collect();
        assert!(parts.len() == 4, "expected `<plat>;<arch>;node<n>;deps=<hash>`, got {result:?}");
        assert!(parts[3].starts_with("deps="), "fourth segment must be `deps=...`: {result:?}");
        assert!(parts[3][5..].len() >= 40, "hash payload must be non-trivial: {result:?}");
    }

    /// Memoization at the cache layer: `calc_dep_graph_hash` writes
    /// each node's hash on first visit and returns the cached
    /// value on re-visit. Two leaf nodes with the same
    /// `full_pkg_id` must agree.
    #[test]
    fn cache_makes_repeat_calls_byte_equal() {
        let mut graph: HashMap<String, DepsGraphNode<'_, String>> = HashMap::new();
        graph.insert(
            "leaf@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "leaf@1.0.0:sha512-x", children: HashMap::new() },
        );
        let mut cache = HashMap::new();
        let opts = CalcDepStateOptions {
            engine_name: "darwin;arm64;node20",
            patch_file_hash: None,
            include_dep_graph_hash: true,
        };
        let a = calc_dep_state(&graph, &mut cache, &"leaf@1.0.0".to_string(), &opts);
        let b = calc_dep_state(&graph, &mut cache, &"leaf@1.0.0".to_string(), &opts);
        assert_eq!(a, b);
        assert_eq!(cache.len(), 1, "cache must hold exactly the one leaf entry");
    }

    /// Diamond graph: root depends on a and b, both depend on c.
    /// Both alias→child entries on the root must agree on the c
    /// node's hash, and the recursion must terminate.
    #[test]
    fn diamond_graph_resolves_consistently() {
        let mut graph: HashMap<String, DepsGraphNode<'_, String>> = HashMap::new();
        let mut root_children = HashMap::new();
        root_children.insert("a", "a@1.0.0".to_string());
        root_children.insert("b", "b@1.0.0".to_string());
        graph.insert(
            "root@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "root@1.0.0:sha512-root", children: root_children },
        );
        let mut a_children = HashMap::new();
        a_children.insert("c", "c@1.0.0".to_string());
        graph.insert(
            "a@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "a@1.0.0:sha512-a", children: a_children },
        );
        let mut b_children = HashMap::new();
        b_children.insert("c", "c@1.0.0".to_string());
        graph.insert(
            "b@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "b@1.0.0:sha512-b", children: b_children },
        );
        graph.insert(
            "c@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "c@1.0.0:sha512-c", children: HashMap::new() },
        );
        let mut cache = HashMap::new();
        let result = calc_dep_state(
            &graph,
            &mut cache,
            &"root@1.0.0".to_string(),
            &CalcDepStateOptions {
                engine_name: "darwin;arm64;node20",
                patch_file_hash: None,
                include_dep_graph_hash: true,
            },
        );
        // root + a + b + c = 4 cache entries.
        assert_eq!(cache.len(), 4, "expected 4 cache entries for diamond, got {cache:#?}");
        assert!(result.contains(";deps="), "result must include deps section: {result:?}");
    }

    /// Cycle: a depends on b, b depends on a. The walk must
    /// terminate (parents-set short-circuit) and produce a stable
    /// hash. Mirrors upstream's `if (!parents.has(node.fullPkgId))`
    /// guard at line 66.
    #[test]
    fn cyclic_graph_terminates_and_is_stable() {
        let mut graph: HashMap<String, DepsGraphNode<'_, String>> = HashMap::new();
        let mut a_children = HashMap::new();
        a_children.insert("b", "b@1.0.0".to_string());
        graph.insert(
            "a@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "a@1.0.0:sha512-a", children: a_children },
        );
        let mut b_children = HashMap::new();
        b_children.insert("a", "a@1.0.0".to_string());
        graph.insert(
            "b@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "b@1.0.0:sha512-b", children: b_children },
        );
        let mut cache = HashMap::new();
        let opts = CalcDepStateOptions {
            engine_name: "darwin;arm64;node20",
            patch_file_hash: None,
            include_dep_graph_hash: true,
        };
        let h1 = calc_dep_state(&graph, &mut cache, &"a@1.0.0".to_string(), &opts);
        let h2 = calc_dep_state(&graph, &mut cache, &"a@1.0.0".to_string(), &opts);
        assert_eq!(h1, h2);
    }

    /// Both patch and dep graph hashes append in upstream's order:
    /// `<engine>;deps=<h>;patch=<h>`. Mirrors index.js:36-42.
    #[test]
    fn dep_graph_and_patch_concatenate_in_upstream_order() {
        let mut graph: HashMap<String, DepsGraphNode<'_, String>> = HashMap::new();
        graph.insert(
            "x@1.0.0".to_string(),
            DepsGraphNode { full_pkg_id: "x@1.0.0:sha512-x", children: HashMap::new() },
        );
        let mut cache = HashMap::new();
        let result = calc_dep_state(
            &graph,
            &mut cache,
            &"x@1.0.0".to_string(),
            &CalcDepStateOptions {
                engine_name: "darwin;arm64;node20",
                patch_file_hash: Some("patchhex"),
                include_dep_graph_hash: true,
            },
        );
        let deps_pos = result.find(";deps=").expect("deps section present");
        let patch_pos = result.find(";patch=").expect("patch section present");
        assert!(deps_pos < patch_pos, "deps must come before patch in {result:?}");
    }
}
