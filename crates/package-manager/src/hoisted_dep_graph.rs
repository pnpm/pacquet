//! Type skeleton for the directory-keyed dependency graph that
//! `nodeLinker: hoisted` installs produce. Ports the data shapes
//! from upstream's
//! [`installing/deps-restorer/src/lockfileToHoistedDepGraph.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts)
//! and the supporting types factored into
//! [`deps/graph-builder/src/lockfileToDepGraph.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts).
//!
//! The walker [`lockfile_to_hoisted_dep_graph`] takes a wanted
//! lockfile plus an optional *current* lockfile and runs
//! `pacquet_real_hoist::hoist` to get the directory shape, then
//! assembles a [`LockfileToDepGraphResult`] keyed by the computed
//! absolute directory of every node. Optional packages whose
//! `cpu` / `os` / `libc` / `engines` don't fit the current host
//! are added to `result.skipped` rather than emitted into the
//! graph; required incompatible packages proceed with a
//! (yet-to-be-wired) warning, matching upstream
//! `package_is_installable`'s `null | true | false` shape. When a
//! current lockfile is supplied, the walker runs a second
//! `force: true, skipped: empty` pass over it to populate
//! `result.prev_graph` — the input Slice 5's linker diffs against
//! to identify orphaned directories. Store I/O (`fetching` /
//! `files_index_file`) is still deferred — those fields are
//! populated by the linker, which kicks off store fetches when it
//! has a real consumer for the handles.
//!
//! Unlike the depPath-keyed [`crate::deps_graph`] module (which is
//! a hashing-side adapter for the build cache), the graph defined
//! here is keyed by *absolute directory path* — that's the
//! identity hoisted-linker nodes have, because the same package
//! can occupy several directories when a name conflict forces it
//! to nest. Hoisting decisions are made at directory granularity,
//! not depPath granularity.

use derive_more::{Display, Error, From};
use indexmap::IndexSet;
use miette::Diagnostic;
use pacquet_lockfile::{
    Lockfile, LockfileResolution, PackageKey, ParsePkgNameVerPeerError, PkgIdWithPatchHash,
};
use pacquet_modules_yaml::DepPath;
use pacquet_package_is_installable::{
    InstallabilityError, InstallabilityOptions, InstallabilityVerdict,
    PackageInstallabilityManifest, SupportedArchitectures, WantedEngine, package_is_installable,
};
use pacquet_patching::PatchInfo;
use pacquet_real_hoist::{HoistError, HoistOpts, HoisterResult, RcByPtr, hoist};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
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
    /// the side-effects cache uses. Ported as
    /// [`pacquet_lockfile::PkgIdWithPatchHash`] — a non-validating
    /// branded newtype around `String` matching upstream's
    /// `string & { __brand: 'PkgIdWithPatchHash' }`.
    pub pkg_id_with_patch_hash: PkgIdWithPatchHash,
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
    /// Packages the walker decided to skip — the input
    /// `opts.skipped` extended with any depPaths whose
    /// installability check failed (optional + unsupported
    /// platform/engine). Upstream mutates the input `Set<string>`
    /// in place; pacquet returns the augmented set on the result
    /// so the caller can persist it into `.modules.yaml.skipped`
    /// without sharing mutable state.
    pub skipped: BTreeSet<String>,
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
    /// When true, suppress the installability check and emit every
    /// dep into the graph regardless of cpu / os / libc / engines.
    /// Used by the `prev_graph` walk (Slice 4d) where the previous
    /// lockfile is replayed wholesale to compute orphans — upstream
    /// passes `force: true, skipped: new Set()` to that call so
    /// the diff catches packages that previously installed but
    /// would now be filtered. Mirrors upstream's `force` at
    /// [lockfileToHoistedDepGraph.ts:73](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L73-L76).
    pub force: bool,
    /// `engineStrict` from config. When true, an engine mismatch on
    /// a *required* (non-optional) package becomes a hard error
    /// instead of a warning.
    pub engine_strict: bool,
    /// Current host's node version, used as the `engines.node`
    /// satisfiability target. See `InstallabilityOptions::current_node_version`.
    pub current_node_version: String,
    /// Current host's OS (`linux`, `darwin`, `win32`, ...).
    pub current_os: String,
    /// Current host's CPU architecture (`x64`, `arm64`, ...).
    pub current_cpu: String,
    /// Current host's libc variant (`glibc`, `musl`, or empty when
    /// the host is not Linux).
    pub current_libc: String,
    /// `supportedArchitectures` override from `pnpm-workspace.yaml`,
    /// widening the host-derived axes so a Linux host can prepare
    /// node_modules for a Windows / macOS target. `None` means use
    /// only the current-host axes.
    pub supported_architectures: Option<SupportedArchitectures>,
}

