//! Real-directory hoister for the `nodeLinker: hoisted` install layout.
//!
//! Ports pnpm v11's [`installing/linking/real-hoist`][upstream-wrapper]
//! package, which is itself a thin wrapper around the
//! [`@yarnpkg/nm/hoist`][yarn-hoist] algorithm. The wrapper translates a
//! pnpm lockfile into a [`HoisterTree`] (rooted at `.` with one child
//! per workspace importer), runs the algorithm, and post-filters
//! `externalDependencies` out of the top-level result.
//!
//! This is Sub-slice 3a of the hoisted-linker umbrella ([#438]). It
//! ships the IO types, the lockfile-to-`HoisterTree` translation, and a
//! stub `nm_hoist` that returns the input tree unchanged. Later
//! sub-slices replace the stub with the real algorithm and add the
//! `hoistingLimits` / `externalDependencies` knobs.
//!
//! [upstream-wrapper]: https://github.com/pnpm/pnpm/blob/94240bc0464196bd52f7006b97f6d9a43df34633/installing/linking/real-hoist/src/index.ts
//! [yarn-hoist]: https://github.com/yarnpkg/berry/blob/4287909fa6a0a1ec976a55776bff606864b31990/packages/yarnpkg-nm/sources/hoist.ts
//! [#438]: https://github.com/pnpm/pacquet/issues/438

use derive_more::{Display, Error};
use indexmap::IndexSet;
use miette::Diagnostic;
use pacquet_lockfile::{Lockfile, PkgName, PkgNameVerPeer, ProjectSnapshot, SnapshotEntry};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
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
    pub dependencies: IndexSet<RcByPtr<HoisterTree>>,
}

/// Output node from the hoister. The shape mirrors `HoisterTree`
/// except that one `HoisterResult` can collect multiple references
/// (when several `HoisterTree` nodes with the same `ident_name`
/// converged onto the same hoist slot).
///
/// Mirrors `HoisterResult` at the [yarn source][yarn-result].
///
/// [yarn-result]: https://github.com/yarnpkg/berry/blob/4287909fa6a0a1ec976a55776bff606864b31990/packages/yarnpkg-nm/sources/hoist.ts#L20-L23
#[derive(Debug, Clone)]
pub struct HoisterResult {
    pub name: String,
    pub ident_name: String,
    pub references: BTreeSet<String>,
    pub dependencies: IndexSet<RcByPtr<HoisterResult>>,
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
#[derive(Debug, Display, Error, Diagnostic)]
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
/// The inner hoist algorithm is currently stubbed (returns the
/// input tree shape unchanged). The lockfile-to-tree translation
/// and the `LockfileMissingDependencyError` surface are the parts
/// pinned by Sub-slice 3a; later sub-slices replace the stub.
///
/// [upstream]: https://github.com/pnpm/pnpm/blob/94240bc0464196bd52f7006b97f6d9a43df34633/installing/linking/real-hoist/src/index.ts
pub fn hoist(lockfile: &Lockfile, opts: &HoistOpts) -> Result<HoisterResult, HoistError> {
    let mut nodes: HashMap<String, Rc<HoisterTree>> = HashMap::new();

    let mut root_children: IndexSet<RcByPtr<HoisterTree>> = IndexSet::new();

    if let Some(root) = lockfile.importers.get(Lockfile::ROOT_IMPORTER_KEY) {
        collect_importer_deps(root, lockfile, opts, &mut nodes, &mut root_children)?;
    }

    // `externalDependencies` are added as `link:` placeholders at
    // the root so the hoister won't move anything else into those
    // slots; they're stripped from the result after hoisting.
    // Pacquet has no consumer for this yet (the umbrella's Slice 10
    // wires it through configuration), but the wrapper handles it
    // for parity with upstream's signature.
    for dep in &opts.external_dependencies {
        let placeholder = Rc::new(HoisterTree {
            name: dep.clone(),
            ident_name: dep.clone(),
            reference: "link:".to_string(),
            peer_names: BTreeSet::new(),
            dependency_kind: HoisterDependencyKind::ExternalSoftLink,
            hoist_priority: 0,
            dependencies: IndexSet::new(),
        });
        root_children.insert(RcByPtr(placeholder));
    }

    // Non-root importers (workspace projects) become children of
    // the virtual `.` root. Pacquet's install pipeline doesn't yet
    // support workspaces (umbrella Slice 9), so the wrapper accepts
    // them but the rest of the install code path will reject a
    // multi-importer lockfile elsewhere.
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
            dependencies: importer_children,
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
        dependencies: root_children,
    });

    let mut result = nm_hoist(&root_node, opts);

    // Strip `externalDependencies` from the top-level result —
    // they exist only to reserve a name slot at the root.
    if !opts.external_dependencies.is_empty() {
        result.dependencies.retain(|dep| !opts.external_dependencies.contains(&dep.name));
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
        let node = build_dep_node(alias, &spec.version, lockfile, opts, nodes)?;
        out.insert(RcByPtr(node));
    }
    Ok(())
}

