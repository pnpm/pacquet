use super::build_sequence;
use pacquet_lockfile::{
    LockfileResolution, PackageKey, PackageMetadata, PkgName, PkgVerPeer, ProjectSnapshot,
    RegistryResolution, ResolvedDependencyMap, ResolvedDependencySpec, SnapshotDepRef,
    SnapshotEntry,
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

fn pkg_meta(requires_build: Option<bool>) -> PackageMetadata {
    PackageMetadata {
        resolution: LockfileResolution::Registry(RegistryResolution {
            integrity: "sha512-deadbeef".parse::<Integrity>().expect("parse integrity"),
        }),
        engines: None,
        cpu: None,
        os: None,
        libc: None,
        deprecated: None,
        has_bin: None,
        prepare: None,
        requires_build,
        bundled_dependencies: None,
        peer_dependencies: None,
        peer_dependencies_meta: None,
    }
}

fn snap(deps: &[(&str, &str)]) -> SnapshotEntry {
    let map: HashMap<PkgName, SnapshotDepRef> =
        deps.iter().map(|(n, v)| (name(n), SnapshotDepRef::Plain(ver(v)))).collect();
    SnapshotEntry {
        id: None,
        dependencies: (!map.is_empty()).then_some(map),
        optional_dependencies: None,
        transitive_peer_dependencies: None,
        patched: None,
    }
}

fn importer(deps: &[(&str, &str)]) -> ProjectSnapshot {
    let map: ResolvedDependencyMap = deps
        .iter()
        .map(|(n, v)| {
            (name(n), ResolvedDependencySpec { specifier: (*v).to_string(), version: ver(v) })
        })
        .collect();
    ProjectSnapshot {
        specifiers: None,
        dependencies: (!map.is_empty()).then_some(map),
        optional_dependencies: None,
        dev_dependencies: None,
        dependencies_meta: None,
        publish_directory: None,
    }
}

fn root_importers(deps: &[(&str, &str)]) -> HashMap<String, ProjectSnapshot> {
    HashMap::from([(".".to_string(), importer(deps))])
}

#[test]
fn empty_inputs() {
    let chunks = build_sequence(&HashMap::new(), &HashMap::new(), &HashMap::new());
    dbg!(&chunks);
    assert!(chunks.is_empty(), "empty inputs ⇒ no chunks: {chunks:?}");
}

#[test]
fn no_requires_build_yields_empty() {
    let snapshots = HashMap::from([
        (key("a", "1.0.0"), snap(&[("b", "1.0.0")])),
        (key("b", "1.0.0"), snap(&[])),
    ]);
    let packages =
        HashMap::from([(key("a", "1.0.0"), pkg_meta(None)), (key("b", "1.0.0"), pkg_meta(None))]);
    let importers = root_importers(&[("a", "1.0.0")]);

    let chunks = build_sequence(&packages, &snapshots, &importers);
    dbg!(&chunks);
    assert!(chunks.is_empty(), "no requires_build ⇒ no chunks: {chunks:?}");
}

#[test]
fn leaf_with_requires_build_runs_first() {
    // a depends on b; only b requires build. Both nodes are added to the
    // build sequence (a is an ancestor of a buildable node), but the order
    // must be b before a.
    let snapshots = HashMap::from([
        (key("a", "1.0.0"), snap(&[("b", "1.0.0")])),
        (key("b", "1.0.0"), snap(&[])),
    ]);
    let packages = HashMap::from([
        (key("a", "1.0.0"), pkg_meta(None)),
        (key("b", "1.0.0"), pkg_meta(Some(true))),
    ]);
    let importers = root_importers(&[("a", "1.0.0")]);

    let chunks = build_sequence(&packages, &snapshots, &importers);
    assert_eq!(chunks, vec![vec![key("b", "1.0.0")], vec![key("a", "1.0.0")]]);
}

#[test]
fn deep_chain_orders_leaf_first() {
    // a -> b -> c, only c requires build. Sequence: [c], [b], [a].
    let snapshots = HashMap::from([
        (key("a", "1.0.0"), snap(&[("b", "1.0.0")])),
        (key("b", "1.0.0"), snap(&[("c", "1.0.0")])),
        (key("c", "1.0.0"), snap(&[])),
    ]);
    let packages = HashMap::from([
        (key("a", "1.0.0"), pkg_meta(None)),
        (key("b", "1.0.0"), pkg_meta(None)),
        (key("c", "1.0.0"), pkg_meta(Some(true))),
    ]);
    let importers = root_importers(&[("a", "1.0.0")]);

    let chunks = build_sequence(&packages, &snapshots, &importers);
    assert_eq!(
        chunks,
        vec![vec![key("c", "1.0.0")], vec![key("b", "1.0.0")], vec![key("a", "1.0.0")]],
    );
}

#[test]
fn unrelated_subgraph_excluded() {
    // a -> b (b builds), x -> y (y builds). Importer only depends on a.
    // Only the `a` subgraph should appear.
    let snapshots = HashMap::from([
        (key("a", "1.0.0"), snap(&[("b", "1.0.0")])),
        (key("b", "1.0.0"), snap(&[])),
        (key("x", "1.0.0"), snap(&[("y", "1.0.0")])),
        (key("y", "1.0.0"), snap(&[])),
    ]);
    let packages = HashMap::from([
        (key("a", "1.0.0"), pkg_meta(None)),
        (key("b", "1.0.0"), pkg_meta(Some(true))),
        (key("x", "1.0.0"), pkg_meta(None)),
        (key("y", "1.0.0"), pkg_meta(Some(true))),
    ]);
    let importers = root_importers(&[("a", "1.0.0")]);

    let chunks = build_sequence(&packages, &snapshots, &importers);
    let flat: Vec<_> = chunks.into_iter().flatten().collect();
    dbg!(&flat);
    assert!(flat.contains(&key("a", "1.0.0")), "ancestor of build leaf must appear: {flat:?}");
    assert!(flat.contains(&key("b", "1.0.0")), "build leaf must appear: {flat:?}");
    assert!(!flat.contains(&key("x", "1.0.0")), "unreachable ancestor must be excluded: {flat:?}");
    assert!(
        !flat.contains(&key("y", "1.0.0")),
        "unreachable build leaf must be excluded: {flat:?}"
    );
}

#[test]
fn parallel_build_leaves_share_chunk() {
    // root depends on a and b; both a and b have requires_build but no shared
    // descendants. Both build leaves should land in the same chunk; root
    // follows in the next chunk as their ancestor.
    let snapshots = HashMap::from([
        (key("root", "1.0.0"), snap(&[("a", "1.0.0"), ("b", "1.0.0")])),
        (key("a", "1.0.0"), snap(&[])),
        (key("b", "1.0.0"), snap(&[])),
    ]);
    let packages = HashMap::from([
        (key("root", "1.0.0"), pkg_meta(None)),
        (key("a", "1.0.0"), pkg_meta(Some(true))),
        (key("b", "1.0.0"), pkg_meta(Some(true))),
    ]);
    let importers = root_importers(&[("root", "1.0.0")]);

    let chunks = build_sequence(&packages, &snapshots, &importers);
    assert_eq!(chunks.len(), 2);
    let mut leaves = chunks[0].clone();
    leaves.sort_by_key(|k| k.to_string());
    assert_eq!(leaves, vec![key("a", "1.0.0"), key("b", "1.0.0")]);
    assert_eq!(chunks[1], vec![key("root", "1.0.0")]);
}