/// Failure modes of [`lockfile_to_hoisted_dep_graph`]. Marked
/// `#[non_exhaustive]` so adding variants in later sub-slices (the
/// installability filter, the store-fetch integration) isn't a
/// breaking API change.
#[derive(Debug, Display, Error, Diagnostic, From)]
#[non_exhaustive]
pub enum HoistedDepGraphError {
    /// The hoister refused the lockfile (broken snapshot,
    /// unsupported workspace, etc.). Surfaced verbatim so callers
    /// see the same error code as upstream.
    #[display("{_0}")]
    Hoist(#[error(source)] HoistError),
    /// A `HoisterResult` node carried a reference string that
    /// doesn't parse as a `name@version[(peers)]` package key.
    /// Should never happen for hoister output produced from a
    /// valid lockfile — the hoister only emits references it
    /// already validated — but the conversion is fallible at the
    /// type level, so a typed error is the honest surface.
    #[display("Unparsable snapshot reference {reference:?} on hoisted node")]
    #[diagnostic(code(ERR_PACQUET_HOISTED_GRAPH_BAD_REFERENCE))]
    BadReference {
        reference: String,
        #[error(source)]
        source: ParsePkgNameVerPeerError,
    },
    /// A required (non-optional) package failed the
    /// installability check. Mirrors upstream's `throw` path
    /// where `engineStrict` + an engine mismatch surfaces as
    /// `ERR_PNPM_UNSUPPORTED_ENGINE`; the inner
    /// `InstallabilityError` is propagated transparently so
    /// callers see the same diagnostic code
    /// (`ERR_PNPM_UNSUPPORTED_ENGINE` /
    /// `ERR_PNPM_UNSUPPORTED_PLATFORM` /
    /// `ERR_PNPM_INVALID_NODE_VERSION`) as upstream, and the
    /// inner error already carries the package id for context.
    ///
    /// Optional packages on incompatible platforms do *not* take
    /// this path — they are added to `result.skipped` and
    /// silently skipped, matching upstream's
    /// `pnpm:skipped-optional-dependency` semantics.
    #[display("{_0}")]
    #[diagnostic(transparent)]
    Installability(#[error(source)] Box<InstallabilityError>),
}

/// Build a directory-keyed [`LockfileToDepGraphResult`] from a
/// wanted lockfile, plus an optional *current* lockfile to diff
/// against. Ports upstream's
/// [`lockfileToHoistedDepGraph`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L65-L85).
///
/// When `current_lockfile` is `Some` and the lockfile has a
/// non-empty `packages:` map, the function runs an extra pass
/// over that lockfile with `force: true` and `skipped: empty` to
/// produce `prev_graph` — the graph the previous install
/// produced, used by Slice 5's linker to identify orphaned
/// directories. Mirrors upstream's pre-await branch at
/// [`lockfileToHoistedDepGraph.ts:70-79`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L70-L79).
///
/// The store-controller-bound `fetching` / `files_index_file`
/// fields on each graph node remain default-valued — those are
/// populated by Slice 5's linker, which kicks off the actual
/// store fetches when it has a real consumer for the handles.
///
/// Single-importer only today — multi-importer (workspace)
/// lockfiles surface as `HoistError::UnsupportedWorkspace` via
/// the hoister.
pub fn lockfile_to_hoisted_dep_graph(
    lockfile: &Lockfile,
    current_lockfile: Option<&Lockfile>,
    opts: &LockfileToHoistedDepGraphOptions,
) -> Result<LockfileToDepGraphResult, HoistedDepGraphError> {
    // Prev-graph walk: forced (every snapshot in the current
    // lockfile must surface so the diff catches packages that
    // would now fail installability) and unskipped (the previous
    // install's `skipped` is irrelevant — we want the full
    // previous layout to compute orphans against).
    let prev_graph = match current_lockfile {
        // Require a non-empty `packages` map. Upstream's
        // `currentLockfile?.packages != null` guard only filters
        // out `null` / `undefined` — but for an empty `packages:
        // {}` the inner walk produces an empty graph too, which
        // is observationally equivalent to "no orphans to
        // consider". Pacquet collapses both null and empty into
        // `prev_graph: None` so the API contract is unambiguous
        // and the empty case skips the (no-op) second walk.
        Some(current) if current.packages.as_ref().is_some_and(|packages| !packages.is_empty()) => {
            let prev_opts = LockfileToHoistedDepGraphOptions {
                force: true,
                skipped: BTreeSet::new(),
                ..opts.clone()
            };
            Some(build_dep_graph(current, &prev_opts)?.graph)
        }
        _ => None,
    };

    let mut result = build_dep_graph(lockfile, opts)?;
    result.prev_graph = prev_graph;
    Ok(result)
}

/// Inner builder: runs the hoister + walker for one lockfile and
/// returns the per-walk subset of [`LockfileToDepGraphResult`]
/// (everything except `prev_graph`, which only the outer wrapper
/// sets). Mirrors upstream's private `_lockfileToHoistedDepGraph`
/// at [`lockfileToHoistedDepGraph.ts:91-127`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L91-L127).
fn build_dep_graph(
    lockfile: &Lockfile,
    opts: &LockfileToHoistedDepGraphOptions,
) -> Result<LockfileToDepGraphResult, HoistedDepGraphError> {
    let hoist_opts =
        HoistOpts { auto_install_peers: opts.auto_install_peers, ..HoistOpts::default() };
    let hoister_result = hoist(lockfile, &hoist_opts)?;

    let modules_dir = opts.lockfile_dir.join("node_modules");
    let mut state = WalkState {
        lockfile,
        lockfile_dir: &opts.lockfile_dir,
        opts,
        skipped: opts.skipped.clone(),
        graph: DependenciesGraph::new(),
        pkg_locations_by_dep_path: BTreeMap::new(),
        hoisted_locations: BTreeMap::new(),
        injection_targets_by_dep_path: BTreeMap::new(),
    };
    let root_deps = hoister_result.dependencies.borrow();
    let root_hierarchy = walk_deps(&mut state, &modules_dir, &root_deps)?;
    drop(root_deps);

    // Pass 2 — fill in each node's `children` map from the
    // now-complete `pkg_locations_by_dep_path`. Mirrors upstream's
    // post-await `graph[dir].children = getChildren(...)` line.
    //
    // The walk above intentionally leaves `children` empty: in
    // upstream's parallel-async walker, every sibling and
    // descendant of a node has its directory pushed to
    // `pkgLocationsByDepPath` during the sync prologue of its
    // `async (dep) => { ... }` body, *before* any continuation
    // (the post-recursion `getChildren` call) runs. So by the
    // time any node computes its children, the location index is
    // already complete. Pacquet runs synchronously, so the
    // simplest way to preserve that invariant is to insert
    // everything first and resolve children second.
    let WalkState {
        graph,
        pkg_locations_by_dep_path,
        hoisted_locations,
        injection_targets_by_dep_path,
        skipped,
        lockfile,
        ..
    } = state;
    let mut graph = graph;
    fill_children(&mut graph, &pkg_locations_by_dep_path, lockfile)?;

    // The hoister produced a children order; the directory keys in
    // `root_hierarchy` follow it. `direct_dependencies_by_importer_id["."]`
    // mirrors upstream's `directDepsMap` at
    // <https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L139-L145>.
    let mut direct_deps_root: BTreeMap<String, PathBuf> = BTreeMap::new();
    for child_dir in root_hierarchy.0.keys() {
        if let Some(alias) = graph.get(child_dir).and_then(|node| node.alias.as_deref()) {
            direct_deps_root.insert(alias.to_string(), child_dir.clone());
        }
    }
    let mut direct_dependencies_by_importer_id: DirectDependenciesByImporterId = BTreeMap::new();
    direct_dependencies_by_importer_id
        .insert(Lockfile::ROOT_IMPORTER_KEY.to_string(), direct_deps_root);

    let mut hierarchy = BTreeMap::new();
    hierarchy.insert(opts.lockfile_dir.clone(), root_hierarchy);

    Ok(LockfileToDepGraphResult {
        graph,
        direct_dependencies_by_importer_id,
        hierarchy,
        hoisted_locations,
        symlinked_direct_dependencies_by_importer_id: DirectDependenciesByImporterId::new(),
        prev_graph: None,
        injection_targets_by_dep_path,
        skipped,
    })
}

/// Second walker pass: with every node's directory already in
/// `pkg_locations`, resolve each graph node's `children: alias →
/// dir` map by looking up the node's snapshot in the lockfile.
fn fill_children(
    graph: &mut DependenciesGraph,
    pkg_locations: &BTreeMap<String, Vec<PathBuf>>,
    lockfile: &Lockfile,
) -> Result<(), HoistedDepGraphError> {
    let dirs: Vec<PathBuf> = graph.keys().cloned().collect();
    for dir in dirs {
        let reference = graph[&dir].dep_path.as_str().to_string();
        let pkg_key: PackageKey = match reference.parse() {
            Ok(key) => key,
            Err(source) => {
                return Err(HoistedDepGraphError::BadReference { reference, source });
            }
        };
        let snapshot = lockfile.snapshots.as_ref().and_then(|m| m.get(&pkg_key));
        let children = compute_children(snapshot, pkg_locations);
        if let Some(node) = graph.get_mut(&dir) {
            node.children = children;
        }
    }
    Ok(())
}

/// Mutable scratch space the recursive walker threads through
/// every level. Borrowing the lockfile + lockfile_dir + opts up
/// front avoids passing four separate arguments. `skipped` is
/// owned (cloned from `opts.skipped`) because the walker mutates
/// it — every dep that fails the installability check gets added.
struct WalkState<'a> {
    lockfile: &'a Lockfile,
    lockfile_dir: &'a Path,
    opts: &'a LockfileToHoistedDepGraphOptions,
    skipped: BTreeSet<String>,
    graph: DependenciesGraph,
    /// Records every directory each depPath landed in, in visit
    /// order. The first entry wins for parent → child wiring (see
    /// upstream `getChildren`).
    pkg_locations_by_dep_path: BTreeMap<String, Vec<PathBuf>>,
    hoisted_locations: BTreeMap<String, Vec<String>>,
    injection_targets_by_dep_path: BTreeMap<String, Vec<PathBuf>>,
}

/// Recursive walker over `HoisterResult.dependencies`. Mirrors
/// upstream's
/// [`fetchDeps`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L168-L296)
/// minus the store-fetch / installability path; here the walker
/// only computes node identity, location, children, and
/// hoisted-location records.
///
/// No cycle detection — matches upstream's recursion shape and
/// trusts the hoister to produce a DAG. The hoister's own
/// cyclic-input tests pin that property.
fn walk_deps(
    state: &mut WalkState<'_>,
    modules: &Path,
    deps: &IndexSet<RcByPtr<HoisterResult>>,
) -> Result<DepHierarchy, HoistedDepGraphError> {
    let mut hierarchy: BTreeMap<PathBuf, DepHierarchy> = BTreeMap::new();
    for dep in deps {
        // The hoister keeps every absorbed reference; the first
        // (alphabetically smallest) is the canonical depPath for
        // this node's location. Mirrors upstream's
        // `Array.from(dep.references)[0]`.
        let reference = match dep.0.references.borrow().iter().next().cloned() {
            Some(r) => r,
            None => continue,
        };

        if state.skipped.contains(&reference) || reference.starts_with("workspace:") {
            continue;
        }

        let pkg_key: PackageKey = match reference.parse() {
            Ok(key) => key,
            Err(source) => {
                return Err(HoistedDepGraphError::BadReference { reference, source });
            }
        };

        // `packages[key]` is the metadata source; absent → this is
        // a link / external placeholder that the wrapper strips,
        // and the walker mirrors upstream's `if (!pkgSnapshot) return`
        // by skipping.
        let Some(metadata) = lookup_package_metadata(state.lockfile, &pkg_key) else {
            continue;
        };
        let snapshot =
            state.lockfile.snapshots.as_ref().and_then(|snapshots| snapshots.get(&pkg_key));

        // Installability filter. Mirrors upstream's
        // `if (!opts.force && packageIsInstallable(...) === false)`
        // at [lockfileToHoistedDepGraph.ts:200](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L200-L210).
        // `optional` comes from the snapshot — an optional dep on
        // an unsupported platform is silently added to `skipped`;
        // a required dep takes the error path.
        if !state.opts.force {
            let manifest = manifest_for_installability(metadata);
            let optional = snapshot.map(|s| s.optional).unwrap_or(false);
            let install_opts = InstallabilityOptions {
                engine_strict: state.opts.engine_strict,
                optional,
                current_node_version: &state.opts.current_node_version,
                pnpm_version: None,
                current_os: &state.opts.current_os,
                current_cpu: &state.opts.current_cpu,
                current_libc: &state.opts.current_libc,
                supported_architectures: state.opts.supported_architectures.as_ref(),
            };
            match package_is_installable(&pkg_key.to_string(), &manifest, &install_opts) {
                Ok(InstallabilityVerdict::Installable)
                | Ok(InstallabilityVerdict::ProceedWithWarning { .. }) => {}
                Ok(InstallabilityVerdict::SkipOptional { .. }) => {
                    state.skipped.insert(reference.clone());
                    continue;
                }
                Err(source) => {
                    return Err(HoistedDepGraphError::Installability(source));
                }
            }
        }

        let dir = modules.join(&dep.0.name);
        let dep_location = path_relative_to_lockfile_dir(&dir, state.lockfile_dir);

        // Insert *before* recursing — mirrors upstream's
        // `fetchDeps` body order (insert + push to pkgLocations,
        // then `await fetchDeps(...)`). `children` is filled in
        // by `fill_children` after the whole walk is done.
        let node = DependenciesGraphNode {
            alias: Some(dep.0.name.clone()),
            dep_path: DepPath::from(reference.clone()),
            pkg_id_with_patch_hash: PkgIdWithPatchHash::from(pkg_key.to_string()),
            dir: dir.clone(),
            modules: modules.to_path_buf(),
            children: BTreeMap::new(),
            name: pkg_key.name.to_string(),
            version: pkg_key.suffix.version().to_string(),
            optional: snapshot.map(|s| s.optional).unwrap_or(false),
            optional_dependencies: snapshot
                .and_then(|s| s.optional_dependencies.as_ref())
                .map(|m| m.keys().map(|k| k.to_string()).collect())
                .unwrap_or_default(),
            has_bin: metadata.has_bin.unwrap_or(false),
            has_bundled_dependencies: metadata.bundled_dependencies.is_some(),
            patch: None,
            resolution: metadata.resolution.clone(),
        };

        state.graph.insert(dir.clone(), node);
        state.pkg_locations_by_dep_path.entry(reference.clone()).or_default().push(dir.clone());

        // Directory resolutions are injected workspace packages.
        // Upstream records every dir an injected dep lands in for
        // the post-install re-mirror step; mirrored here so a
        // future re-mirror pass has the same input shape.
        if let LockfileResolution::Directory(_) = &metadata.resolution {
            state
                .injection_targets_by_dep_path
                .entry(reference.clone())
                .or_default()
                .push(dir.clone());
        }

        // Recurse into the children (records their pkg_locations
        // and produces their `DepHierarchy`).
        let inner_modules = dir.join("node_modules");
        let child_deps = dep.0.dependencies.borrow();
        let inner_hierarchy = walk_deps(state, &inner_modules, &child_deps)?;
        drop(child_deps);

        // `hoistedLocations` is pushed AFTER the recursion, matching
        // upstream. The pre-recursion sites that mutate state are
        // for graph/index identity; this one is the user-visible
        // location list that the linker consumes.
        state.hoisted_locations.entry(reference).or_default().push(dep_location);
        hierarchy.insert(dir, inner_hierarchy);
    }
    Ok(DepHierarchy(hierarchy))
}

/// Look up the metadata side of a snapshot. Pacquet stores
/// `packages` and `snapshots` separately; the walker needs the
/// metadata for resolution / has_bin / bundledDependencies (which
/// upstream pulls from `pkgSnapshot`).
fn lookup_package_metadata<'a>(
    lockfile: &'a Lockfile,
    key: &PackageKey,
) -> Option<&'a pacquet_lockfile::PackageMetadata> {
    lockfile.packages.as_ref()?.get(key)
}

