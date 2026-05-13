//! Real-directory hoister for the `nodeLinker: hoisted` install layout.
//!
//! Ports pnpm v11's [`installing/linking/real-hoist`][upstream-wrapper]
//! package, which is itself a thin wrapper around the
//! [`@yarnpkg/nm/hoist`][yarn-hoist] algorithm. The wrapper translates a
//! pnpm lockfile into a [`HoisterTree`] (rooted at `.` with one child
//! per workspace importer), runs the algorithm, and post-filters
//! `externalDependencies` out of the top-level result.
//!
//! [upstream-wrapper]: https://github.com/pnpm/pnpm/blob/94240bc0464196bd52f7006b97f6d9a43df34633/installing/linking/real-hoist/src/index.ts
//! [yarn-hoist]: https://github.com/yarnpkg/berry/blob/4287909fa6a0a1ec976a55776bff606864b31990/packages/yarnpkg-nm/sources/hoist.ts

use derive_more::{Display, Error};
use indexmap::IndexSet;
use miette::Diagnostic;
use pacquet_lockfile::{Lockfile, PkgName, PkgNameVerPeer, ProjectSnapshot, SnapshotEntry};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    rc::Rc,
};

/// One of the three node categories the `@yarnpkg/nm` hoister
/// distinguishes. Mirrors `HoisterDependencyKind` at the
/// [yarn source][yarn-kind].
///
/// [yarn-kind]: https://github.com/yarnpkg/berry/blob/4287909fa6a0a1ec976a55776bff606864b31990/packages/yarnpkg-nm/sources/hoist.ts#L12-L14
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoisterDependencyKind {
    /// A normal package — eligible for hoisting.
    Regular,
    /// A workspace project. The root `.` node is one of these; each
    /// non-root importer is added under it as another `Workspace`
    /// node. Workspace nodes never hoist past their declared slot.
    Workspace,
    /// A package linked from outside the lockfile graph (e.g. a
    /// `link:` ref). Only hoists when *all* of its descendants
    /// hoist, and triggers another round when any do — see
    /// [`hoist.ts:416`][soft-link].
    ///
    /// [soft-link]: https://github.com/yarnpkg/berry/blob/4287909fa6a0a1ec976a55776bff606864b31990/packages/yarnpkg-nm/sources/hoist.ts#L416
    ExternalSoftLink,
}

/// Input node for the hoister. Built by [`hoist`] from the lockfile.
///
/// Mirrors `HoisterTree` at the [yarn source][yarn-tree]. Children
/// are stored in an [`IndexSet`] so insertion order is preserved (the
/// upstream hoister's traversal relies on declaration order to break
/// ties between equivalent candidates), and so that a node added via
/// two parent paths is shared by `Rc` identity the way JS's
/// `Set<HoisterTree>` shares by object identity.
///
/// `dependencies` is behind a [`RefCell`] so the construction phase
/// can stash a placeholder `Rc<HoisterTree>` for cycle short-circuit,
/// recurse, then populate the children in place. The placeholder Rc
/// and the populated one are the same allocation, so a node visited
/// via a back-edge sees the eventually-populated set — matching JS's
/// `Set<HoisterTree>` mutation semantics. The same interior
/// mutability is what the hoister algorithm will use to move children
/// between parents when it lands.
///
/// [yarn-tree]: https://github.com/yarnpkg/berry/blob/4287909fa6a0a1ec976a55776bff606864b31990/packages/yarnpkg-nm/sources/hoist.ts#L16-L19
#[derive(Debug)]
pub struct HoisterTree {
    /// The alias the package is exposed under at *this* parent —
    /// what would appear as the directory name in `node_modules`.
    /// For npm-alias deps (`"foo": "npm:bar@^1"`), this is `foo`.
    pub name: String,
    /// The package's underlying identity, independent of the alias.
    /// For npm-aliases this is the target package name (`bar`); for
    /// non-aliased deps it equals `name`.
    pub ident_name: String,
    /// Version-with-peer ref. For the root and workspace nodes this
    /// is `""` or `"workspace:<id>"`; for regular nodes it's the
    /// snapshot key (`name@version(peer)`).
    pub reference: String,
    /// Aliases that this node refuses to hoist past — its parent
    /// must keep them in scope. The union of `peerDependencies` and
    /// `transitivePeerDependencies` from the lockfile, unless
    /// `autoInstallPeers` is set (which zeroes the set so the
    /// hoister moves freely).
    pub peer_names: BTreeSet<String>,
    pub dependency_kind: HoisterDependencyKind,
    /// Tiebreaker for the hoister's BFS. Pacquet always builds with
    /// `0`; pnpm and yarn-berry use higher values for packages that
    /// declare a high `hoistPriority` to bias them toward the root.
    pub hoist_priority: u32,
    /// Children of this node. Order matches insertion order — the
    /// hoister depends on it.
    pub dependencies: RefCell<IndexSet<RcByPtr<HoisterTree>>>,
}

