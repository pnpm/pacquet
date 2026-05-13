//! Type skeleton for the directory-keyed dependency graph that
//! `nodeLinker: hoisted` installs produce. Ports the data shapes
//! from upstream's
//! [`installing/deps-restorer/src/lockfileToHoistedDepGraph.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts)
//! and the supporting types factored into
//! [`deps/graph-builder/src/lockfileToDepGraph.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts).
//!
//! This module is types-only. The walker that produces a result
//! from a lockfile and a `pacquet_real_hoist::HoisterResult`
//! lands in a follow-up; the types are pinned here first so the
//! walker, the installability filter, and the eventual linker can
//! all be reviewed against a fixed shape.
//!
//! Unlike the depPath-keyed [`crate::deps_graph`] module (which is
//! a hashing-side adapter for the build cache), the graph defined
//! here is keyed by *absolute directory path* — that's the
//! identity hoisted-linker nodes have, because the same package
//! can occupy several directories when a name conflict forces it
//! to nest. Hoisting decisions are made at directory granularity,
//! not depPath granularity.

use pacquet_lockfile::LockfileResolution;
use pacquet_modules_yaml::DepPath;
use pacquet_patching::PatchInfo;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

/// One node in a hoisted-linker dependency graph. Keyed in the
/// outer [`DependenciesGraph`] by the node's absolute `dir`.
///
/// Mirrors upstream's
/// [`DependenciesGraphNode`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L38)
/// minus the store-controller-bound fields (`fetching`,
/// `files_index_file`) that the walker only learns about once it
/// calls `storeController.fetchPackage`. Those land in the
/// follow-up sub-slice that wires the store in; today, this type
/// pins the shape of every other field so the walker can fill
/// them without churning the call sites.
#[derive(Debug, Clone, PartialEq)]
pub struct DependenciesGraphNode {
    /// The alias this node was placed under in its parent's
    /// `node_modules`. Optional for parity with upstream — only
    /// populated when the node is reached via the hoist walk;
    /// upstream marks it `?` for the same reason.
    pub alias: Option<String>,
    /// The depPath that produced this node, used as the key for
    /// `hoistedLocations` and the join key for `hoistedDependencies`.
    pub dep_path: DepPath,
    /// Upstream's `pkgIdWithPatchHash`: the patch-aware ident key
    /// the side-effects cache uses. Kept as a plain `String` —
    /// matches the convention pacquet's `virtual_store_layout`
    /// module already uses for the same value.
    pub pkg_id_with_patch_hash: String,
    /// Absolute path of the package's directory on disk. The
    /// outer [`DependenciesGraph`]'s key is this same value;
    /// upstream stores it on the node too so consumers don't need
    /// to walk the map by reverse lookup.
    pub dir: PathBuf,
    /// Absolute path of the `node_modules/` directory the package
    /// lives in (i.e. `dir.parent()`). Used by the bin-linker
    /// pass: every hoist location needs `<modules>/.bin` populated.
    pub modules: PathBuf,
    /// Alias → child `dir` of this node's listed dependencies, as
    /// computed from the lockfile snapshot's `dependencies` and
    /// (when included) `optionalDependencies`. The walker resolves
    /// each child to the directory the alias was hoisted to —
    /// which may be the root, a sibling, or this node's own
    /// `node_modules`, depending on the hoister's decision.
    pub children: BTreeMap<String, PathBuf>,
    pub name: String,
    pub version: String,
    pub optional: bool,
    pub optional_dependencies: BTreeSet<String>,
    pub has_bin: bool,
    pub has_bundled_dependencies: bool,
    pub patch: Option<PatchInfo>,
    pub resolution: LockfileResolution,
}

/// Directory-keyed graph of every hoisted-linker node the walker
/// emitted. Mirrors upstream's
/// [`DependenciesGraph`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L60-L62).
pub type DependenciesGraph = BTreeMap<PathBuf, DependenciesGraphNode>;

/// Recursive directory hierarchy: each `node_modules` directory
/// maps to its children, which in turn map to their own
/// children's `node_modules`. The linker walks this to know which
/// directories to populate (and in what order) and which
/// `<dir>/node_modules/.bin` to wire up. Mirrors upstream's
/// [`DepHierarchy`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L98).
///
/// Wrapped in a newtype rather than typedef'd to a recursive
/// `BTreeMap` because Rust doesn't allow recursive type aliases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepHierarchy(pub BTreeMap<PathBuf, DepHierarchy>);