/// Project the platform / engines axes from a `PackageMetadata`
/// onto the [`PackageInstallabilityManifest`] shape
/// [`package_is_installable`] consumes. Upstream builds this
/// inline at
/// [lockfileToHoistedDepGraph.ts:192-199](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L192-L199);
/// extracted here so the walker body stays small.
fn manifest_for_installability(
    metadata: &pacquet_lockfile::PackageMetadata,
) -> PackageInstallabilityManifest {
    let engines = metadata.engines.as_ref().map(|engines| WantedEngine {
        node: engines.get("node").cloned(),
        pnpm: engines.get("pnpm").cloned(),
    });
    PackageInstallabilityManifest {
        engines,
        cpu: metadata.cpu.clone(),
        os: metadata.os.clone(),
        libc: metadata.libc.clone(),
    }
}

/// Lockfile-relative path string, matching upstream's
/// `path.relative(lockfileDir, dir)`. Returns an empty string when
/// `dir == lockfile_dir`.
///
/// Backslashes are normalized to forward slashes so the value is
/// portable across platforms — `.modules.yaml.hoistedLocations`
/// is read on whatever OS the next install runs on, and pnpm's
/// `pnpm-lock.yaml` already uses forward slashes for the same
/// reason. Upstream's `path.relative` produces OS-native
/// separators (so `.modules.yaml` written on Windows technically
/// holds backslashes), but pacquet normalizes here for
/// cross-platform consistency with the rest of pnpm's serialised
/// formats.
fn path_relative_to_lockfile_dir(dir: &Path, lockfile_dir: &Path) -> String {
    dir.strip_prefix(lockfile_dir)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| dir.to_string_lossy().replace('\\', "/"))
}