/// Output node from the hoister. The shape mirrors `HoisterTree`
/// except that one `HoisterResult` can collect multiple references
/// (when several `HoisterTree` nodes with the same `ident_name`
/// converged onto the same hoist slot).
///
/// Both `references` and `dependencies` use [`RefCell`] for the same
/// reason [`HoisterTree::dependencies`] does: nodes are shared by
/// `Rc` identity across the result graph, and the algorithm
/// accumulates references / reorders children in place rather than
/// rebuilding `Rc`s (which would break the shared-by-identity
/// invariant for any earlier clone).
///
/// Mirrors `HoisterResult` at the [yarn source][yarn-result].
///
/// [yarn-result]: https://github.com/yarnpkg/berry/blob/4287909fa6a0a1ec976a55776bff606864b31990/packages/yarnpkg-nm/sources/hoist.ts#L20-L23
#[derive(Debug, Clone)]
pub struct HoisterResult {
    pub name: String,
    pub ident_name: String,
    pub references: RefCell<BTreeSet<String>>,
    pub dependencies: RefCell<IndexSet<RcByPtr<HoisterResult>>>,
}

/// Per-importer hoisting borders. Outer key is the importer locator
/// (e.g. `.@`); the inner set lists package aliases that may not be
/// hoisted past that importer.
///
/// Upstream `HoistingLimits` is `Map<string, Set<string>>`. Pacquet
/// uses `BTreeMap` / `BTreeSet` so the order is deterministic for
/// snapshot tests.
pub type HoistingLimits = BTreeMap<String, BTreeSet<String>>;

/// Options accepted by [`hoist`]. Mirrors the `opts` object of the
/// pnpm wrapper.
#[derive(Debug, Default, Clone)]
pub struct HoistOpts {
    pub hoisting_limits: HoistingLimits,
    pub external_dependencies: BTreeSet<String>,
    /// When `true`, every package's `peer_names` is zeroed before
    /// the hoister runs. Mirrors pnpm's `autoInstallPeers` short-
    /// circuit at [real-hoist:124][auto].
    ///
    /// [auto]: https://github.com/pnpm/pnpm/blob/94240bc0464196bd52f7006b97f6d9a43df34633/installing/linking/real-hoist/src/index.ts#L124-L129
    pub auto_install_peers: bool,
}

