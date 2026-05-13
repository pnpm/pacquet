use super::{HoistError, HoistOpts, HoisterResult, hoist};
use pacquet_lockfile::{
    ComVer, Lockfile, LockfileSettings, LockfileVersion, PkgName, PkgNameVerPeer, PkgVerPeer,
    ProjectSnapshot, ResolvedDependencyMap, ResolvedDependencySpec, SnapshotDepRef, SnapshotEntry,
};
use pretty_assertions::assert_eq;
use std::{collections::HashMap, rc::Rc};

fn lockfile_version() -> LockfileVersion<9> {
    LockfileVersion::<9>::try_from(ComVer::new(9, 0)).expect("lockfileVersion 9.0 is compatible")
}

fn pkg_name(s: &str) -> PkgName {
    PkgName::parse(s).expect("parse PkgName")
}

fn ver_peer(s: &str) -> PkgVerPeer {
    s.parse::<PkgVerPeer>().expect("parse PkgVerPeer")
}

fn dep_key(name: &str, version: &str) -> PkgNameVerPeer {
    PkgNameVerPeer::new(pkg_name(name), ver_peer(version))
}

fn resolved_dep(version: &str) -> ResolvedDependencySpec {
    ResolvedDependencySpec { specifier: version.to_string(), version: ver_peer(version).into() }
}

fn empty_lockfile() -> Lockfile {
    Lockfile {
        lockfile_version: lockfile_version(),
        settings: Some(LockfileSettings::default()),
        overrides: None,
        importers: HashMap::new(),
        packages: None,
        snapshots: None,
    }
}

/// Direct port of the upstream "hoist throws an error if the
/// lockfile is broken" test at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/installing/linking/real-hoist/test/index.ts>.
/// The root importer references `foo@1.0.0` but the `snapshots`
/// map is empty, so the wrapper's snapshot lookup must surface
/// `LockfileMissingDependency` rather than silently produce a
/// truncated tree.
#[test]
fn hoist_throws_on_broken_lockfile() {
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("foo"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: None,
        snapshots: None,
    };

    let err = hoist(&lockfile, &HoistOpts::default()).expect_err("missing snapshot should error");
    match err {
        HoistError::LockfileMissingDependency { pkg_key } => assert_eq!(pkg_key, "foo@1.0.0"),
        other => panic!("expected LockfileMissingDependency, got {other:?}"),
    }
}

/// An empty lockfile (no importers at all) hoists to an empty
/// result. Sanity-checks the wrapper's "no root importer" branch
/// and the stub `nm_hoist` end-to-end.
#[test]
fn empty_lockfile_yields_empty_root() {
    let lockfile = empty_lockfile();
    let result = hoist(&lockfile, &HoistOpts::default()).expect("empty hoist should succeed");
    assert_eq!(result.name, ".");
    assert_eq!(result.ident_name, ".");
    assert!(result.dependencies.borrow().is_empty(), "no importers means no children at the root");
}