/// Compute the `children: alias → dir` map for a node. Mirrors
/// upstream's
/// [`getChildren`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L320-L334):
/// look up every direct (and optional, with `include` always on
/// here) dep of the snapshot, resolve it to its depPath via
/// `SnapshotDepRef::resolve`, and take the first recorded
/// location.
fn compute_children(
    snapshot: Option<&pacquet_lockfile::SnapshotEntry>,
    pkg_locations: &BTreeMap<String, Vec<PathBuf>>,
) -> BTreeMap<String, PathBuf> {
    let mut children: BTreeMap<String, PathBuf> = BTreeMap::new();
    let Some(snapshot) = snapshot else { return children };

    let dep_iter = snapshot
        .dependencies
        .iter()
        .flatten()
        .chain(snapshot.optional_dependencies.iter().flatten());
    for (alias_name, dep_ref) in dep_iter {
        let child_key = dep_ref.resolve(alias_name);
        let child_dep_path = child_key.to_string();
        if let Some(locations) = pkg_locations.get(&child_dep_path)
            && let Some(first) = locations.first()
        {
            children.insert(alias_name.to_string(), first.clone());
        }
    }
    children
}

#[cfg(test)]
mod tests {
    use super::{
        DepHierarchy, DependenciesGraph, DependenciesGraphNode, LockfileToDepGraphResult,
        LockfileToHoistedDepGraphOptions,
    };
    use pacquet_lockfile::{DirectoryResolution, LockfileResolution, PkgIdWithPatchHash};
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
        assert!(actual.skipped.is_empty());
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
            pkg_id_with_patch_hash: PkgIdWithPatchHash::from(ACCEPTS_DEP_PATH),
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
        assert!(!opts.force);
        assert!(!opts.engine_strict);
        assert!(opts.current_node_version.is_empty());
        assert!(opts.supported_architectures.is_none());
    }

    // --- Walker tests ----------------------------------------------------

    use super::{HoistedDepGraphError, InstallabilityError, lockfile_to_hoisted_dep_graph};
    use pacquet_lockfile::{
        ComVer, Lockfile, LockfileSettings, LockfileVersion, PackageKey, PackageMetadata, PkgName,
        PkgNameVerPeer, PkgVerPeer, ProjectSnapshot, ResolvedDependencyMap, ResolvedDependencySpec,
        SnapshotDepRef, SnapshotEntry,
    };
    use std::collections::HashMap;

    fn lockfile_version() -> LockfileVersion<9> {
        LockfileVersion::<9>::try_from(ComVer::new(9, 0))
            .expect("lockfileVersion 9.0 is compatible")
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

    fn directory_resolution(directory: &str) -> LockfileResolution {
        DirectoryResolution { directory: directory.to_string() }.into()
    }

    /// Build a metadata stub for a package using a synthetic
    /// `directory:` resolution. Walker tests don't exercise
    /// resolution semantics — they only need *some* resolution so
    /// the graph node has a non-default value to inspect.
    fn metadata_stub() -> PackageMetadata {
        PackageMetadata {
            resolution: directory_resolution("/dev/null/stub"),
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

    fn lockfile_with(
        importer_deps: ResolvedDependencyMap,
        packages: HashMap<PackageKey, PackageMetadata>,
        snapshots: HashMap<PackageKey, SnapshotEntry>,
    ) -> Lockfile {
        let mut importers = HashMap::new();
        importers.insert(
            Lockfile::ROOT_IMPORTER_KEY.to_string(),
            ProjectSnapshot { dependencies: Some(importer_deps), ..ProjectSnapshot::default() },
        );
        Lockfile {
            lockfile_version: lockfile_version(),
            settings: Some(LockfileSettings::default()),
            overrides: None,
            ignored_optional_dependencies: None,
            importers,
            packages: Some(packages),
            snapshots: Some(snapshots),
        }
    }

    /// A lockfile with no importers walks to an empty graph and a
    /// hierarchy with no root entry. Mirrors the
    /// `empty_lockfile_yields_empty_root` case from the hoister.
    #[test]
    fn walker_empty_lockfile_produces_empty_result() {
        let lockfile = Lockfile {
            lockfile_version: lockfile_version(),
            settings: Some(LockfileSettings::default()),
            overrides: None,
            ignored_optional_dependencies: None,
            importers: HashMap::new(),
            packages: None,
            snapshots: None,
        };
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: PathBuf::from("/repo"),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&lockfile, None, &opts).expect("empty lockfile walks");

        assert!(result.graph.is_empty(), "graph should be empty");
        assert!(result.hoisted_locations.is_empty(), "no locations recorded");
        // `direct_dependencies_by_importer_id["."]` is always
        // present (the root importer is implicit), but its inner
        // map is empty when there are no children.
        assert_eq!(result.direct_dependencies_by_importer_id.len(), 1);
        assert!(result.direct_dependencies_by_importer_id[Lockfile::ROOT_IMPORTER_KEY].is_empty());
    }

    /// `root → a` with `a` having no transitive deps: walker emits
    /// a single graph node at `<lockfile_dir>/node_modules/a`,
    /// populates `hoisted_locations["a@1.0.0"]`, and records `a` as
    /// the root's only direct dep.
    #[test]
    fn walker_single_root_dep_emits_one_node() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_stub());

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let lockfile_dir = PathBuf::from("/repo");
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: lockfile_dir.clone(),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&lockfile, None, &opts).expect("walker succeeds");

        let expected_dir = lockfile_dir.join("node_modules").join("a");
        assert_eq!(
            result.graph.len(),
            1,
            "one node emitted: {:?}",
            result.graph.keys().collect::<Vec<_>>(),
        );
        let node = result.graph.get(&expected_dir).expect("node keyed by dir");
        assert_eq!(node.alias.as_deref(), Some("a"));
        assert_eq!(node.dep_path, DepPath::from("a@1.0.0".to_string()));
        assert_eq!(node.name, "a");
        assert_eq!(node.version, "1.0.0");

        assert_eq!(result.hoisted_locations["a@1.0.0"], vec!["node_modules/a".to_string()]);
        assert_eq!(
            result.direct_dependencies_by_importer_id[Lockfile::ROOT_IMPORTER_KEY]["a"],
            expected_dir,
        );
    }

    /// `root → a → b` (no name conflict): the hoister flattens `b`
    /// to root, and the walker emits two graph nodes — both under
    /// `<lockfile_dir>/node_modules/`. `a`'s `children["b"]` points
    /// at `b`'s root-level directory (not `a/node_modules/b`),
    /// because the hoister moved it there.
    #[test]
    fn walker_transitive_dep_flattens_under_root() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_stub());
        packages.insert(dep_key("b", "1.0.0"), metadata_stub());

        let mut snapshots = HashMap::new();
        let mut a_deps = HashMap::new();
        a_deps.insert(pkg_name("b"), SnapshotDepRef::Plain(ver_peer("1.0.0")));
        snapshots.insert(
            dep_key("a", "1.0.0"),
            SnapshotEntry { dependencies: Some(a_deps), ..SnapshotEntry::default() },
        );
        snapshots.insert(dep_key("b", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let lockfile_dir = PathBuf::from("/repo");
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: lockfile_dir.clone(),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&lockfile, None, &opts).expect("walker succeeds");

        let modules = lockfile_dir.join("node_modules");
        assert_eq!(
            result.graph.keys().cloned().collect::<Vec<_>>(),
            vec![modules.join("a"), modules.join("b")],
            "both nodes hoisted to root, sorted by dir",
        );
        let a_node = result.graph.get(&modules.join("a")).expect("a in graph");
        assert_eq!(
            a_node.children.get("b"),
            Some(&modules.join("b")),
            "a's `children[\"b\"]` points at the hoisted (root-level) dir",
        );

        // Both depPaths recorded at the root level only — no
        // nesting needed because there's no version conflict.
        assert_eq!(result.hoisted_locations["a@1.0.0"], vec!["node_modules/a".to_string()]);
        assert_eq!(result.hoisted_locations["b@1.0.0"], vec!["node_modules/b".to_string()]);
    }

    /// Version conflict: `root → {a@1, c}` plus `c → a@2`. `a@1`
    /// gets the root slot; `a@2` stays nested under `c`. The
    /// walker should record `a@1.0.0` at root and `a@2.0.0` at
    /// `node_modules/c/node_modules/a`.
    #[test]
    fn walker_version_conflict_keeps_loser_nested() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));
        root_deps.insert(pkg_name("c"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_stub());
        packages.insert(dep_key("a", "2.0.0"), metadata_stub());
        packages.insert(dep_key("c", "1.0.0"), metadata_stub());

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());
        snapshots.insert(dep_key("a", "2.0.0"), SnapshotEntry::default());
        let mut c_deps = HashMap::new();
        c_deps.insert(pkg_name("a"), SnapshotDepRef::Plain(ver_peer("2.0.0")));
        snapshots.insert(
            dep_key("c", "1.0.0"),
            SnapshotEntry { dependencies: Some(c_deps), ..SnapshotEntry::default() },
        );

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let lockfile_dir = PathBuf::from("/repo");
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: lockfile_dir.clone(),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&lockfile, None, &opts).expect("walker succeeds");

        let modules = lockfile_dir.join("node_modules");
        let a1_dir = modules.join("a");
        let c_dir = modules.join("c");
        let a2_dir = c_dir.join("node_modules").join("a");

        assert!(result.graph.contains_key(&a1_dir), "a@1 at root");
        assert!(result.graph.contains_key(&c_dir), "c at root");
        assert!(result.graph.contains_key(&a2_dir), "a@2 nested under c");

        assert_eq!(result.graph[&a1_dir].dep_path, DepPath::from("a@1.0.0".to_string()));
        assert_eq!(result.graph[&a2_dir].dep_path, DepPath::from("a@2.0.0".to_string()));

        assert_eq!(result.hoisted_locations["a@1.0.0"], vec!["node_modules/a".to_string()]);
        assert_eq!(
            result.hoisted_locations["a@2.0.0"],
            vec!["node_modules/c/node_modules/a".to_string()],
        );

        // `c`'s `children["a"]` points at the nested `a@2`, not the
        // root's `a@1` — because hoisting kept the nested slot.
        assert_eq!(result.graph[&c_dir].children.get("a"), Some(&a2_dir));
    }

    /// Pre-`skipped` packages aren't emitted into the graph at all.
    /// Upstream's walker honors the input `skipped` set without
    /// re-checking installability; pacquet's walker does the same.
    #[test]
    fn walker_honors_pre_skipped_dep_path() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_stub());

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let mut skipped = BTreeSet::new();
        skipped.insert("a@1.0.0".to_string());
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: PathBuf::from("/repo"),
            skipped,
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&lockfile, None, &opts).expect("walker succeeds");

        assert!(result.graph.is_empty(), "skipped dep not emitted");
        assert!(result.hoisted_locations.is_empty());
        assert!(
            result.skipped.contains("a@1.0.0"),
            "pre-skipped dep is still in the output skipped set",
        );
    }

    /// A `directory:` resolution gets recorded in
    /// `injection_targets_by_dep_path` so the post-install
    /// re-mirror step (a later sub-slice) can find it.
    #[test]
    fn walker_records_directory_resolution_as_injection_target() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(
            dep_key("a", "1.0.0"),
            PackageMetadata { resolution: directory_resolution("../local-a"), ..metadata_stub() },
        );

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let lockfile_dir = PathBuf::from("/repo");
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: lockfile_dir.clone(),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&lockfile, None, &opts).expect("walker succeeds");

        assert_eq!(
            result.injection_targets_by_dep_path["a@1.0.0"],
            vec![lockfile_dir.join("node_modules").join("a")],
        );
    }

    // --- Installability tests --------------------------------------------

    fn host_aware_opts() -> LockfileToHoistedDepGraphOptions {
        // Concrete platform values so the installability check has
        // something to compare against. The specific host doesn't
        // matter — tests assert relative behavior (compatible vs
        // incompatible) by setting metadata that targets *this*
        // value or its opposite.
        LockfileToHoistedDepGraphOptions {
            lockfile_dir: PathBuf::from("/repo"),
            current_node_version: "20.0.0".to_string(),
            current_os: "linux".to_string(),
            current_cpu: "x64".to_string(),
            current_libc: "glibc".to_string(),
            ..LockfileToHoistedDepGraphOptions::default()
        }
    }

    fn metadata_with_os(os: &str) -> PackageMetadata {
        PackageMetadata { os: Some(vec![os.to_string()]), ..metadata_stub() }
    }

    /// Optional package whose `os` constraint excludes the current
    /// host is added to `result.skipped` and never emitted into the
    /// graph. Mirrors upstream's
    /// `if (!opts.force && packageIsInstallable(...) === false) {
    /// opts.skipped.add(depPath); return; }`.
    #[test]
    fn walker_skips_optional_dep_on_unsupported_platform() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        // Linux host, package targets darwin only → unsupported.
        packages.insert(dep_key("a", "1.0.0"), metadata_with_os("darwin"));

        let mut snapshots = HashMap::new();
        snapshots.insert(
            dep_key("a", "1.0.0"),
            SnapshotEntry { optional: true, ..SnapshotEntry::default() },
        );

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let result = lockfile_to_hoisted_dep_graph(&lockfile, None, &host_aware_opts())
            .expect("walker succeeds");

        assert!(result.graph.is_empty(), "optional incompatible dep not emitted");
        assert!(
            result.skipped.contains("a@1.0.0"),
            "incompatible optional dep added to skipped: {:?}",
            result.skipped,
        );
        assert!(result.hoisted_locations.is_empty(), "no location recorded for skipped dep");
    }

    /// Required (non-optional) package on an unsupported platform
    /// proceeds with a warning rather than erroring — mirrors
    /// upstream `package_is_installable`'s `true` return for the
    /// required-incompatible case (only `engineStrict + engine
    /// mismatch` and `InvalidNodeVersion` actually throw). The
    /// warning log emit is out of scope here; the walker proceeds
    /// silently for now.
    #[test]
    fn walker_emits_required_dep_with_unsupported_platform_as_warning() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_with_os("darwin"));

        let mut snapshots = HashMap::new();
        // optional: false — upstream's `packageIsInstallable`
        // returns `true` (warn but proceed) rather than `false`.
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let result = lockfile_to_hoisted_dep_graph(&lockfile, None, &host_aware_opts())
            .expect("walker proceeds");

        assert_eq!(result.graph.len(), 1, "required incompatible dep emitted as warning");
        assert!(result.skipped.is_empty(), "required dep not added to skipped");
    }

    /// `engineStrict = true` + engine mismatch surfaces as
    /// `HoistedDepGraphError::Installability`. Mirrors upstream's
    /// `throw warn` path in `packageIsInstallable` when
    /// `engineStrict && warn instanceof UnsupportedEngineError`.
    #[test]
    fn walker_errors_on_engine_strict_mismatch() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut engines = HashMap::new();
        engines.insert("node".to_string(), ">=99.0.0".to_string());
        let mut packages = HashMap::new();
        packages.insert(
            dep_key("a", "1.0.0"),
            PackageMetadata { engines: Some(engines), ..metadata_stub() },
        );

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let opts = LockfileToHoistedDepGraphOptions { engine_strict: true, ..host_aware_opts() };
        let err = lockfile_to_hoisted_dep_graph(&lockfile, None, &opts)
            .expect_err("engine_strict + engine mismatch should error");
        match err {
            HoistedDepGraphError::Installability(inner) => match *inner {
                InstallabilityError::Engine(engine_err) => {
                    assert_eq!(engine_err.package_id, "a@1.0.0");
                }
                other => panic!("expected Engine variant, got {other:?}"),
            },
            other => panic!("expected Installability error, got {other:?}"),
        }
    }

    /// `opts.force = true` bypasses the installability check
    /// entirely — even a required dep on an unsupported platform
    /// passes through. Used by the `prev_graph` walk so the diff
    /// against the previous lockfile catches packages that
    /// previously installed but would now be filtered.
    #[test]
    fn walker_force_bypasses_installability_check() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_with_os("darwin"));

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let opts = LockfileToHoistedDepGraphOptions { force: true, ..host_aware_opts() };
        let result =
            lockfile_to_hoisted_dep_graph(&lockfile, None, &opts).expect("force bypasses check");

        assert_eq!(result.graph.len(), 1, "force=true emits the dep regardless of platform");
        assert!(result.skipped.is_empty(), "force=true doesn't add to skipped");
    }

    /// Compatible host: a package with metadata targeting the
    /// current host is emitted normally. Sanity check that the
    /// installability path doesn't drop packages it shouldn't.
    #[test]
    fn walker_emits_compatible_dep() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        // Linux host, package targets linux → compatible.
        packages.insert(dep_key("a", "1.0.0"), metadata_with_os("linux"));

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let result = lockfile_to_hoisted_dep_graph(&lockfile, None, &host_aware_opts())
            .expect("walker succeeds");

        assert_eq!(result.graph.len(), 1);
        assert!(result.skipped.is_empty());
    }

    // --- prev_graph tests ------------------------------------------------

    /// `current_lockfile: None` yields `prev_graph: None`. Mirrors
    /// upstream's `prevGraph = {}` fallback when no current
    /// lockfile is supplied — pacquet uses `None` instead of an
    /// empty map, but the linker treats the two the same way (no
    /// orphans to remove on a fresh install).
    #[test]
    fn prev_graph_none_when_current_lockfile_absent() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_stub());

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let lockfile = lockfile_with(root_deps, packages, snapshots);
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: PathBuf::from("/repo"),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&lockfile, None, &opts).expect("walker succeeds");

        assert!(result.prev_graph.is_none(), "no current_lockfile → no prev_graph");
        assert_eq!(result.graph.len(), 1, "wanted lockfile still produces the graph");
    }

    /// A current lockfile with no `packages:` map (a brand-new
    /// install in progress) also yields `prev_graph: None`.
    /// Mirrors upstream's `currentLockfile?.packages != null`
    /// guard.
    #[test]
    fn prev_graph_none_when_current_lockfile_has_no_packages() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_stub());

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let wanted = lockfile_with(root_deps, packages, snapshots);
        let current = Lockfile {
            lockfile_version: lockfile_version(),
            settings: Some(LockfileSettings::default()),
            overrides: None,
            ignored_optional_dependencies: None,
            importers: HashMap::new(),
            packages: None,
            snapshots: None,
        };
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: PathBuf::from("/repo"),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&wanted, Some(&current), &opts).expect("walker succeeds");

        assert!(result.prev_graph.is_none(), "current lockfile without packages → no prev_graph");
    }

    /// A current lockfile with `packages: Some(empty map)` also
    /// yields `prev_graph: None` — pacquet collapses null and
    /// empty into the same "no orphans" representation, since
    /// walking an empty `packages:` would just produce an empty
    /// graph anyway. Regression for the empty-map case Coderabbit
    /// flagged on the original 4d patch.
    #[test]
    fn prev_graph_none_when_current_lockfile_has_empty_packages() {
        let mut root_deps = ResolvedDependencyMap::new();
        root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));

        let mut packages = HashMap::new();
        packages.insert(dep_key("a", "1.0.0"), metadata_stub());

        let mut snapshots = HashMap::new();
        snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());

        let wanted = lockfile_with(root_deps, packages, snapshots);
        let current = Lockfile {
            lockfile_version: lockfile_version(),
            settings: Some(LockfileSettings::default()),
            overrides: None,
            ignored_optional_dependencies: None,
            importers: HashMap::new(),
            packages: Some(HashMap::new()),
            snapshots: Some(HashMap::new()),
        };
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: PathBuf::from("/repo"),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&wanted, Some(&current), &opts).expect("walker succeeds");

        assert!(
            result.prev_graph.is_none(),
            "current lockfile with empty packages → no prev_graph",
        );
    }

    /// A package present in the current lockfile but absent from
    /// the wanted lockfile surfaces in `prev_graph` while not
    /// appearing in `graph`. Slice 5's linker will subtract `graph`
    /// from `prev_graph` to find this directory and `rimraf` it.
    #[test]
    fn prev_graph_contains_orphan_from_current_only_lockfile() {
        // Current install: root → {a, orphan}
        let mut current_root_deps = ResolvedDependencyMap::new();
        current_root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));
        current_root_deps.insert(pkg_name("orphan"), resolved_dep("1.0.0"));
        let mut current_packages = HashMap::new();
        current_packages.insert(dep_key("a", "1.0.0"), metadata_stub());
        current_packages.insert(dep_key("orphan", "1.0.0"), metadata_stub());
        let mut current_snapshots = HashMap::new();
        current_snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());
        current_snapshots.insert(dep_key("orphan", "1.0.0"), SnapshotEntry::default());
        let current_lockfile =
            lockfile_with(current_root_deps, current_packages, current_snapshots);

        // Wanted install: root → {a} (orphan removed)
        let mut wanted_root_deps = ResolvedDependencyMap::new();
        wanted_root_deps.insert(pkg_name("a"), resolved_dep("1.0.0"));
        let mut wanted_packages = HashMap::new();
        wanted_packages.insert(dep_key("a", "1.0.0"), metadata_stub());
        let mut wanted_snapshots = HashMap::new();
        wanted_snapshots.insert(dep_key("a", "1.0.0"), SnapshotEntry::default());
        let wanted_lockfile = lockfile_with(wanted_root_deps, wanted_packages, wanted_snapshots);

        let lockfile_dir = PathBuf::from("/repo");
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: lockfile_dir.clone(),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&wanted_lockfile, Some(&current_lockfile), &opts)
                .expect("walker succeeds");

        let orphan_dir = lockfile_dir.join("node_modules").join("orphan");
        let a_dir = lockfile_dir.join("node_modules").join("a");

        let prev = result.prev_graph.expect("prev_graph populated");
        assert!(prev.contains_key(&orphan_dir), "orphan present in prev_graph");
        assert!(prev.contains_key(&a_dir), "carried-over dep also in prev_graph");
        assert!(result.graph.contains_key(&a_dir), "wanted graph carries a");
        assert!(!result.graph.contains_key(&orphan_dir), "wanted graph omits orphan");
    }

    /// The prev-graph walk uses `force: true, skipped: empty` so
    /// the *current* layout is preserved even for packages that
    /// would now fail installability. Mirrors upstream's
    /// `{ ...opts, force: true, skipped: new Set() }` override at
    /// [lockfileToHoistedDepGraph.ts:72-76](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/lockfileToHoistedDepGraph.ts#L72-L76).
    /// Without this, an orphan that targets an unsupported platform
    /// wouldn't appear in `prev_graph` and the linker would leave
    /// the stale directory in place.
    #[test]
    fn prev_graph_includes_orphan_even_when_now_incompatible() {
        // Current install had a darwin-targeting orphan dep that
        // landed on a host where the wanted install runs on linux.
        let mut current_root_deps = ResolvedDependencyMap::new();
        current_root_deps.insert(pkg_name("orphan"), resolved_dep("1.0.0"));
        let mut current_packages = HashMap::new();
        current_packages.insert(dep_key("orphan", "1.0.0"), metadata_with_os("darwin"));
        let mut current_snapshots = HashMap::new();
        current_snapshots.insert(dep_key("orphan", "1.0.0"), SnapshotEntry::default());
        let current_lockfile =
            lockfile_with(current_root_deps, current_packages, current_snapshots);

        // Wanted install: empty root.
        let wanted_lockfile =
            lockfile_with(ResolvedDependencyMap::new(), HashMap::new(), HashMap::new());

        let lockfile_dir = PathBuf::from("/repo");
        let opts = LockfileToHoistedDepGraphOptions {
            lockfile_dir: lockfile_dir.clone(),
            current_node_version: "20.0.0".to_string(),
            current_os: "linux".to_string(),
            current_cpu: "x64".to_string(),
            current_libc: "glibc".to_string(),
            ..LockfileToHoistedDepGraphOptions::default()
        };
        let result =
            lockfile_to_hoisted_dep_graph(&wanted_lockfile, Some(&current_lockfile), &opts)
                .expect("walker succeeds");

        let orphan_dir = lockfile_dir.join("node_modules").join("orphan");
        let prev = result.prev_graph.expect("prev_graph populated");
        assert!(
            prev.contains_key(&orphan_dir),
            "force: true emits the orphan even though it would now fail installability",
        );
        assert!(result.graph.is_empty(), "wanted graph stays empty");
        // The orphan must not appear in the wanted-walk's skipped
        // set either — its installability check was never run.
        assert!(result.skipped.is_empty(), "skipped from wanted walk only, not prev walk");
    }
}