/// Failure modes of [`hoist`].
///
/// Marked `#[non_exhaustive]` so adding variants in later work
/// isn't a breaking API change.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum HoistError {
    /// A snapshot referenced by an importer is missing from
    /// `lockfile.snapshots`. Mirrors pnpm's
    /// `LockfileMissingDependencyError` raised at
    /// [real-hoist:111][missing-dep].
    ///
    /// [missing-dep]: https://github.com/pnpm/pnpm/blob/94240bc0464196bd52f7006b97f6d9a43df34633/installing/linking/real-hoist/src/index.ts#L109-L111
    #[display("Broken lockfile: missing snapshot for {pkg_key}")]
    #[diagnostic(
        code(ERR_PNPM_LOCKFILE_MISSING_DEPENDENCY),
        url("https://pnpm.io/errors#err_pnpm_lockfile_missing_dependency")
    )]
    LockfileMissingDependency {
        /// The depPath (snapshot key) the lockfile failed to
        /// resolve.
        pkg_key: String,
    },
    /// A package in the lockfile declares peer dependencies (or
    /// transitive peer dependencies). The hoist algorithm doesn't
    /// model peer-dependency constraints yet, so it would freely
    /// hoist this node past parents that supply the peer — a
    /// silent shadowing pnpm would reject. Refuse upfront rather
    /// than emit a wrong layout.
    #[display("hoister cannot yet model peer dependencies (package {ident}, peers: {peers:?})")]
    #[diagnostic(
        code(ERR_PACQUET_HOIST_UNSUPPORTED_PEER),
        help(
            "the hoister doesn't yet model peer-dependency constraints; \
             pass a lockfile whose packages declare no `peerDependencies` \
             (and whose snapshots carry no `transitivePeerDependencies`)."
        )
    )]
    UnsupportedPeerDependency {
        /// The snapshot key of the offending package.
        ident: String,
        /// The peer / transitive-peer names that block the hoist.
        peers: BTreeSet<String>,
    },
    /// The caller passed a non-empty `hoisting_limits` map. The
    /// hoist algorithm doesn't enforce limits yet, so flattening
    /// would violate the borders the caller asked to keep. Refuse
    /// upfront.
    #[display(
        "hoister cannot yet enforce `hoisting_limits`; the supplied map is non-empty ({len} entries)"
    )]
    #[diagnostic(
        code(ERR_PACQUET_HOIST_UNSUPPORTED_HOISTING_LIMITS),
        help("the hoister doesn't yet enforce `hoistingLimits`; pass an empty map.")
    )]
    UnsupportedHoistingLimits {
        /// How many entries the caller supplied. Carries no
        /// semantic value beyond debug; if zero we wouldn't have
        /// fired.
        len: usize,
    },
    /// The lockfile contains more than one importer. The hoist
    /// algorithm doesn't yet support workspace projects (multi-
    /// importer hoist trees with `workspace:` references). Refuse
    /// upfront rather than build a single-importer layout that
    /// pnpm would reject.
    #[display("hoister cannot yet model workspace lockfiles; extra importers: {extra_importers:?}")]
    #[diagnostic(
        code(ERR_PACQUET_HOIST_UNSUPPORTED_WORKSPACE),
        help(
            "the hoister doesn't yet model multi-importer (workspace) lockfiles; \
             pass a lockfile that carries only the `.` importer. \
             pacquet's wider install path supports workspaces — see \
             `SymlinkDirectDependencies` for the isolated-linker case."
        )
    )]
    UnsupportedWorkspace {
        /// The importer IDs the lockfile carries beyond the root
        /// `.` importer.
        extra_importers: Vec<String>,
    },
}

/// Identity-hashed wrapper around `Rc<T>`. Two `RcByPtr` values are
/// equal iff their underlying `Rc`s point at the same allocation;
/// hashing uses the pointer address, not `T`'s `Hash` impl.
///
/// This mirrors JS `Set<HoisterTree>` semantics — JS Sets hash by
/// object identity, so adding the same node via two parent paths
/// keeps one entry. Cloning a `RcByPtr` only bumps the refcount, so
/// the dedup property survives parent-to-child propagation.
///
/// Without this wrapper, [`IndexSet<Rc<HoisterTree>>`] would hash on
/// the tree contents — recursive and expensive for deep graphs,
/// and wrong when two structurally-identical nodes come from
/// different sources and should stay distinct.
#[derive(Debug)]
pub struct RcByPtr<T>(pub Rc<T>);

impl<T> Clone for RcByPtr<T> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<T> PartialEq for RcByPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for RcByPtr<T> {}

impl<T> std::hash::Hash for RcByPtr<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Rc::as_ptr(&self.0) as usize).hash(state);
    }
}

impl<T> std::ops::Deref for RcByPtr<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> From<Rc<T>> for RcByPtr<T> {
    fn from(rc: Rc<T>) -> Self {
        Self(rc)
    }
}

