use super::build_deps_graph;
use pacquet_lockfile::{
    LockfileResolution, PackageKey, PackageMetadata, PkgName, PkgVerPeer, RegistryResolution,
    SnapshotDepRef, SnapshotEntry,
};
use pretty_assertions::assert_eq;
use ssri::Integrity;
use std::collections::HashMap;

fn name(s: &str) -> PkgName {
    PkgName::parse(s).expect("parse pkg name")
}

fn ver(s: &str) -> PkgVerPeer {
    s.parse().expect("parse PkgVerPeer")
}

fn key(n: &str, v: &str) -> PackageKey {
    PackageKey::new(name(n), ver(v))
}

fn integrity() -> Integrity {
    // Valid-shaped sha512 integrity. Content is irrelevant since
    // the adapter just stringifies it.
    "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        .parse()
        .expect("parse integrity")
}

fn registry_metadata() -> PackageMetadata {
    PackageMetadata {
        resolution: LockfileResolution::Registry(RegistryResolution { integrity: integrity() }),
        engines: None,
        cpu: None,
        os: None,
        libc: None,
        deprecated: None,
        has_bin: None,
        prepare: None,
        bundled_dependencies: None,
        peer_dependencies: None,
        peer_dependencies_meta: None,
    }
}

/// A registry-resolution snapshot's `full_pkg_id` is
/// `<pkg_id>:<integrity>` — no fall-through to `hashObject`.
/// Mirrors upstream's `createFullPkgId` branch at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/deps/graph-hasher/src/index.ts#L268-L270>.
#[test]
fn registry_resolution_full_pkg_id_uses_integrity_verbatim() {
    let pkg = key("@scope/foo", "1.0.0");
    let snapshots = HashMap::from([(pkg.clone(), SnapshotEntry::default())]);
    let packages = HashMap::from([(pkg.clone(), registry_metadata())]);

    let graph = build_deps_graph(&snapshots, &packages);
    let node = graph.get(&pkg).expect("graph node");
    // `PackageKey`'s `Display` impl renders `<name>@<ver>`; the
    // `full_pkg_id` prefixes that with the integrity verbatim.
    let expected_prefix = "@scope/foo@1.0.0:sha512-";
    assert!(
        node.full_pkg_id.starts_with(expected_prefix),
        "expected full_pkg_id to start with `{expected_prefix}`, got `{}`",
        node.full_pkg_id,
    );
}

/// `snapshots[].dependencies` populates the children map. Each
/// alias resolves to the `PackageKey` of the dep entry.
#[test]
fn dependencies_become_children() {
    let parent_key = key("parent", "1.0.0");
    let child_key = key("child", "2.0.0");
    let dependencies = HashMap::from([(name("child"), SnapshotDepRef::Plain(ver("2.0.0")))]);
    let snapshots = HashMap::from([
        (
            parent_key.clone(),
            SnapshotEntry { dependencies: Some(dependencies), ..Default::default() },
        ),
        (child_key.clone(), SnapshotEntry::default()),
    ]);
    let packages = HashMap::from([
        (parent_key.clone(), registry_metadata()),
        (child_key.clone(), registry_metadata()),
    ]);

    let graph = build_deps_graph(&snapshots, &packages);
    let parent_node = graph.get(&parent_key).expect("parent node");
    assert_eq!(parent_node.children.len(), 1);
    let resolved = parent_node.children.get("child").expect("alias `child` present");
    assert_eq!(resolved, &child_key);
}

/// `snapshots[].optional_dependencies` is folded into `children`
/// alongside regular deps — the dep-state hash doesn't distinguish
/// the two. Mirrors upstream's
/// [`lockfileDepsToGraphChildren`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/deps/graph-hasher/src/index.ts#L252-L261)
/// spreading `{ ...dependencies, ...optionalDependencies }`.
#[test]
fn optional_dependencies_fold_into_children() {
    let parent_key = key("parent", "1.0.0");
    let opt_key = key("optional", "3.0.0");
    let optional = HashMap::from([(name("optional"), SnapshotDepRef::Plain(ver("3.0.0")))]);
    let snapshots = HashMap::from([
        (
            parent_key.clone(),
            SnapshotEntry {
                dependencies: None,
                optional_dependencies: Some(optional),
                ..Default::default()
            },
        ),
        (opt_key.clone(), SnapshotEntry::default()),
    ]);
    let packages = HashMap::from([
        (parent_key.clone(), registry_metadata()),
        (opt_key.clone(), registry_metadata()),
    ]);

    let graph = build_deps_graph(&snapshots, &packages);
    let parent_node = graph.get(&parent_key).expect("parent node");
    assert!(parent_node.children.contains_key("optional"));
}

/// Snapshot whose metadata entry is missing from `packages:` is
/// skipped silently. The cache lookup for that snapshot will miss
/// (no graph node → empty deps hash), which means `BuildModules`'s
/// `is_built` gate falls through to "rebuild" — safe default.
#[test]
fn snapshot_without_metadata_is_skipped() {
    let pkg = key("orphan", "1.0.0");
    let snapshots = HashMap::from([(pkg.clone(), SnapshotEntry::default())]);
    let packages: HashMap<PackageKey, PackageMetadata> = HashMap::new();

    let graph = build_deps_graph(&snapshots, &packages);
    assert!(graph.is_empty(), "orphan snapshot must not produce a graph node");
}