/// Per-importer alias → direct-dependency directory. For the
/// single-importer case the only key is `"."`; workspace support
/// will add per-importer entries keyed by the importer's
/// project id. Mirrors upstream's
/// [`DirectDependenciesByImporterId`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L94-L96).
pub type DirectDependenciesByImporterId = BTreeMap<String, BTreeMap<String, PathBuf>>;

/// Everything the walker hands back to the install pipeline.
///
/// Mirrors upstream's
/// [`LockfileToDepGraphResult`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L100-L108).
/// All fields are populated for the hoisted-linker path; the
/// isolated linker uses the same struct with `hierarchy`,
/// `hoisted_locations`, and `symlinked_direct_dependencies_by_importer_id`
/// left empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LockfileToDepGraphResult {
    pub graph: DependenciesGraph,
    pub direct_dependencies_by_importer_id: DirectDependenciesByImporterId,
    /// Outer key is the project root that owns the inner
    /// hierarchy (the workspace root for single-importer
    /// lockfiles, plus per-project roots once Slice 9 lands).
    pub hierarchy: BTreeMap<PathBuf, DepHierarchy>,
    /// Per-depPath list of lockfile-relative directory paths
    /// where the package landed. Round-trips through
    /// [`pacquet_modules_yaml::Modules::hoisted_locations`].
    ///
    /// Upstream literally types the values as `Record<string,
    /// string[]>` (not `Record<DepPath, string[]>`), even though
    /// the strings are populated from depPaths internally —
    /// mirrored here to keep the on-disk shape identical. The
    /// same choice was made for the `Modules` schema field this
    /// round-trips through (see its doc-comment in
    /// `pacquet-modules-yaml`).
    pub hoisted_locations: BTreeMap<String, Vec<String>>,
    pub symlinked_direct_dependencies_by_importer_id: DirectDependenciesByImporterId,
    /// Diffed against `graph` by the linker's orphan-removal pass
    /// to know which directories the previous install owned that
    /// the new install does not. `None` on a fresh install (no
    /// prior lockfile).
    pub prev_graph: Option<DependenciesGraph>,
    /// Per-depPath list of directories where the package is
    /// expected to live as an *injected* workspace package. Used
    /// by the post-install re-mirror step. Upstream is
    /// `Map<string, string[]>` (keys typed as raw `string`, not
    /// `DepPath`); mirrored here. See `injectionTargetsByDepPath`
    /// at
    /// [lockfileToHoistedDepGraph.ts:286](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L286-L292).
    pub injection_targets_by_dep_path: BTreeMap<String, Vec<PathBuf>>,
}