/// Build the [`HoisterTree`] for `lockfile`'s root importer (plus
/// any non-root importers as workspace children) and run the
/// `@yarnpkg/nm` hoister over it. Ports
/// [`installing/linking/real-hoist/src/index.ts`][upstream].
///
/// The inner hoist algorithm is currently stubbed: it returns the
/// input tree shape converted to `HoisterResult` without moving
/// anything. The lockfile-to-tree translation and the
/// `LockfileMissingDependencyError` surface are what this function
/// pins today.
///
/// [upstream]: https://github.com/pnpm/pnpm/blob/94240bc0464196bd52f7006b97f6d9a43df34633/installing/linking/real-hoist/src/index.ts
pub fn hoist(lockfile: &Lockfile, opts: &HoistOpts) -> Result<HoisterResult, HoistError> {
    // Refuse upfront for inputs the algorithm doesn't yet model.
    // Better an explicit error than a silently-invented layout
    // pnpm would reject.
    if !opts.hoisting_limits.is_empty() {
        return Err(HoistError::UnsupportedHoistingLimits { len: opts.hoisting_limits.len() });
    }
    let extra_importers: Vec<String> = lockfile
        .importers
        .keys()
        .filter(|k| k.as_str() != Lockfile::ROOT_IMPORTER_KEY)
        .cloned()
        .collect();
    if !extra_importers.is_empty() {
        return Err(HoistError::UnsupportedWorkspace { extra_importers });
    }

    let mut nodes: HashMap<String, Rc<HoisterTree>> = HashMap::new();

    let mut root_children: IndexSet<RcByPtr<HoisterTree>> = IndexSet::new();

    if let Some(root) = lockfile.importers.get(Lockfile::ROOT_IMPORTER_KEY) {
        collect_importer_deps(root, lockfile, opts, &mut nodes, &mut root_children)?;
    }

    // `externalDependencies` are added as `link:` placeholders at
    // the root so the hoister won't move anything else into those
    // slots; they're stripped from the result after hoisting.
    // Pacquet has no consumer for this yet, but the wrapper handles
    // it for parity with upstream's signature.
    for dep in &opts.external_dependencies {
        let placeholder = Rc::new(HoisterTree {
            name: dep.clone(),
            ident_name: dep.clone(),
            reference: "link:".to_string(),
            peer_names: BTreeSet::new(),
            dependency_kind: HoisterDependencyKind::ExternalSoftLink,
            hoist_priority: 0,
            dependencies: RefCell::new(IndexSet::new()),
        });
        root_children.insert(RcByPtr(placeholder));
    }

    // Non-root importers (workspace projects) become children of
    // the virtual `.` root. Pacquet's install pipeline doesn't yet
    // support workspaces, so the wrapper accepts them but the rest
    // of the install code path will reject a multi-importer
    // lockfile elsewhere.
    let mut non_root: Vec<(&String, &ProjectSnapshot)> = lockfile
        .importers
        .iter()
        .filter(|(id, _)| id.as_str() != Lockfile::ROOT_IMPORTER_KEY)
        .collect();
    // HashMap iteration order is non-deterministic; sort so the
    // output tree is stable across runs (matters for snapshot
    // tests).
    non_root.sort_by(|a, b| a.0.cmp(b.0));

    for (importer_id, importer) in non_root {
        let mut importer_children: IndexSet<RcByPtr<HoisterTree>> = IndexSet::new();
        collect_importer_deps(importer, lockfile, opts, &mut nodes, &mut importer_children)?;
        let importer_node = Rc::new(HoisterTree {
            name: percent_encode_path(importer_id),
            ident_name: percent_encode_path(importer_id),
            reference: format!("workspace:{importer_id}"),
            peer_names: BTreeSet::new(),
            dependency_kind: HoisterDependencyKind::Workspace,
            hoist_priority: 0,
            dependencies: RefCell::new(importer_children),
        });
        root_children.insert(RcByPtr(importer_node));
    }

    let root_node = Rc::new(HoisterTree {
        name: ".".to_string(),
        ident_name: ".".to_string(),
        reference: String::new(),
        peer_names: BTreeSet::new(),
        dependency_kind: HoisterDependencyKind::Workspace,
        hoist_priority: 0,
        dependencies: RefCell::new(root_children),
    });

    // Scan the constructed tree for peer-constrained packages.
    // `peer_names` only gets populated when the lockfile's
    // `packages` map declares `peerDependencies` (or
    // `transitive_peer_dependencies` on a snapshot), so a peer-
    // free lockfile passes the guard unchanged.
    if let Some(err) = find_first_peer_constrained(&root_node) {
        return Err(err);
    }

    let result = nm_hoist(&root_node, opts);

    // Strip `externalDependencies` from the top-level result —
    // they exist only to reserve a name slot at the root.
    if !opts.external_dependencies.is_empty() {
        result
            .dependencies
            .borrow_mut()
            .retain(|dep| !opts.external_dependencies.contains(&dep.name));
    }

    Ok(result)
}