fn build_dep_node(
    alias: &PkgName,
    version: &pacquet_lockfile::PkgVerPeer,
    lockfile: &Lockfile,
    opts: &HoistOpts,
    nodes: &mut HashMap<String, Rc<HoisterTree>>,
) -> Result<Rc<HoisterTree>, HoistError> {
    let dep_key = PkgNameVerPeer::new(alias.clone(), version.clone());
    let cache_key = format!("{alias}:{dep_key}");
    if let Some(existing) = nodes.get(&cache_key) {
        return Ok(Rc::clone(existing));
    }

    let snapshots = lockfile
        .snapshots
        .as_ref()
        .ok_or_else(|| HoistError::LockfileMissingDependency { pkg_key: dep_key.to_string() })?;
    let snapshot = snapshots
        .get(&dep_key)
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

    let mut children: IndexSet<RcByPtr<HoisterTree>> = IndexSet::new();
    // Insert a placeholder first so cyclic descents short-circuit
    // back to it (the JS code achieves the same by setting `node`
    // in the map before recursing — see real-hoist:131).
    let node = Rc::new(HoisterTree {
        name: alias.to_string(),
        ident_name: pkg_name_from_key(&dep_key),
        reference: dep_key.to_string(),
        peer_names,
        dependency_kind: HoisterDependencyKind::Regular,
        hoist_priority: 0,
        dependencies: IndexSet::new(),
    });
    nodes.insert(cache_key.clone(), Rc::clone(&node));

    collect_snapshot_deps(snapshot, lockfile, opts, nodes, &mut children)?;

    // The placeholder we stashed in `nodes` is shared with anyone
    // who recursed back into us. Swap its empty `dependencies` for
    // the populated set by rebuilding the `Rc` — safe because we
    // never handed out a `&HoisterTree` reference that's still
    // live, only `Rc<HoisterTree>` clones that point at the same
    // allocation. Existing clones see the rebuilt set because we
    // overwrite the map entry too.
    //
    // (A cleaner pattern is `Rc<RefCell<HoisterTree>>` but `RefCell`
    // pollutes every read site downstream. We re-Rc once at
    // construction here.)
    let finished = Rc::new(HoisterTree {
        name: node.name.clone(),
        ident_name: node.ident_name.clone(),
        reference: node.reference.clone(),
        peer_names: node.peer_names.clone(),
        dependency_kind: node.dependency_kind,
        hoist_priority: node.hoist_priority,
        dependencies: children,
    });
    nodes.insert(cache_key, Rc::clone(&finished));
    Ok(finished)
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
        let resolved = dep_ref.resolve(alias);
        let node = build_dep_node(alias, &resolved.suffix, lockfile, opts, nodes)?;
        out.insert(RcByPtr(node));
    }
    Ok(())
}

/// Extract `name@version` from a snapshot key. The upstream
/// `nameVerFromPkgSnapshot` returns just the name portion here; we
/// rebuild it from the parsed key.
fn pkg_name_from_key(key: &PkgNameVerPeer) -> String {
    key.name.to_string()
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

/// Stub for the `@yarnpkg/nm` hoist algorithm. Walks the input tree
/// and returns it in `HoisterResult` shape unchanged — no actual
/// hoisting happens. Sub-slice 3b replaces this with the real
/// algorithm.
fn nm_hoist(tree: &HoisterTree, _opts: &HoistOpts) -> HoisterResult {
    let mut memo: HashMap<*const HoisterTree, Rc<HoisterResult>> = HashMap::new();
    convert(tree, &mut memo).as_ref().clone()
}

fn convert(
    tree: &HoisterTree,
    memo: &mut HashMap<*const HoisterTree, Rc<HoisterResult>>,
) -> Rc<HoisterResult> {
    let ptr = tree as *const HoisterTree;
    if let Some(existing) = memo.get(&ptr) {
        return Rc::clone(existing);
    }
    // Stash a placeholder before recursing so cycles in the input
    // graph short-circuit. The placeholder has empty children; the
    // real conversion overwrites the memo entry below. Anyone who
    // received the placeholder via a cycle reads its (then-empty)
    // children — matches JS Set-identity dedup semantics where a
    // node visited via a cycle yields the same object.
    let placeholder = Rc::new(HoisterResult {
        name: tree.name.clone(),
        ident_name: tree.ident_name.clone(),
        references: {
            let mut s = BTreeSet::new();
            s.insert(tree.reference.clone());
            s
        },
        dependencies: IndexSet::new(),
    });
    memo.insert(ptr, Rc::clone(&placeholder));

    let mut children: IndexSet<RcByPtr<HoisterResult>> = IndexSet::new();
    for child in &tree.dependencies {
        children.insert(RcByPtr(convert(&child.0, memo)));
    }

    let finished = Rc::new(HoisterResult {
        name: tree.name.clone(),
        ident_name: tree.ident_name.clone(),
        references: {
            let mut s = BTreeSet::new();
            s.insert(tree.reference.clone());
            s
        },
        dependencies: children,
    });
    memo.insert(ptr, Rc::clone(&finished));
    finished
}

#[cfg(test)]
mod tests;
