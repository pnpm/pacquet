use super::{HoistError, HoistOpts, hoist};
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
    let HoistError::LockfileMissingDependency { pkg_key } = err;
    assert_eq!(pkg_key, "foo@1.0.0");
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

/// A minimal lockfile with one direct dependency and one
/// transitive: `root → a@1 → b@1`. With the stub `nm_hoist` no
/// hoisting happens, so the result must have `a` as the only
/// root child and `b` under it. Pins the current shape so that
/// when the stub is replaced with the real algorithm the tree
/// changing to `root → {a, b}` is an observable diff in this
/// test rather than a silent behaviour change elsewhere.
#[test]
fn one_transitive_dep_appears_under_its_parent_in_the_stub() {
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
    assert_eq!(root_children.len(), 1, "root has one child: {result:#?}");
    let a = root_children[0].0.clone();
    assert_eq!(a.name, "a");
    let a_children = a.dependencies.borrow();
    assert_eq!(a_children.len(), 1, "a has one child under the stub: {a:#?}");
    let b = a_children[0].0.clone();
    assert_eq!(b.name, "b");
    assert!(b.dependencies.borrow().is_empty(), "b has no transitive deps");
}

/// Two importers pointing at the same package version share a
/// single `HoisterTree` node by identity — same as JS's
/// `Set<HoisterTree>` semantics. Pinning this stops a future
/// refactor of `build_dep_node`'s `nodes` cache from silently
/// breaking the dedup that the real hoist algorithm relies on.
#[test]
fn shared_dep_via_two_root_paths_is_one_node() {
    let mut importers = HashMap::new();
    let mut root_deps = ResolvedDependencyMap::new();
    root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));
    root_deps.insert(pkg_name("c"), resolved_dep("1.0.0"));
    importers.insert(
        Lockfile::ROOT_IMPORTER_KEY.to_string(),
        ProjectSnapshot { dependencies: Some(root_deps), ..ProjectSnapshot::default() },
    );

    // Both a@1 and c@1 depend on b@1.
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
    let a = root_children.iter().find(|d| d.name == "a").expect("a present").0.clone();
    let c = root_children.iter().find(|d| d.name == "c").expect("c present").0.clone();
    let a_kids = a.dependencies.borrow();
    let c_kids = c.dependencies.borrow();
    let b_under_a = a_kids[0].0.clone();
    let b_under_c = c_kids[0].0.clone();
    // Identity comparison: two `b` references surfaced through
    // different paths must point at the same allocation — proving
    // the cache shared the node.
    assert!(Rc::ptr_eq(&b_under_a, &b_under_c), "b should be the same Rc'd HoisterResult node");
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
    let root_children = result.dependencies.borrow();
    let host = root_children.iter().find(|d| d.name == "host").expect("host present").0.clone();
    let host_kids = host.dependencies.borrow();
    let aliased = host_kids[0].0.clone();
    // The exposed name stays the alias the parent uses...
    assert_eq!(aliased.name, "aliased-name");
    // ...but the underlying identity is the target package.
    assert_eq!(aliased.ident_name, "real-pkg");
    assert_eq!(
        aliased.references.borrow().iter().next().map(String::as_str),
        Some("real-pkg@2.0.0"),
        "reference is the resolved snapshot key, not the alias",
    );
}