fn collect_importer_deps(
    importer: &ProjectSnapshot,
    lockfile: &Lockfile,
    opts: &HoistOpts,
    nodes: &mut HashMap<String, Rc<HoisterTree>>,
    out: &mut IndexSet<RcByPtr<HoisterTree>>,
) -> Result<(), HoistError> {
    // Upstream merges `dependencies + devDependencies +
    // optionalDependencies` into one alias-keyed object. Later
    // entries (in declaration order) win on duplicate aliases —
    // which is the same as inserting in that order and keeping the
    // last write. Pacquet's `ResolvedDependencyMap` is a HashMap so
    // declaration order is lost; merge into a `HashMap` (last write
    // wins) and emit in alias-sorted order so the build is
    // deterministic regardless of map seed.
    let mut merged: HashMap<&PkgName, &pacquet_lockfile::ResolvedDependencySpec> = HashMap::new();
    for deps in
        [&importer.dependencies, &importer.dev_dependencies, &importer.optional_dependencies]
            .into_iter()
            .flatten()
    {
        for (alias, spec) in deps {
            merged.insert(alias, spec);
        }
    }
    let mut entries: Vec<_> = merged.into_iter().collect();
    entries.sort_by_key(|(alias, _)| alias.to_string());
    for (alias, spec) in entries {
        // Pacquet's `ResolvedDependencySpec.version` doesn't carry an
        // alternate package name, so importer-level npm-aliases
        // aren't modelled here today — assume the alias is the
        // registry name. Transitive npm-aliases (modelled via
        // `SnapshotDepRef::Alias`) are handled in
        // `collect_snapshot_deps`.
        //
        // `link:` deps (cross-importer `workspace:*` resolutions, see
        // [`ImporterDepVersion::Link`]) don't live in the virtual
        // store — they're directory symlinks materialised by
        // [`pacquet_package_manager::SymlinkDirectDependencies`] —
        // so they have no snapshot to hoist and we skip them here.
        let Some(ver_peer) = spec.version.as_regular() else {
            continue;
        };
        let dep_key = PkgNameVerPeer::new(alias.clone(), ver_peer.clone());
        let node = build_dep_node(alias, &dep_key, lockfile, opts, nodes)?;
        out.insert(RcByPtr(node));
    }
    Ok(())
}

fn build_dep_node(
    alias: &PkgName,
    dep_key: &PkgNameVerPeer,
    lockfile: &Lockfile,
    opts: &HoistOpts,
    nodes: &mut HashMap<String, Rc<HoisterTree>>,
) -> Result<Rc<HoisterTree>, HoistError> {
    // Cache key is `<alias>:<dep_key>` to match upstream — two
    // different aliases pointing at the same package are
    // intentionally different nodes (the node's `name` field
    // differs), so they shouldn't share a cache slot.
    let cache_key = format!("{alias}:{dep_key}");
    if let Some(existing) = nodes.get(&cache_key) {
        return Ok(Rc::clone(existing));
    }

    let snapshots = lockfile
        .snapshots
        .as_ref()
        .ok_or_else(|| HoistError::LockfileMissingDependency { pkg_key: dep_key.to_string() })?;
    let snapshot = snapshots
        .get(dep_key)
        .ok_or_else(|| HoistError::LockfileMissingDependency { pkg_key: dep_key.to_string() })?;

    // Peer-name set: peerDependencies (from the `packages:` map)
    // plus transitivePeerDependencies (from the `snapshots:` map).
    // Mirrors upstream's
    // `[...Object.keys(pkgSnapshot.peerDependencies), ...transitivePeerDependencies]`.
    // Zeroed when `auto_install_peers` is on, so the hoister moves
    // freely.
    let mut peer_names: BTreeSet<String> = BTreeSet::new();
    if !opts.auto_install_peers {
        if let Some(packages) = lockfile.packages.as_ref() {
            let packages_key = dep_key.without_peer();
            if let Some(meta) = packages.get(&packages_key)
                && let Some(peer_deps) = meta.peer_dependencies.as_ref()
            {
                for name in peer_deps.keys() {
                    peer_names.insert(name.clone());
                }
            }
        }
        if let Some(transitive) = snapshot.transitive_peer_dependencies.as_ref() {
            for name in transitive {
                peer_names.insert(name.clone());
            }
        }
    }

    // Construct the node with an empty `dependencies` cell, stash
    // it in the cache, then recurse and populate the cell in place.
    // A back-edge that hits the same `cache_key` during the
    // recursion gets the same `Rc<HoisterTree>` — by the time the
    // outer call returns the cell holds the populated set, and the
    // shared-by-identity invariant the hoister algorithm relies on
    // survives. Mirrors the in-place mutation of `node.dependencies`
    // at upstream's real-hoist:132.
    let node = Rc::new(HoisterTree {
        name: alias.to_string(),
        ident_name: dep_key.name.to_string(),
        reference: dep_key.to_string(),
        peer_names,
        dependency_kind: HoisterDependencyKind::Regular,
        hoist_priority: 0,
        dependencies: RefCell::new(IndexSet::new()),
    });
    nodes.insert(cache_key, Rc::clone(&node));

    let mut children: IndexSet<RcByPtr<HoisterTree>> = IndexSet::new();
    collect_snapshot_deps(snapshot, lockfile, opts, nodes, &mut children)?;
    *node.dependencies.borrow_mut() = children;
    Ok(node)
}