/// `root → a → b` collapses to `root → {a, b}` because `b` has no
/// name conflict at root. Pins the simplest hoisting case: a
/// single transitive dep surfaces at the root and its old parent
/// no longer carries it.
#[test]
fn one_transitive_dep_hoists_to_root() {
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    let mut snapshots = HashMap::new();
    let mut a_deps = HashMap::new();
    a_deps.insert(pkg_name("b"), SnapshotDepRef::Plain(ver_peer("1.0.0")));
    snapshots.insert(
        dep_key("a", "1.0.0"),
        SnapshotEntry { dependencies: Some(a_deps), ..SnapshotEntry::default() },
    );
    snapshots.insert(dep_key("b", "1.0.0"), SnapshotEntry::default());

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: None,
        snapshots: Some(snapshots),
    };

    let result = hoist(&lockfile, &HoistOpts::default()).expect("happy hoist should succeed");
    assert_eq!(result.name, ".");
    let root_children = result.dependencies.borrow();
    let mut names: Vec<&str> = root_children.iter().map(|d| d.0.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a", "b"], "both a and b sit at root: {result:#?}");
    let a = root_children.iter().find(|d| d.0.name == "a").unwrap().0.clone();
    assert!(a.dependencies.borrow().is_empty(), "a's b moved to root: {a:#?}");
}

/// Diamond dependency `root → {a, c}` with both `a → b@1` and
/// `c → b@1` (same `Rc` thanks to the wrapper's identity dedup).
/// After hoist, `b` appears once at root and its old parents
/// `a` and `c` carry no transitive deps.
#[test]
fn diamond_dep_hoists_once_to_root() {
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));
    root_deps.insert(pkg_name("c"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    // Both a@1 and c@1 depend on b@1 — same dep_key → same Rc in
    // the input HoisterTree, same Rc in the result graph.
    let mut snapshots = HashMap::new();
    let mut a_deps = HashMap::new();
    a_deps.insert(pkg_name("b"), SnapshotDepRef::Plain(ver_peer("1.0.0")));
    let mut c_deps = HashMap::new();
    c_deps.insert(pkg_name("b"), SnapshotDepRef::Plain(ver_peer("1.0.0")));
    snapshots.insert(
        dep_key("a", "1.0.0"),
        SnapshotEntry { dependencies: Some(a_deps), ..SnapshotEntry::default() },
    );
    snapshots.insert(
        dep_key("c", "1.0.0"),
        SnapshotEntry { dependencies: Some(c_deps), ..SnapshotEntry::default() },
    );
    snapshots.insert(dep_key("b", "1.0.0"), SnapshotEntry::default());

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: None,
        snapshots: Some(snapshots),
    };

    let result = hoist(&lockfile, &HoistOpts::default()).expect("hoist should succeed");
    let root_children = result.dependencies.borrow();
    let mut names: Vec<&str> = root_children.iter().map(|d| d.0.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a", "b", "c"], "diamond flattens at root: {result:#?}");
    let a = root_children.iter().find(|d| d.0.name == "a").unwrap().0.clone();
    let c = root_children.iter().find(|d| d.0.name == "c").unwrap().0.clone();
    assert!(a.dependencies.borrow().is_empty(), "a stripped of its b: {a:#?}");
    assert!(c.dependencies.borrow().is_empty(), "c stripped of its b: {c:#?}");

    // Walk the whole result graph and collect every distinct
    // allocation whose `name == "b"`. The wrapper deduped a@1's b
    // and c@1's b into one `Rc<HoisterResult>` (the diamond shares
    // by identity), and the hoist must preserve that identity
    // rather than allocating a second copy somewhere — so the set
    // of pointers we collect has exactly one entry.
    let mut b_ptrs: std::collections::HashSet<*const HoisterResult> =
        std::collections::HashSet::new();
    let mut stack: Vec<Rc<HoisterResult>> = root_children.iter().map(|d| d.0.clone()).collect();
    let mut walked: std::collections::HashSet<*const HoisterResult> =
        std::collections::HashSet::new();
    while let Some(node) = stack.pop() {
        if !walked.insert(Rc::as_ptr(&node)) {
            continue;
        }
        if node.name == "b" {
            b_ptrs.insert(Rc::as_ptr(&node));
        }
        for d in node.dependencies.borrow().iter() {
            stack.push(d.0.clone());
        }
    }
    assert_eq!(b_ptrs.len(), 1, "exactly one `b` allocation across the entire result graph");
}

/// Version conflict: `root → {a, c}` with `a → b@1` and
/// `c → b@2`. The first BFS reach wins root's `b` slot; the
/// other version stays under its declaring parent.
#[test]
fn version_conflict_keeps_loser_at_parent() {
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));
    root_deps.insert(pkg_name("c"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    // a@1 → b@1, c@1 → b@2.
    let mut snapshots = HashMap::new();
    let mut a_deps = HashMap::new();
    a_deps.insert(pkg_name("b"), SnapshotDepRef::Plain(ver_peer("1.0.0")));
    let mut c_deps = HashMap::new();
    c_deps.insert(pkg_name("b"), SnapshotDepRef::Plain(ver_peer("2.0.0")));
    snapshots.insert(
        dep_key("a", "1.0.0"),
        SnapshotEntry { dependencies: Some(a_deps), ..SnapshotEntry::default() },
    );
    snapshots.insert(
        dep_key("c", "1.0.0"),
        SnapshotEntry { dependencies: Some(c_deps), ..SnapshotEntry::default() },
    );
    snapshots.insert(dep_key("b", "1.0.0"), SnapshotEntry::default());
    snapshots.insert(dep_key("b", "2.0.0"), SnapshotEntry::default());

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: None,
        snapshots: Some(snapshots),
    };

    let result = hoist(&lockfile, &HoistOpts::default()).expect("hoist should succeed");
    let root_children = result.dependencies.borrow();
    let mut names: Vec<&str> = root_children.iter().map(|d| d.0.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a", "b", "c"], "root has a, c, and one b");
    let b_at_root = root_children.iter().find(|d| d.0.name == "b").unwrap().0.clone();
    // The first BFS visit from root iterates root's direct deps in
    // alias order (`a` then `c`), so `a@1`'s `b@1.0.0` reaches
    // root first and wins the slot. Assert membership (not
    // iteration-order-derived equality) so the test stays focused
    // on which reference is present, not on which one happens to
    // come back first from the set.
    let b_refs = b_at_root.references.borrow();
    assert!(b_refs.contains("b@1.0.0"), "first BFS visitor wins root slot: {b_refs:?}");
    assert_eq!(b_refs.len(), 1, "no other reference accumulated yet: {b_refs:?}");
    // `c`'s `b@2` remains under `c`.
    let c = root_children.iter().find(|d| d.0.name == "c").unwrap().0.clone();
    let c_kids = c.dependencies.borrow();
    assert_eq!(c_kids.len(), 1, "c kept its conflicting b@2");
    let b_under_c_refs = c_kids[0].0.references.borrow();
    assert!(b_under_c_refs.contains("b@2.0.0"), "loser stays under c: {b_under_c_refs:?}");
    assert_eq!(b_under_c_refs.len(), 1);
}

/// Deep linear chain `root → a → b → c → d` flattens to
/// `root → {a, b, c, d}` in a single BFS pass: each node, once
/// queued, sees the previously-hoisted nodes as direct children
/// of root, so its own children evaluate against root's slots
/// (which are all free).
#[test]
fn deep_chain_flattens_in_one_pass() {
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    let mut snapshots = HashMap::new();
    let mut a_deps = HashMap::new();
    a_deps.insert(pkg_name("b"), SnapshotDepRef::Plain(ver_peer("1.0.0")));
    let mut b_deps = HashMap::new();
    b_deps.insert(pkg_name("c"), SnapshotDepRef::Plain(ver_peer("1.0.0")));
    let mut c_deps = HashMap::new();
    c_deps.insert(pkg_name("d"), SnapshotDepRef::Plain(ver_peer("1.0.0")));
    snapshots.insert(
        dep_key("a", "1.0.0"),
        SnapshotEntry { dependencies: Some(a_deps), ..SnapshotEntry::default() },
    );
    snapshots.insert(
        dep_key("b", "1.0.0"),
        SnapshotEntry { dependencies: Some(b_deps), ..SnapshotEntry::default() },
    );
    snapshots.insert(
        dep_key("c", "1.0.0"),
        SnapshotEntry { dependencies: Some(c_deps), ..SnapshotEntry::default() },
    );
    snapshots.insert(dep_key("d", "1.0.0"), SnapshotEntry::default());

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: None,
        snapshots: Some(snapshots),
    };

    let result = hoist(&lockfile, &HoistOpts::default()).expect("hoist should succeed");
    let root_children = result.dependencies.borrow();
    let mut names: Vec<&str> = root_children.iter().map(|d| d.0.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a", "b", "c", "d"], "depth-4 chain flattens: {result:#?}");
    for entry in root_children.iter() {
        assert!(entry.0.dependencies.borrow().is_empty(), "{} has no nested deps", entry.0.name);
    }
}

/// `external_dependencies` are added as `link:` placeholders at the
/// root so the inner hoister won't hoist anything else into those
/// name slots, and they're stripped from the result after hoisting.
/// Pin both: the placeholder doesn't leak into the result, and any
/// real package the lockfile contributes still does.
#[test]
fn external_dependencies_are_stripped_from_the_result() {
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("real"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    let mut snapshots = HashMap::new();
    snapshots.insert(dep_key("real", "1.0.0"), SnapshotEntry::default());

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: None,
        snapshots: Some(snapshots),
    };

    let opts = HoistOpts {
        external_dependencies: ["bit-managed".to_string()].into_iter().collect(),
        ..HoistOpts::default()
    };
    let result = hoist(&lockfile, &opts).expect("hoist should succeed");
    let names: Vec<String> = result.dependencies.borrow().iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, ["real"], "external dep is stripped, real dep remains: {names:?}");
}

/// A transitive npm-alias dep (`SnapshotDepRef::Alias`) must look
/// up the snapshot under the *target* package name, not under the
/// alias. Regression for the wrapper's earlier bug where the
/// snapshot key was reconstructed from `(alias, suffix)` instead
/// of the resolved key — that produced
/// `LockfileMissingDependency` on real npm-aliased transitives.
#[test]
fn transitive_npm_alias_resolves_target_snapshot() {
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("host"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    let mut snapshots = HashMap::new();
    // `host@1.0.0` depends on `aliased-name` resolved to the
    // target snapshot `real-pkg@2.0.0` — i.e. an npm-alias.
    let mut host_deps = HashMap::new();
    host_deps.insert(pkg_name("aliased-name"), SnapshotDepRef::Alias(dep_key("real-pkg", "2.0.0")));
    snapshots.insert(
        dep_key("host", "1.0.0"),
        SnapshotEntry { dependencies: Some(host_deps), ..SnapshotEntry::default() },
    );
    // Snapshot lookup must target `real-pkg@2.0.0`, NOT
    // `aliased-name@2.0.0`. If we put only the target key in the
    // map, the wrapper succeeds; if it builds the key from the
    // alias the lookup misses and we get
    // `LockfileMissingDependency`.
    snapshots.insert(dep_key("real-pkg", "2.0.0"), SnapshotEntry::default());

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: None,
        snapshots: Some(snapshots),
    };

    let result =
        hoist(&lockfile, &HoistOpts::default()).expect("aliased transitive should resolve");
    // After hoist, both `host` and `aliased-name` sit at root —
    // `aliased-name` had no conflict so it floats up. The npm-
    // alias indirection is observable on the hoisted node itself:
    // `name` is the exposed alias, `ident_name` and `references`
    // carry the resolved target's identity.
    let root_children = result.dependencies.borrow();
    let mut names: Vec<&str> = root_children.iter().map(|d| d.0.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["aliased-name", "host"]);
    let aliased = root_children
        .iter()
        .find(|d| d.0.name == "aliased-name")
        .expect("aliased-name hoisted")
        .0
        .clone();
    assert_eq!(aliased.name, "aliased-name");
    assert_eq!(aliased.ident_name, "real-pkg");
    let refs = aliased.references.borrow();
    assert!(
        refs.contains("real-pkg@2.0.0"),
        "reference is the resolved snapshot key, not the alias: {refs:?}",
    );
    assert_eq!(refs.len(), 1);
    let host = root_children.iter().find(|d| d.0.name == "host").unwrap().0.clone();
    assert!(host.dependencies.borrow().is_empty(), "host stripped of its aliased dep: {host:#?}");
}

/// A package with `peer_dependencies` declared in the lockfile's
/// `packages:` map must surface `UnsupportedPeerDependency` rather
/// than silently hoist past parents that supply the peer. The
/// algorithm doesn't model peer constraints today; refusing
/// upfront keeps a future caller from getting a wrong layout.
#[test]
fn peer_dependency_in_lockfile_surfaces_unsupported() {
    use pacquet_lockfile::{LockfileResolution, PackageMetadata, TarballResolution};
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("widget"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    // `widget@1.0.0` declares `react` as a peer dep — the
    // `packages:` map carries that information at the
    // `name@version` (peer-stripped) key.
    let mut packages = HashMap::new();
    let mut peer_deps = HashMap::new();
    peer_deps.insert("react".to_string(), "^18".to_string());
    packages.insert(
        dep_key("widget", "1.0.0").without_peer(),
        PackageMetadata {
            resolution: LockfileResolution::Tarball(TarballResolution {
                tarball: "https://example.invalid/widget-1.0.0.tgz".to_string(),
                integrity: None,
                git_hosted: None,
                path: None,
            }),
            engines: None,
            cpu: None,
            os: None,
            libc: None,
            deprecated: None,
            has_bin: None,
            prepare: None,
            bundled_dependencies: None,
            peer_dependencies: Some(peer_deps),
            peer_dependencies_meta: None,
        },
    );

    let mut snapshots = HashMap::new();
    snapshots.insert(dep_key("widget", "1.0.0"), SnapshotEntry::default());

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: Some(packages),
        snapshots: Some(snapshots),
    };

    let err = hoist(&lockfile, &HoistOpts::default()).expect_err("peer dep should bail");
    match err {
        HoistError::UnsupportedPeerDependency { ident, peers } => {
            assert_eq!(ident, "widget@1.0.0", "carries the offending ident: {ident}");
            assert!(peers.contains("react"), "carries the peer name set: {peers:?}");
        }
        other => panic!("expected UnsupportedPeerDependency, got {other:?}"),
    }
}

/// Non-empty `hoisting_limits` surfaces `UnsupportedHoistingLimits`.
/// The algorithm doesn't honor limits today, so flattening past
/// them would silently violate the borders the caller asked to
/// keep.
#[test]
fn non_empty_hoisting_limits_surfaces_unsupported() {
    let lockfile = empty_lockfile();
    let mut opts = HoistOpts::default();
    opts.hoisting_limits.insert(".@".to_string(), Default::default());

    let err = hoist(&lockfile, &opts).expect_err("hoisting_limits should bail");
    match err {
        HoistError::UnsupportedHoistingLimits { len } => assert_eq!(len, 1),
        other => panic!("expected UnsupportedHoistingLimits, got {other:?}"),
    }
}

/// A lockfile with importers beyond `.` (a workspace) surfaces
/// `UnsupportedWorkspace`. Multi-importer hoisting requires
/// workspace-aware traversal and a different output shape.
#[test]
fn multi_importer_lockfile_surfaces_unsupported_workspace() {
    let mut importers = HashMap::new();
    importers.insert(Lockfile::ROOT_IMPORTER_KEY.to_string(), ProjectSnapshot::default());
    importers.insert("packages/foo".to_string(), ProjectSnapshot::default());
    importers.insert("packages/bar".to_string(), ProjectSnapshot::default());

    let lockfile = Lockfile {
        lockfile_version: lockfile_version(),
        settings: None,
        overrides: None,
        importers,
        packages: None,
        snapshots: None,
    };

    let err = hoist(&lockfile, &HoistOpts::default()).expect_err("workspace should bail");
    match err {
        HoistError::UnsupportedWorkspace { mut extra_importers } => {
            extra_importers.sort();
            assert_eq!(extra_importers, vec!["packages/bar", "packages/foo"]);
        }
        other => panic!("expected UnsupportedWorkspace, got {other:?}"),
    }
}
