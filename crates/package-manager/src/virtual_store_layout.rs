//! Per-install computed layout of the virtual store.
//!
//! Stage 1 of pnpm/pacquet#432 introduces a path split: when the global
//! virtual store is enabled, packages live at
//! `<store_dir>/links/<scope>/<name>/<version>/<hash>/node_modules/<name>`,
//! not at the project-local
//! `<project>/node_modules/.pnpm/<flat-name>/node_modules/<name>`. The
//! shape of `<flat-name>` versus `<scope>/<name>/<version>/<hash>` is
//! also different — flat name uses [`PkgNameVerPeer::to_virtual_store_name`]
//! while the GVS layout uses
//! [`pacquet_graph_hasher::format_global_virtual_store_path`] over a
//! `calc_graph_node_hash`-computed digest.
//!
//! [`VirtualStoreLayout`] hides that difference behind one
//! [`slot_dir`] lookup so the install pipeline doesn't have to branch
//! on `Config::enable_global_virtual_store` at every site that
//! computes a per-snapshot path.
//!
//! [`slot_dir`]: VirtualStoreLayout::slot_dir
//! [`PkgNameVerPeer::to_virtual_store_name`]: pacquet_lockfile::PkgNameVerPeer::to_virtual_store_name
//! [`pacquet_graph_hasher::format_global_virtual_store_path`]: pacquet_graph_hasher::format_global_virtual_store_path