fn collect_snapshot_deps(
    snapshot: &SnapshotEntry,
    lockfile: &Lockfile,
    opts: &HoistOpts,
    nodes: &mut HashMap<String, Rc<HoisterTree>>,
    out: &mut IndexSet<RcByPtr<HoisterTree>>,
) -> Result<(), HoistError> {
    let mut merged: HashMap<&PkgName, &pacquet_lockfile::SnapshotDepRef> = HashMap::new();
    for deps in [&snapshot.dependencies, &snapshot.optional_dependencies].into_iter().flatten() {
        for (alias, dep_ref) in deps {
            merged.insert(alias, dep_ref);
        }
    }
    let mut entries: Vec<_> = merged.into_iter().collect();
    entries.sort_by_key(|(alias, _)| alias.to_string());
    for (alias, dep_ref) in entries {
        // `dep_ref.resolve(alias)` returns the *snapshot lookup
        // key*: `<alias>@<ver>` for `Plain`, `<target>@<ver>` for
        // an npm-alias `Alias`. Pass that as `dep_key` so the
        // snapshot lookup hits the right entry. The node's exposed
        // `name` stays `alias`; only the lookup uses the resolved
        // target name.
        let dep_key = dep_ref.resolve(alias);
        let node = build_dep_node(alias, &dep_key, lockfile, opts, nodes)?;
        out.insert(RcByPtr(node));
    }
    Ok(())
}

/// Encode an importer id for use as a child node's `name`. Upstream
/// uses `encodeURIComponent`, which percent-encodes everything
/// except `A-Z a-z 0-9 - _ . ! ~ * ' ( )`. Pacquet workspace
/// importers are filesystem-relative paths, so the common case is
/// alphanumeric + `/` + `-` + `_`. Encode `/` (since it would
/// confuse `node_modules` directory parsing) and pass the rest
/// through; if a richer set ever shows up the function can switch
/// to a full encoder without touching call sites.
fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'A'..='Z'
            | 'a'..='z'
            | '0'..='9'
            | '-'
            | '_'
            | '.'
            | '!'
            | '~'
            | '*'
            | '\''
            | '('
            | ')' => out.push(ch),
            '/' => out.push_str("%2F"),
            other => {
                // Best-effort %xx encode for the ASCII subset we
                // expect in importer ids. Anything else is left
                // verbatim — pacquet's lockfile doesn't currently
                // hand the wrapper non-ASCII paths.
                if (other as u32) < 0x80 {
                    out.push_str(&format!("%{:02X}", other as u32));
                } else {
                    out.push(other);
                }
            }
        }
    }
    out
}