/// Inputs the walker reads from. Mirrors the subset of upstream's
/// [`LockfileToHoistedDepGraphOptions`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L34-L63)
/// pacquet needs for the hoisted-linker path that's actually
/// implemented today. Fields tied to the still-unported store
/// controller, fetch concurrency, or workspace project list will
/// be added when their consumers land.
#[derive(Debug, Clone, Default)]
pub struct LockfileToHoistedDepGraphOptions {
    /// Project / workspace root. Used as the base for relativizing
    /// `hoisted_locations` entries and for placing the root's
    /// `node_modules/` directory.
    pub lockfile_dir: PathBuf,
    /// `autoInstallPeers` from `.npmrc`. Passed through to the
    /// hoister, which zeroes every node's `peer_names` when this
    /// is `true` so peer-constrained packages float freely.
    pub auto_install_peers: bool,
    /// Packages the previous install decided not to fetch
    /// (installability check failed; the package was added here).
    /// The walker skips any depPath in this set without consulting
    /// the snapshot. Cloned + extended on the way out. Upstream's
    /// `LockfileToHoistedDepGraphOptions.skipped` is `Set<string>`
    /// (note: `Set<DepPath>` in the isolated-graph builder's
    /// options — pacquet matches the hoisted-specific typing
    /// here), so the wrapper here is `BTreeSet<String>`.
    pub skipped: BTreeSet<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        DepHierarchy, DependenciesGraph, DependenciesGraphNode, LockfileToDepGraphResult,
        LockfileToHoistedDepGraphOptions,
    };
    use pacquet_lockfile::{DirectoryResolution, LockfileResolution};
    use pacquet_modules_yaml::DepPath;
    use pretty_assertions::assert_eq;
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };

    fn sample_resolution() -> LockfileResolution {
        DirectoryResolution { directory: "../local-pkg".to_string() }.into()
    }

    /// Sample v9 depPath. v9 lockfiles use `name@version[(peers)]`
    /// (see `PkgNameVerPeer` in `pacquet-lockfile`); the v5-era
    /// `/name/version` shape is only kept for legacy
    /// `hoistedAliases` read-side compatibility.
    const ACCEPTS_DEP_PATH: &str = "accepts@1.3.7";

    /// `LockfileToDepGraphResult::default()` returns the empty
    /// graph the walker should emit when the lockfile has no
    /// importers — every collection is empty and `prev_graph` is
    /// `None` (no previous lockfile to diff against).
    #[test]
    fn default_result_is_empty() {
        let actual = LockfileToDepGraphResult::default();
        assert_eq!(actual.graph, DependenciesGraph::new());
        assert!(actual.direct_dependencies_by_importer_id.is_empty());
        assert!(actual.hierarchy.is_empty());
        assert!(actual.hoisted_locations.is_empty());
        assert!(actual.symlinked_direct_dependencies_by_importer_id.is_empty());
        assert!(actual.prev_graph.is_none());
        assert!(actual.injection_targets_by_dep_path.is_empty());
    }

    /// A `DependenciesGraphNode` can be constructed and inserted
    /// into a `DependenciesGraph` keyed by its `dir`. The walker
    /// will do exactly this for every package it visits; this
    /// test pins that the type composes correctly.
    #[test]
    fn graph_node_inserts_by_dir() {
        let dir = PathBuf::from("/repo/node_modules/accepts");
        let modules = PathBuf::from("/repo/node_modules");
        let node = DependenciesGraphNode {
            alias: Some("accepts".to_string()),
            dep_path: DepPath::from(ACCEPTS_DEP_PATH.to_string()),
            pkg_id_with_patch_hash: ACCEPTS_DEP_PATH.to_string(),
            dir: dir.clone(),
            modules,
            children: BTreeMap::new(),
            name: "accepts".to_string(),
            version: "1.3.7".to_string(),
            optional: false,
            optional_dependencies: BTreeSet::new(),
            has_bin: false,
            has_bundled_dependencies: false,
            patch: None,
            resolution: sample_resolution(),
        };

        let mut graph = DependenciesGraph::new();
        graph.insert(dir.clone(), node.clone());
        assert_eq!(graph.get(&dir), Some(&node));
    }

    /// `DepHierarchy` is a recursive map: a `node_modules`
    /// directory points to its child packages, which in turn
    /// expose their own `node_modules` directories. The newtype
    /// wrapper exists because Rust doesn't allow recursive type
    /// aliases; the nesting itself must round-trip through
    /// `Default`-construction and equality so the walker can
    /// assemble it bottom-up.
    #[test]
    fn hierarchy_nests_recursively() {
        let mut inner_children = BTreeMap::new();
        inner_children.insert(
            PathBuf::from("/repo/node_modules/accepts/node_modules/mime-types"),
            DepHierarchy::default(),
        );
        let inner = DepHierarchy(inner_children);

        let mut root_children = BTreeMap::new();
        root_children.insert(PathBuf::from("/repo/node_modules/accepts"), inner.clone());
        let root = DepHierarchy(root_children);

        let accepts =
            root.0.get(&PathBuf::from("/repo/node_modules/accepts")).expect("accepts entry");
        assert_eq!(accepts, &inner);
        assert_eq!(accepts.0.len(), 1);
    }

    /// `LockfileToHoistedDepGraphOptions::default()` produces the
    /// shape a no-op walker would accept: empty lockfile dir,
    /// `autoInstallPeers: false`, no pre-skipped packages.
    #[test]
    fn options_default_is_empty() {
        let opts = LockfileToHoistedDepGraphOptions::default();
        assert_eq!(opts.lockfile_dir, PathBuf::new());
        assert!(!opts.auto_install_peers);
        assert!(opts.skipped.is_empty());
    }
}