use crate::AllowBuildPolicy;
use pacquet_config::Config;
use pacquet_graph_hasher::{
    DepsGraphNode, DepsStateCache, calc_graph_node_hash, format_global_virtual_store_path,
};
use pacquet_lockfile::{
    LockfileResolution, PackageKey, PackageMetadata, PkgIdWithPatchHash, PkgName, SnapshotDepRef,
    SnapshotEntry,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// Precomputed mapping from each snapshot key to the directory where
/// its files live on disk. Built once per install in
/// [`InstallFrozenLockfile::run`](crate::InstallFrozenLockfile::run);
/// passed by reference to every helper that needs to know where a
/// particular snapshot is materialised.
///
/// [`Self::slot_dir`] is the only call site every consumer has to
/// touch — it returns an absolute directory whose `node_modules/<name>`
/// subdirectory holds the unpacked package.
pub struct VirtualStoreLayout {
    /// Root containing every per-snapshot subdirectory. Picked from
    /// `Config::global_virtual_store_dir` when GVS is enabled (the
    /// shared `<store_dir>/links` path, or the user's pinned override)
    /// and from `Config::virtual_store_dir` when GVS is disabled (the
    /// project-local `<modules_dir>/.pnpm`). Pacquet keeps the two
    /// fields separate so the legacy non-frozen
    /// [`crate::InstallWithoutLockfile`] path can keep reading
    /// `virtual_store_dir` directly via [`Self::legacy`] without the
    /// frozen-lockfile derivation redirecting it. See
    /// [`Config::apply_global_virtual_store_derivation`] for the
    /// reasoning behind the field split.
    ///
    /// Stored separately from a `&Config` so callers don't have to
    /// thread the full config through the helpers that only need a
    /// path lookup.
    package_store_dir: PathBuf,

    /// `Some` only when the global virtual store is enabled. For each
    /// snapshot, holds the precomputed
    /// `[<scope>/]<name>/<version>/<hash>` suffix that goes after
    /// `package_store_dir`. `None` when GVS is off — callers fall back
    /// to [`PkgNameVerPeer::to_virtual_store_name`] computed on demand
    /// from the snapshot key.
    ///
    /// [`PkgNameVerPeer::to_virtual_store_name`]: pacquet_lockfile::PkgNameVerPeer::to_virtual_store_name
    gvs_suffixes: Option<HashMap<PackageKey, String>>,
}

impl VirtualStoreLayout {
    /// Construct a layout that always uses the legacy
    /// `<root>/<flat-name>` shape, regardless of any
    /// `enable_global_virtual_store` setting on `Config`. The
    /// non-frozen install path uses this — GVS is scoped to
    /// frozen-lockfile installs (pnpm/pacquet#432), so without-lockfile
    /// callers stay on the project-local flat layout even when
    /// `enable_global_virtual_store: true` is configured.
    pub fn legacy(root: impl Into<PathBuf>) -> Self {
        VirtualStoreLayout { package_store_dir: root.into(), gvs_suffixes: None }
    }

    /// Build the layout for one install. Reads
    /// [`Config::enable_global_virtual_store`] to decide whether to
    /// precompute GVS slot names, then iterates the lockfile's
    /// `snapshots` (the per-peer-context entries) and computes each
    /// snapshot's [`format_global_virtual_store_path`]-shaped suffix
    /// via [`calc_graph_node_hash`].
    ///
    /// Returns a layout that's safe to pass by reference across rayon
    /// workers: every field is `Send + Sync` once constructed (the
    /// internal `HashMap<PackageKey, String>` doesn't mutate after
    /// `new`).
    ///
    /// `engine` is the `ENGINE_NAME`-style string that
    /// [`pacquet_graph_hasher::engine_name`] produces; threaded in
    /// instead of recomputed inside so the value matches whatever the
    /// rest of the install (notably the side-effects cache key) uses.
    /// `None` propagates straight into
    /// [`calc_graph_node_hash`]'s `engine` parameter — `None` and
    /// `Some("")` produce *different* GVS hashes (the former omits
    /// the `engine` contribution, the latter hashes the empty string),
    /// so the call site must keep the `Option` shape rather than
    /// flattening to `unwrap_or("")`.
    ///
    /// `snapshots` / `packages` are the lockfile fields the caller
    /// already has by the time the install dispatches to a frozen-
    /// lockfile flow — see
    /// [`crate::InstallFrozenLockfile::run`].
    ///
    /// `allow_build_policy` drives engine-agnostic gating. When
    /// `Some`, the constructor walks `snapshots` once to collect
    /// every key whose `(name, version)` passes
    /// [`AllowBuildPolicy::check`] returning `Some(true)`, then
    /// passes that set as `built_dep_paths` to
    /// [`calc_graph_node_hash`]. Pure-JS subgraphs hash with
    /// `engine = null` so their GVS directories survive Node.js
    /// upgrades. When `None`, every snapshot keeps the engine in
    /// its hash payload — matches upstream's
    /// [`builtDepPaths === undefined`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L140-L142)
    /// branch and the existing pacquet behaviour from
    /// pnpm/pacquet#449.
    pub fn new(
        config: &Config,
        engine: Option<&str>,
        snapshots: Option<&HashMap<PackageKey, SnapshotEntry>>,
        packages: Option<&HashMap<PackageKey, PackageMetadata>>,
        allow_build_policy: Option<&AllowBuildPolicy>,
    ) -> Self {
        // Pacquet keeps `virtual_store_dir` and `global_virtual_store_dir`
        // as two separate fields (see
        // [`Config::apply_global_virtual_store_derivation`] for why).
        // The frozen-lockfile install picks
        // `global_virtual_store_dir` here when GVS is on so the
        // without-lockfile path can stay on the project-local
        // `virtual_store_dir` without colliding.
        let package_store_dir = if config.enable_global_virtual_store {
            config.global_virtual_store_dir.clone()
        } else {
            config.virtual_store_dir.clone()
        };
        if !config.enable_global_virtual_store {
            return VirtualStoreLayout { package_store_dir, gvs_suffixes: None };
        }
        let Some(snapshots) = snapshots else {
            return VirtualStoreLayout { package_store_dir, gvs_suffixes: Some(HashMap::new()) };
        };
        let graph = lockfile_to_dep_graph(snapshots, packages);
        // Build the engine-agnostic gating set once per install,
        // mirroring upstream's
        // [`computeBuiltDepPaths`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L208-L219).
        // `None` here disables gating so every snapshot still hashes
        // with its engine string — the pre-pnpm/pacquet#459 behaviour.
        let built_dep_paths: Option<HashSet<PackageKey>> = allow_build_policy.map(|policy| {
            snapshots
                .keys()
                .filter(|k| {
                    let metadata_key = k.without_peer();
                    let name = metadata_key.name.to_string();
                    let version = metadata_key.suffix.version().to_string();
                    policy.check(&name, &version) == Some(true)
                })
                .cloned()
                .collect()
        });
        let mut cache: DepsStateCache<PackageKey> = HashMap::new();
        // Install-scoped memoization for the `transitivelyRequiresBuild`
        // walk; shared across every snapshot's hash computation so
        // diamond-shaped subgraphs only get visited once. Untouched
        // when `built_dep_paths` is `None`. Mirrors upstream's
        // [`buildRequiredCache`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L113-L114).
        let mut build_required_cache: HashMap<PackageKey, bool> = HashMap::new();
        let mut gvs_suffixes: HashMap<PackageKey, String> = HashMap::with_capacity(snapshots.len());
        for snapshot_key in snapshots.keys() {
            let hex_digest = calc_graph_node_hash(
                &graph,
                &mut cache,
                snapshot_key,
                engine,
                built_dep_paths.as_ref(),
                &mut build_required_cache,
            );
            let metadata_key = snapshot_key.without_peer();
            let name = metadata_key.name.to_string();
            let version = metadata_key.suffix.version().to_string();
            let suffix = format_global_virtual_store_path(&name, &version, &hex_digest);
            gvs_suffixes.insert(snapshot_key.clone(), suffix);
        }
        VirtualStoreLayout { package_store_dir, gvs_suffixes: Some(gvs_suffixes) }
    }

    /// Root of the layout — the directory that contains every per-
    /// snapshot subdirectory. Exposed so callers that need to pass a
    /// path to existing helpers (e.g. the
    /// [`pacquet_modules_yaml::Modules`] writer, which still records
    /// the legacy [`Config::virtual_store_dir`] string) have one
    /// source of truth.
    pub fn package_store_dir(&self) -> &Path {
        &self.package_store_dir
    }

    /// Whether this install is running in global-virtual-store mode.
    /// Mirrors `config.enable_global_virtual_store` — captured here so
    /// callers can ask the layout itself instead of having to keep a
    /// separate `&Config` reference for the boolean.
    pub fn enable_global_virtual_store(&self) -> bool {
        self.gvs_suffixes.is_some()
    }

    /// Absolute directory that holds `node_modules/<name>` for one
    /// snapshot. Falls back to
    /// [`PkgNameVerPeer::to_virtual_store_name`](pacquet_lockfile::PkgNameVerPeer::to_virtual_store_name)
    /// when GVS is off, or when GVS is on but the key isn't in the
    /// precomputed map (which would indicate a bug — every snapshot
    /// the install touches must have been visited in
    /// [`Self::new`]; the fallback is defensive rather than expected
    /// to fire).
    pub fn slot_dir(&self, key: &PackageKey) -> PathBuf {
        let suffix = match &self.gvs_suffixes {
            Some(map) => map.get(key).cloned().unwrap_or_else(|| key.to_virtual_store_name()),
            None => key.to_virtual_store_name(),
        };
        self.package_store_dir.join(suffix)
    }
}

/// Build the dependency graph from the lockfile's `snapshots` /
/// `packages` sections. Mirrors upstream's
/// [`lockfileToDepGraph`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L162-L181)
/// — every entry in `snapshots` becomes a node whose `full_pkg_id` is
/// `<pkg_id_with_patch_hash>:<integrity>` (for tarball / registry
/// resolutions) and whose `children` are the alias→snapshot-key edges
/// pulled from the snapshot's combined `dependencies` +
/// `optionalDependencies`.
///
/// Packages whose metadata is missing or whose resolution has no
/// `integrity` (directory / git) are emitted with the bare
/// `pkg_id_with_patch_hash` as their `full_pkg_id`. The frozen-
/// lockfile install path rejects those resolutions before reaching the
/// linker, so a stub `full_pkg_id` here is safe — the GVS hash for an
/// install that contains one of those snapshots is irrelevant because
/// the install will error out before consulting it.
fn lockfile_to_dep_graph(
    snapshots: &HashMap<PackageKey, SnapshotEntry>,
    packages: Option<&HashMap<PackageKey, PackageMetadata>>,
) -> HashMap<PackageKey, DepsGraphNode<PackageKey>> {
    snapshots
        .iter()
        .map(|(snapshot_key, snapshot)| {
            let children = collect_children(snapshot);
            let metadata_key = snapshot_key.without_peer();
            let pkg_id_with_patch_hash = PkgIdWithPatchHash::from(metadata_key.to_string());
            let resolution = packages.and_then(|m| m.get(&metadata_key)).map(|m| &m.resolution);
            let full_pkg_id = create_full_pkg_id(&pkg_id_with_patch_hash, resolution);
            (snapshot_key.clone(), DepsGraphNode { full_pkg_id, children })
        })
        .collect()
}

/// Combine a snapshot's `dependencies` and `optionalDependencies` into
/// the graph's alias→key edges. Mirrors
/// [`lockfileDepsToGraphChildren`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L237-L246)
/// composed with upstream's `{...deps, ...optionalDeps}` spread at the
/// caller.
fn collect_children(snapshot: &SnapshotEntry) -> HashMap<String, PackageKey> {
    let mut children = HashMap::new();
    if let Some(deps) = &snapshot.dependencies {
        merge_into_children(&mut children, deps);
    }
    if let Some(deps) = &snapshot.optional_dependencies {
        merge_into_children(&mut children, deps);
    }
    children
}

fn merge_into_children(
    children: &mut HashMap<String, PackageKey>,
    deps: &HashMap<PkgName, SnapshotDepRef>,
) {
    for (alias, dep_ref) in deps {
        let resolved = dep_ref.resolve(alias);
        children.insert(alias.to_string(), resolved);
    }
}

/// Mirrors upstream's
/// [`createFullPkgId`](https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-hasher/src/index.ts#L248-L274).
/// `variations` (cross-platform variant) resolutions don't exist in
/// pacquet's lockfile model yet — when they're added, this helper
/// will need the `selectPlatformVariant` branch upstream uses to pick
/// the right integrity.
fn create_full_pkg_id(
    pkg_id_with_patch_hash: &PkgIdWithPatchHash,
    resolution: Option<&LockfileResolution>,
) -> String {
    match resolution.and_then(LockfileResolution::integrity) {
        Some(integrity) => format!("{pkg_id_with_patch_hash}:{integrity}"),
        // Directory / git / missing-metadata fall through to the bare
        // id. The install path rejects these resolutions before the
        // hash is consulted (see
        // [`crate::InstallPackageBySnapshotError::UnsupportedResolution`]),
        // so the value never actually drives a slot path on disk.
        None => pkg_id_with_patch_hash.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::VirtualStoreLayout;
    use pacquet_config::Config;
    use pacquet_lockfile::{
        LockfileResolution, PackageKey, PackageMetadata, RegistryResolution, SnapshotEntry,
    };
    use pretty_assertions::assert_eq;
    use std::{collections::HashMap, path::PathBuf};

    /// Build a `Config` test-double with the GVS-relevant fields
    /// wired explicitly. `gvs_dir` populates `global_virtual_store_dir`
    /// for the GVS-on path; `virtual_store_dir` stays at the
    /// project-local default for the GVS-off path.
    fn make_config(gvs: bool, virtual_store_dir: PathBuf, gvs_dir: PathBuf) -> Config {
        let mut config = Config::new();
        config.enable_global_virtual_store = gvs;
        config.virtual_store_dir = virtual_store_dir;
        config.global_virtual_store_dir = gvs_dir;
        config
    }

    /// With GVS off, the layout reproduces today's flat-name layout
    /// (`<virtual_store_dir>/<flat-name>`) — proving the helper is a
    /// drop-in for the legacy path.
    #[test]
    fn slot_dir_uses_flat_name_when_gvs_off() {
        let config = make_config(
            false,
            PathBuf::from("/tmp/proj/node_modules/.pnpm"),
            PathBuf::from("/tmp/store/links"),
        );
        let layout = VirtualStoreLayout::new(&config, Some("ignored"), None, None, None);
        let key: PackageKey = "@scope/foo@1.2.3".parse().unwrap();
        assert_eq!(
            layout.slot_dir(&key),
            PathBuf::from("/tmp/proj/node_modules/.pnpm/@scope+foo@1.2.3"),
        );
    }

    /// With GVS on and a single snapshot, the layout produces the
    /// `<root>/<scope>/<name>/<version>/<hash>` shape upstream's tests
    /// assert against. The hash is opaque; we only check the prefix
    /// and depth.
    #[test]
    fn slot_dir_uses_gvs_layout_when_gvs_on() {
        let config = make_config(
            true,
            PathBuf::from("/tmp/proj/node_modules/.pnpm"),
            PathBuf::from("/tmp/store/links"),
        );
        let key: PackageKey = "@scope/foo@1.2.3".parse().unwrap();
        let mut packages = HashMap::new();
        packages.insert(
            key.clone(),
            PackageMetadata {
                resolution: LockfileResolution::Registry(RegistryResolution {
                    integrity: "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                        .parse()
                        .expect("parse integrity"),
                }),
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
            },
        );
        let mut snapshots = HashMap::new();
        snapshots.insert(key.clone(), SnapshotEntry::default());
        let layout = VirtualStoreLayout::new(
            &config,
            Some("darwin-arm64-node20"),
            Some(&snapshots),
            Some(&packages),
            None,
        );
        let slot = layout.slot_dir(&key);
        // Shape: `/tmp/store/links/@scope/foo/1.2.3/<64-hex>`.
        let stripped = slot
            .strip_prefix("/tmp/store/links/@scope/foo/1.2.3/")
            .expect("slot dir must live under <root>/<scope>/<name>/<version>/ when GVS is on");
        assert_eq!(
            stripped.to_string_lossy().len(),
            64,
            "trailing hash component must be a full sha256 hex digest",
        );
    }

    /// Unscoped packages get an `@/` prefix so every entry in the
    /// shared store sits at the same `<scope>/<name>/<version>/<hash>`
    /// depth — easier `readdir`-driven traversal.
    #[test]
    fn slot_dir_prefixes_unscoped_with_at_slash_under_gvs() {
        let config = make_config(
            true,
            PathBuf::from("/tmp/proj/node_modules/.pnpm"),
            PathBuf::from("/tmp/store/links"),
        );
        let key: PackageKey = "foo@1.0.0".parse().unwrap();
        let mut packages = HashMap::new();
        packages.insert(
            key.clone(),
            PackageMetadata {
                resolution: LockfileResolution::Registry(RegistryResolution {
                    integrity: "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                        .parse()
                        .expect("parse integrity"),
                }),
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
            },
        );
        let mut snapshots = HashMap::new();
        snapshots.insert(key.clone(), SnapshotEntry::default());
        let layout = VirtualStoreLayout::new(
            &config,
            Some("linux-x64-node22"),
            Some(&snapshots),
            Some(&packages),
            None,
        );
        let slot = layout.slot_dir(&key);
        let _ = slot
            .strip_prefix("/tmp/store/links/@/foo/1.0.0/")
            .expect("unscoped GVS slots live under <root>/@/<name>/<version>/<hash>");
    }

    /// End-to-end gating check: a pure-JS snapshot's GVS slot is
    /// engine-agnostic when an empty `AllowBuildPolicy` is supplied
    /// (matches upstream's
    /// [`enableGlobalVirtualStore: true` → `allowBuilds ??= {}`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/index.ts#L342-L344)
    /// shape). Two installs that differ only in the `engine` string
    /// produce the *same* slot directory.
    #[test]
    fn slot_dir_engine_agnostic_with_empty_allow_build_policy() {
        let config = make_config(
            true,
            PathBuf::from("/tmp/proj/node_modules/.pnpm"),
            PathBuf::from("/tmp/store/links"),
        );
        let key: PackageKey = "left-pad@1.0.0".parse().unwrap();
        let mut packages = HashMap::new();
        packages.insert(
            key.clone(),
            PackageMetadata {
                resolution: LockfileResolution::Registry(RegistryResolution {
                    integrity: "sha512-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
                        .parse()
                        .expect("parse integrity"),
                }),
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
            },
        );
        let mut snapshots = HashMap::new();
        snapshots.insert(key.clone(), SnapshotEntry::default());
        let policy = crate::AllowBuildPolicy::default();
        let darwin = VirtualStoreLayout::new(
            &config,
            Some("darwin-arm64-node20"),
            Some(&snapshots),
            Some(&packages),
            Some(&policy),
        )
        .slot_dir(&key);
        let linux = VirtualStoreLayout::new(
            &config,
            Some("linux-x64-node22"),
            Some(&snapshots),
            Some(&packages),
            Some(&policy),
        )
        .slot_dir(&key);
        assert_eq!(
            darwin, linux,
            "pure-JS snapshot must share one GVS slot across engines when gating is active",
        );
    }

    /// Symmetric to [`slot_dir_engine_agnostic_with_empty_allow_build_policy`]:
    /// when the snapshot is in `allow_builds`, the engine *is* part
    /// of the slot path. Two installs that differ in `engine` end up
    /// in different directories.
    #[test]
    fn slot_dir_engine_specific_when_snapshot_is_built() {
        let config = make_config(
            true,
            PathBuf::from("/tmp/proj/node_modules/.pnpm"),
            PathBuf::from("/tmp/store/links"),
        );
        let key: PackageKey = "native-pkg@1.0.0".parse().unwrap();
        let mut packages = HashMap::new();
        packages.insert(
            key.clone(),
            PackageMetadata {
                resolution: LockfileResolution::Registry(RegistryResolution {
                    integrity: "sha512-CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"
                        .parse()
                        .expect("parse integrity"),
                }),
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
            },
        );
        let mut snapshots = HashMap::new();
        snapshots.insert(key.clone(), SnapshotEntry::default());
        let allowed: std::collections::HashSet<String> =
            ["native-pkg".to_string()].into_iter().collect();
        let policy = crate::AllowBuildPolicy::new(allowed, std::collections::HashSet::new(), false);
        let darwin = VirtualStoreLayout::new(
            &config,
            Some("darwin-arm64-node20"),
            Some(&snapshots),
            Some(&packages),
            Some(&policy),
        )
        .slot_dir(&key);
        let linux = VirtualStoreLayout::new(
            &config,
            Some("linux-x64-node22"),
            Some(&snapshots),
            Some(&packages),
            Some(&policy),
        )
        .slot_dir(&key);
        assert_ne!(darwin, linux, "builder snapshot must partition GVS slot by engine string");
    }
}