/// Walk the constructed tree and return the first node whose
/// `peer_names` is non-empty as an `UnsupportedPeerDependency`
/// error. Returns `None` when the tree is peer-free.
fn find_first_peer_constrained(root: &Rc<HoisterTree>) -> Option<HoistError> {
    let mut visited: HashSet<*const HoisterTree> = HashSet::new();
    let mut stack: Vec<Rc<HoisterTree>> = vec![Rc::clone(root)];
    while let Some(node) = stack.pop() {
        if !visited.insert(Rc::as_ptr(&node)) {
            continue;
        }
        if !node.peer_names.is_empty() {
            return Some(HoistError::UnsupportedPeerDependency {
                ident: node.reference.clone(),
                peers: node.peer_names.clone(),
            });
        }
        for dep in node.dependencies.borrow().iter() {
            stack.push(Rc::clone(&dep.0));
        }
    }
    None
}

/// Pacquet's port of the `@yarnpkg/nm` hoist algorithm. Walks the
/// input tree, deep-copies it into a `HoisterResult` shape, then
/// pulls eligible descendants up to the root via single-pass BFS
/// with parent-wins conflict resolution. Models the common case
/// of pnpm's `nodeLinker: hoisted` install — every transitive
/// dependency that doesn't collide with an already-hoisted name
/// surfaces at the root, just like a flat `node_modules`.
///
/// What this models today:
///
/// * Free hoist: a transitive dep with no name collision at the
///   root surfaces at the root.
/// * Identity dedup: a dep reachable through multiple parents
///   (same `Rc` thanks to the wrapper's `nodes` cache) collapses
///   to one node at root.
/// * Parent-wins on version conflict: when two distinct deps
///   share an alias but resolve to different snapshot keys, the
///   first one BFS reaches takes the root slot and the other
///   stays under its parent.
///
/// What this does *not* model yet (each gated on later work
/// because they require additional graph structure and tests):
///
/// * Peer-dependency constraints (`peer_names`) — packages that
///   refuse to hoist past parents declaring them as peers. The
///   wrapper refuses lockfiles that contain any peer-constrained
///   nodes via `HoistError::UnsupportedPeerDependency`, so this
///   function only ever sees peer-free input today.
/// * Multi-round convergence — re-walking the tree to discover
///   newly-hoistable deps after the first pass. The BFS does
///   handle deep chains (`root → a → b → c` flattens in one
///   pass), so the cases requiring true multi-round are limited
///   to peer interactions.
/// * `hoistingLimits` / `externalDependencies` knobs.
/// * `dependencyKind` distinctions for workspaces and external
///   soft links.
///
/// Matches the structural intent of upstream `hoistTo` at
/// [hoist.ts:329][upstream] for the subset above.
///
/// [upstream]: https://github.com/yarnpkg/berry/blob/4287909fa6a0a1ec976a55776bff606864b31990/packages/yarnpkg-nm/sources/hoist.ts#L329
fn nm_hoist(tree: &HoisterTree, _opts: &HoistOpts) -> HoisterResult {
    let mut memo: HashMap<*const HoisterTree, Rc<HoisterResult>> = HashMap::new();
    let root = convert(tree, &mut memo);
    hoist_into_root(&root);
    // Returning an owned `HoisterResult` (rather than
    // `Rc<HoisterResult>`) keeps the wrapper's post-hoist
    // `external_dependencies` filter from mutating the shared graph.
    // Cloning the outer struct duplicates only the top-level fields —
    // the subtree children remain shared via the cloned `RcByPtr`
    // values, so deep deps stay deduplicated.
    (*root).clone()
}

/// Outcome of the per-child hoist decision at the root.
enum AbsorbDecision {
    /// Root's name slot is free; the child should be moved up to
    /// the root.
    Free,
    /// Root already holds *this exact `Rc`* (the same node was
    /// reachable through another parent path and got hoisted
    /// earlier). The duplicate reference in the current parent
    /// just needs to be removed.
    SameNode,
    /// Root's name slot is taken by a different `Rc` — a version
    /// conflict. The child stays under its current parent.
    Conflict,
}

