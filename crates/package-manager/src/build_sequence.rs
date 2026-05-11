use crate::graph_sequencer::{GraphSequencerResult, graph_sequencer};
use pacquet_lockfile::{PackageKey, ProjectSnapshot, SnapshotEntry};
use std::collections::{HashMap, HashSet};

/// Compute topologically ordered chunks of packages that need building.
///
/// Ports `buildSequence` from
/// `https://github.com/pnpm/pnpm/blob/80037699fb/building/during-install/src/buildSequence.ts`.
///
/// The returned chunks are ordered children-first: every chunk may safely
/// run only after every preceding chunk has finished. Members of the same
/// chunk are independent and could run concurrently (pacquet currently runs
/// them sequentially).
///
/// Only nodes whose subtree contains at least one build candidate appear in
/// the output. Snapshots not reachable from any importer are excluded —
/// matching pnpm's `getSubgraphToBuild` walk.
///
/// `requires_build` is the per-snapshot map computed by the caller after
/// extraction (from each package's manifest scripts and presence of
/// `binding.gyp` / `.hooks/`). Mirrors the role of `node.requiresBuild`
/// upstream, which the worker computes from the extracted package contents.
pub fn build_sequence(
    requires_build: &HashMap<PackageKey, bool>,
    snapshots: &HashMap<PackageKey, SnapshotEntry>,
    importers: &HashMap<String, ProjectSnapshot>,
) -> Vec<Vec<PackageKey>> {
    let children = build_children_map(snapshots);
    let root_dep_paths = collect_root_dep_paths(importers, snapshots);

    let mut nodes_to_build_set: HashSet<PackageKey> = HashSet::new();
    let mut nodes_to_build: Vec<PackageKey> = Vec::new();
    let mut walked: HashSet<PackageKey> = HashSet::new();
    get_subgraph_to_build(
        &root_dep_paths,
        &children,
        requires_build,
        &mut nodes_to_build_set,
        &mut nodes_to_build,
        &mut walked,
    );

    if nodes_to_build.is_empty() {
        return Vec::new();
    }

    let filtered_graph: HashMap<PackageKey, Vec<PackageKey>> = nodes_to_build
        .iter()
        .map(|k| {
            let edges = children
                .get(k)
                .map(|cs| cs.iter().filter(|c| nodes_to_build_set.contains(c)).cloned().collect())
                .unwrap_or_default();
            (k.clone(), edges)
        })
        .collect();

    let GraphSequencerResult { chunks, safe, .. } =
        graph_sequencer(&filtered_graph, &nodes_to_build);
    if !safe {
        tracing::warn!(
            target: "pacquet::build",
            "dependency cycle detected while computing build order; \
             packages inside the cycle will run in arbitrary order",
        );
    }
    chunks
}

/// Build the `node → children` adjacency map from the snapshot map.
///
/// Children are the resolved snapshot keys of `dependencies` and
/// `optional_dependencies`. Edges to keys not present in the snapshot map
/// are dropped (matches pnpm: missing nodes mean the dependency was already
/// in `node_modules` and not part of this install's graph).
fn build_children_map(
    snapshots: &HashMap<PackageKey, SnapshotEntry>,
) -> HashMap<PackageKey, Vec<PackageKey>> {
    let mut children: HashMap<PackageKey, Vec<PackageKey>> =
        HashMap::with_capacity(snapshots.len());
    for (key, snap) in snapshots {
        let mut child_keys: Vec<PackageKey> = Vec::new();
        for deps in
            [snap.dependencies.as_ref(), snap.optional_dependencies.as_ref()].into_iter().flatten()
        {
            for (alias, dep_ref) in deps {
                let resolved = dep_ref.resolve(alias);
                if snapshots.contains_key(&resolved) {
                    child_keys.push(resolved);
                }
            }
        }
        children.insert(key.clone(), child_keys);
    }
    children
}

/// Gather snapshot keys for every direct dependency declared by an importer.
///
/// Iterates `dependencies`, `devDependencies`, and `optionalDependencies` of
/// every importer. Keys whose constructed snapshot key is not in `snapshots`
/// are dropped silently (e.g. workspace links that are not separate packages).
fn collect_root_dep_paths(
    importers: &HashMap<String, ProjectSnapshot>,
    snapshots: &HashMap<PackageKey, SnapshotEntry>,
) -> Vec<PackageKey> {
    let mut roots: Vec<PackageKey> = Vec::new();
    let mut seen: HashSet<PackageKey> = HashSet::new();
    for snapshot in importers.values() {
        for map in [
            snapshot.dependencies.as_ref(),
            snapshot.optional_dependencies.as_ref(),
            snapshot.dev_dependencies.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            for (name, spec) in map {
                let key = PackageKey::new(name.clone(), spec.version.clone());
                if !snapshots.contains_key(&key) {
                    continue;
                }
                if seen.insert(key.clone()) {
                    roots.push(key);
                }
            }
        }
    }
    roots
}

/// Walk the dep graph from `entry_nodes`, filling `nodes_to_build` with
/// packages whose subtree (including themselves) contains a build candidate.
///
/// Ports `getSubgraphToBuild` from
/// `https://github.com/pnpm/pnpm/blob/80037699fb/building/during-install/src/buildSequence.ts`.
///
/// Returns whether *any* of the entry nodes (or their subtrees) needs to build.
fn get_subgraph_to_build(
    entry_nodes: &[PackageKey],
    children: &HashMap<PackageKey, Vec<PackageKey>>,
    requires_build: &HashMap<PackageKey, bool>,
    nodes_to_build_set: &mut HashSet<PackageKey>,
    nodes_to_build: &mut Vec<PackageKey>,
    walked: &mut HashSet<PackageKey>,
) -> bool {
    let mut current_should_be_built = false;
    for dep_path in entry_nodes {
        if !children.contains_key(dep_path) {
            continue; // already in node_modules / not part of this graph
        }
        if walked.contains(dep_path) {
            continue;
        }
        walked.insert(dep_path.clone());

        let child_paths = children.get(dep_path).cloned().unwrap_or_default();
        let child_should_be_built = get_subgraph_to_build(
            &child_paths,
            children,
            requires_build,
            nodes_to_build_set,
            nodes_to_build,
            walked,
        );

        let needs_build = requires_build.get(dep_path).copied().unwrap_or(false);
        // TODO: also trigger on `patch != null` when pacquet supports
        // `patchedDependencies`.

        if child_should_be_built || needs_build {
            if nodes_to_build_set.insert(dep_path.clone()) {
                nodes_to_build.push(dep_path.clone());
            }
            current_should_be_built = true;
        }
    }
    current_should_be_built
}

#[cfg(test)]
mod tests;