/// Walk the result tree breadth-first and hoist every eligible
/// descendant of `root` onto `root` itself. Single-pass: each node
/// is visited once, and a descendant's children become hoist
/// candidates as soon as the descendant itself is queued.
///
/// Maintains a side `HashMap<name, RcByPtr>` mirror of root's
/// direct deps so the per-edge "is this name taken at root?" check
/// stays O(1). Without the index a graph with `N` packages all
/// hoisting freely would do O(N²) `IndexSet` scans.
fn hoist_into_root(root: &Rc<HoisterResult>) {
    let root_ptr = Rc::as_ptr(root);
    let mut visited: HashSet<*const HoisterResult> = HashSet::new();
    let mut queue: VecDeque<Rc<HoisterResult>> = VecDeque::new();
    queue.push_back(Rc::clone(root));

    let mut root_index: HashMap<String, RcByPtr<HoisterResult>> =
        root.dependencies.borrow().iter().map(|d| (d.0.name.clone(), d.clone())).collect();

    while let Some(node) = queue.pop_front() {
        let node_ptr = Rc::as_ptr(&node);
        if !visited.insert(node_ptr) {
            continue;
        }

        // Snapshot the current children so we can mutate
        // `node.dependencies` mid-iteration without invalidating the
        // borrow. `RcByPtr::clone` just bumps refcounts.
        let children: Vec<RcByPtr<HoisterResult>> =
            node.dependencies.borrow().iter().cloned().collect();

        let is_root_parent = Rc::ptr_eq(&node, root);

        for child in children {
            let child_ptr = Rc::as_ptr(&child.0);
            if child_ptr == root_ptr {
                // Back-edge to root via a cycle. Nothing to hoist.
                continue;
            }

            let decision = match root_index.get(&child.0.name) {
                None => AbsorbDecision::Free,
                Some(existing) if Rc::ptr_eq(&existing.0, &child.0) => AbsorbDecision::SameNode,
                Some(_) => AbsorbDecision::Conflict,
            };

            if !is_root_parent {
                match decision {
                    AbsorbDecision::Free => {
                        node.dependencies.borrow_mut().shift_remove(&child);
                        root.dependencies.borrow_mut().insert(child.clone());
                        root_index.insert(child.0.name.clone(), child.clone());
                    }
                    AbsorbDecision::SameNode => {
                        // The shared `Rc` is already at root; just
                        // strip the duplicate reference at this
                        // parent so the deeper copy disappears.
                        node.dependencies.borrow_mut().shift_remove(&child);
                    }
                    AbsorbDecision::Conflict => {
                        // Stays at the current parent. The version
                        // already at root wins the slot.
                    }
                }
            }

            // Queue the child so its own descendants get a chance.
            // This is what lets deep chains (`root → a → b → c`)
            // flatten in a single BFS pass: by the time `b` is
            // dequeued it's already a direct child of root, so
            // `c` is evaluated against root's slot, not against
            // `a`'s slot.
            queue.push_back(Rc::clone(&child.0));
        }
    }
}

fn convert(
    tree: &HoisterTree,
    memo: &mut HashMap<*const HoisterTree, Rc<HoisterResult>>,
) -> Rc<HoisterResult> {
    let ptr = tree as *const HoisterTree;
    if let Some(existing) = memo.get(&ptr) {
        return Rc::clone(existing);
    }
    // Stash a node with empty `dependencies`, then recurse and
    // populate the cell in place. Anyone reached via a back-edge
    // gets `Rc::clone` of the same allocation and reads the
    // (eventually-populated) cell — matches the in-place mutation
    // semantics the real hoist algorithm needs.
    let mut refs = BTreeSet::new();
    refs.insert(tree.reference.clone());
    let node = Rc::new(HoisterResult {
        name: tree.name.clone(),
        ident_name: tree.ident_name.clone(),
        references: RefCell::new(refs),
        dependencies: RefCell::new(IndexSet::new()),
    });
    memo.insert(ptr, Rc::clone(&node));

    // Collect the children before recursing so we can drop the
    // `Ref<'_, IndexSet<...>>` borrow on `tree.dependencies`. The
    // recursion only reads (not mutates) `HoisterTree` cells, so
    // holding the borrow across recursive calls is technically
    // safe, but releasing it keeps the panic surface smaller if
    // the algorithm later grows a mutation pass over the input.
    let to_convert: Vec<RcByPtr<HoisterTree>> =
        tree.dependencies.borrow().iter().cloned().collect();
    let mut children: IndexSet<RcByPtr<HoisterResult>> = IndexSet::new();
    for child in to_convert {
        children.insert(RcByPtr(convert(&child.0, memo)));
    }
    *node.dependencies.borrow_mut() = children;
    node
}

#[cfg(test)]
mod tests;
