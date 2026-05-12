use crate::build_sequence::build_sequence;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_config::Config;
use pacquet_executor::{
    LifecycleScriptError, RunPostinstallHooks, ScriptsPrependNodePath, run_postinstall_hooks,
};
use pacquet_lockfile::{PackageKey, ProjectSnapshot, SnapshotEntry};
use pacquet_package_manifest::pkg_requires_build;
use pacquet_patching::{PatchApplyError, apply_patch_to_dir};
use pacquet_reporter::{
    LogEvent, LogLevel, Reporter, SkippedOptionalDependencyLog, SkippedOptionalPackage,
    SkippedOptionalReason,
};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};

/// Error from the build-modules step.
#[derive(Debug, Display, Error, Diagnostic)]
pub enum BuildModulesError {
    #[diagnostic(transparent)]
    LifecycleScript(#[error(source)] LifecycleScriptError),

    #[diagnostic(transparent)]
    PatchApply(#[error(source)] PatchApplyError),

    /// Mirrors upstream's
    /// [`ERR_PNPM_PATCH_FILE_PATH_MISSING`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/during-install/src/index.ts#L172-L176)
    /// — fired when a snapshot's resolved patch carries a hash but
    /// no `patch_file_path`. The hash-without-path shape can come
    /// from the lockfile when no live config provides the path, so
    /// the user must add an entry to `patchedDependencies` in
    /// `pnpm-workspace.yaml` to bring the file back into scope.
    #[display("Cannot apply patch for {dep_path}: patch file path is missing")]
    #[diagnostic(
        code(ERR_PNPM_PATCH_FILE_PATH_MISSING),
        help("Ensure the package is listed in patchedDependencies configuration")
    )]
    PatchFilePathMissing { dep_path: String },
}

/// Build policy derived from `allowBuilds` and
/// `dangerouslyAllowAllBuilds` in `pnpm-workspace.yaml`.
///
/// Ports pnpm's `createAllowBuildFunction` from
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/policy/src/index.ts>.
///
/// The tri-state return from [`AllowBuildPolicy::check`]:
/// - `Some(true)`: explicitly allowed, run scripts
/// - `Some(false)`: explicitly denied, silently skip
/// - `None`: not in the list, skip and report as ignored
#[derive(Debug, Default)]
pub struct AllowBuildPolicy {
    rules: HashMap<String, bool>,
    dangerously_allow_all: bool,
}

impl AllowBuildPolicy {
    /// Build a policy from already-parsed `allowBuilds` rules and
    /// `dangerouslyAllowAllBuilds`. Pure constructor — no IO — so
    /// the policy logic is tested directly with in-memory inputs
    /// (mirrors upstream's `createAllowBuildFunction(opts)` in
    /// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/policy/src/index.ts>).
    pub fn new(rules: HashMap<String, bool>, dangerously_allow_all: bool) -> Self {
        Self { rules, dangerously_allow_all }
    }

    /// Build the policy from a resolved [`Config`]. Reads
    /// `allow_builds` and `dangerously_allow_all_builds`, which are
    /// populated by [`pacquet_config::WorkspaceSettings::apply_to`]
    /// from `pnpm-workspace.yaml`. pnpm v11 stopped reading these
    /// from `package.json#pnpm` — see pnpm/pacquet#397 item 5.
    pub fn from_config(config: &Config) -> Self {
        Self::new(config.allow_builds.clone(), config.dangerously_allow_all_builds)
    }

    /// Check whether a package is allowed to run build scripts.
    ///
    /// Returns:
    /// - `Some(true)`: explicitly allowed (or `dangerouslyAllowAllBuilds`)
    /// - `Some(false)`: explicitly denied, silently skip
    /// - `None`: not in the list, skip and report as ignored
    pub fn check(&self, name: &str, version: &str) -> Option<bool> {
        if self.dangerously_allow_all {
            return Some(true);
        }

        let exact_key = format!("{name}@{version}");
        if let Some(&allowed) = self.rules.get(&exact_key) {
            return Some(allowed);
        }

        if let Some(&allowed) = self.rules.get(name) {
            return Some(allowed);
        }

        None
    }
}

/// Run lifecycle scripts for all packages that require a build.
///
/// Ports the core of `buildModules` from
/// `https://github.com/pnpm/pnpm/blob/80037699fb/building/during-install/src/index.ts`.
///
/// Packages are visited in topological order (children before parents) via
/// [`build_sequence`]. Within a chunk, members are independent and could run
/// concurrently — pacquet currently runs them sequentially (TODO: honor
/// `childConcurrency`).
pub struct BuildModules<'a> {
    pub virtual_store_dir: &'a Path,
    pub modules_dir: &'a Path,
    pub lockfile_dir: &'a Path,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    pub packages: Option<&'a HashMap<PackageKey, pacquet_lockfile::PackageMetadata>>,
    pub importers: &'a HashMap<String, ProjectSnapshot>,
    pub allow_build_policy: &'a AllowBuildPolicy,
    /// Per-snapshot side-effects-cache overlays — passed in from
    /// `CreateVirtualStore`'s prefetch. `None` means the cache is
    /// disabled or no rows were prefetched; the gate falls through
    /// to "rebuild" for every snapshot.
    pub side_effects_maps_by_snapshot: Option<&'a crate::SideEffectsMapsBySnapshot>,
    /// `<platform>;<arch>;node<major>` — the prefix part of
    /// upstream's dep-state cache key. Computed once at install
    /// start by [`pacquet_graph_hasher::detect_node_major`] +
    /// [`pacquet_graph_hasher::engine_name`]. When `None`, the
    /// gate falls through to "rebuild" (no key to look up).
    pub engine_name: Option<&'a str>,
    /// Mirrors `config.side_effects_cache`. When `false`, the
    /// gate is bypassed entirely and every `requires_build`
    /// snapshot runs its scripts.
    pub side_effects_cache: bool,
    /// Mirrors upstream's `sideEffectsCacheWrite` at
    /// <https://github.com/pnpm/pnpm/blob/7e3145f9fc/config/reader/src/index.ts#L615>.
    /// When `true`, a successful postinstall triggers a re-CAFS of
    /// the built package directory and a queued mutation of the
    /// matching `PackageFilesIndex.sideEffects` row.
    pub side_effects_cache_write: bool,
    /// Store-dir handle for the WRITE path's `add_files_from_dir`
    /// call. `None` short-circuits the upload site entirely — used
    /// by unit tests that don't set up a CAFS.
    pub store_dir: Option<&'a pacquet_store_dir::StoreDir>,
    /// Shared batched writer for the side-effects upload's
    /// read-modify-write of the existing `PackageFilesIndex` row.
    /// `None` short-circuits the upload site.
    pub store_index_writer: Option<&'a std::sync::Arc<pacquet_store_dir::StoreIndexWriter>>,
    /// Per-snapshot resolved patch metadata. Keyed by the snapshot's
    /// peer-stripped `PackageKey`, value is the matching
    /// `ExtendedPatchInfo` (hash + absolute path) computed by
    /// [`pacquet_patching::resolve_and_group`] + per-snapshot
    /// [`pacquet_patching::get_patch_info`]. `None` when no
    /// `patchedDependencies` is configured.
    ///
    /// Drives three things, mirroring upstream's
    /// [`during-install`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/during-install/src/index.ts)
    /// flow:
    ///
    /// 1. Build trigger — a snapshot with a patch entry becomes a
    ///    build candidate even when `requires_build` is false.
    /// 2. Side-effects-cache key — `patch_file_hash` carries the
    ///    SHA-256 hex into [`pacquet_graph_hasher::CalcDepStateOptions`].
    /// 3. Patch application — the patch is applied to the extracted
    ///    package dir before postinstall hooks run.
    pub patches: Option<&'a HashMap<PackageKey, pacquet_patching::ExtendedPatchInfo>>,
}

impl<'a> BuildModules<'a> {
    /// Run the build, returning the sorted set of `name@version` keys whose
    /// scripts were skipped because the package was not in `allowBuilds`.
    ///
    /// The caller is expected to fold the returned set into a single
    /// `pnpm:ignored-scripts` event — mirroring upstream's emit at
    /// <https://github.com/pnpm/pnpm/blob/80037699fb/installing/deps-installer/src/install/index.ts#L414>.
    pub fn run<R: Reporter>(self) -> Result<Vec<String>, BuildModulesError> {
        let BuildModules {
            virtual_store_dir,
            modules_dir,
            lockfile_dir,
            snapshots,
            packages,
            importers,
            allow_build_policy,
            side_effects_maps_by_snapshot,
            engine_name,
            side_effects_cache,
            side_effects_cache_write,
            store_dir,
            store_index_writer,
            patches,
        } = self;

        let Some(snapshots) = snapshots else { return Ok(Vec::new()) };

        let extra_env = HashMap::new();
        let extra_bin_paths: Vec<PathBuf> = vec![];

        // Compute requires_build per snapshot from each extracted package
        // directory. Mirrors upstream where the worker computes
        // `node.requiresBuild` from the package's manifest scripts and the
        // presence of `binding.gyp` / `.hooks/` after extraction
        // (`https://github.com/pnpm/pnpm/blob/80037699fb/building/pkg-requires-build/src/index.ts`).
        // Pacquet does this here rather than in a worker because the worker
        // does not exist yet — it is the same per-package on-disk inspection,
        // moved to the build entry point.
        let requires_build_map: HashMap<PackageKey, bool> = snapshots
            .keys()
            .map(|key| {
                let pkg_dir = virtual_store_dir_for_key(virtual_store_dir, key);
                (key.clone(), pkg_requires_build(&pkg_dir))
            })
            .collect();

        // Build the dep graph + state cache only when the
        // side-effects-cache gate has a chance of firing — on
        // either the READ side (prefetch surfaced cache rows) or
        // the WRITE side (the install will be populating new
        // cache entries after a successful build).
        //
        // The graph is bounded to the *forward closure of
        // `requires_build` snapshots* via `build_deps_subgraph`.
        // The upload-site and gate-check loops only ever compute
        // cache keys for `requires_build` snapshots (the
        // `continue` at the top of the chunk loop), and
        // `calc_dep_state` only recurses into a snapshot's own
        // children, so the closure-bounded graph produces the
        // exact same cache keys as the full graph for every
        // root we'll query. A pure-JS install with no
        // `requires_build` snapshots feeds in an empty root
        // iterator and the function returns immediately —
        // O(0) walk for that path.
        //
        // Mirrors upstream's per-install `DepsStateCache` at
        // <https://github.com/pnpm/pnpm/blob/7e3145f9fc/building/during-install/src/index.ts#L74>.
        // The cache memoizes per-node hash across diamond-shaped
        // subgraphs so the recursive walk stays linear in
        // |closure| even when the same dep is reachable through
        // many parents.
        let read_gate_active = side_effects_cache
            && engine_name.is_some()
            && side_effects_maps_by_snapshot.is_some_and(|m| !m.is_empty());
        let write_gate_active = side_effects_cache_write
            && engine_name.is_some()
            && store_index_writer.is_some()
            && store_dir.is_some();
        let cache_gate_active = (read_gate_active || write_gate_active) && packages.is_some();
        let dep_graph = cache_gate_active.then(|| {
            let roots = requires_build_map
                .iter()
                .filter(|&(_, &requires_build)| requires_build)
                .map(|(key, _)| key.clone());
            crate::build_deps_subgraph(
                snapshots,
                packages.expect("`cache_gate_active` requires packages: Some"),
                roots,
            )
        });
        let mut deps_state_cache: pacquet_graph_hasher::DepsStateCache<PackageKey> =
            pacquet_graph_hasher::DepsStateCache::new();

        let chunks = build_sequence(&requires_build_map, patches, snapshots, importers);

        // Collect peer-stripped keys so the final list is unique and
        // sorted lexicographically — matches `dedupePackageNamesFromIgnoredBuilds`.
        let mut ignored_builds: BTreeSet<String> = BTreeSet::new();

        for chunk in chunks {
            for snapshot_key in chunk {
                let metadata_key = snapshot_key.without_peer();
                // Look up against the peer-stripped key because
                // patches are configured at the (name, version)
                // granularity in `pnpm-workspace.yaml`, not per
                // peer-resolution variant.
                let patch = patches.and_then(|map| map.get(&metadata_key));
                let has_patch = patch.is_some();
                let requires_build =
                    requires_build_map.get(&snapshot_key).copied().unwrap_or(false);

                // Ancestors of a build/patch candidate are included
                // in the sequence (so the topo order stays correct)
                // but only run scripts / apply patches when they
                // themselves are candidates. Mirrors upstream's
                // chunk filter at
                // <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/during-install/src/index.ts#L73-L77>.
                if !requires_build && !has_patch {
                    continue;
                }

                let (name, version) = parse_name_version_from_key(&metadata_key.to_string());

                // Mirrors upstream's `if (node.requiresBuild) { allowBuild(...) }`
                // at lines 88-101: the allowBuilds gate only applies
                // when the node has scripts to run. A patched-only
                // package skips this check entirely and proceeds to
                // patch application below.
                //
                // `false` / `None` from the policy set
                // `should_run_scripts = false` (NOT `continue`), so
                // the patch still gets applied even when scripts
                // are disallowed. Matches upstream's `ignoreScripts
                // = true; break` pattern.
                let mut should_run_scripts = requires_build;
                if requires_build {
                    match allow_build_policy.check(&name, &version) {
                        Some(false) => {
                            should_run_scripts = false;
                        }
                        None => {
                            // "Not in allowBuilds" — surfaced as
                            // `pnpm:ignored-scripts`. Explicit
                            // `false` is silently denied (above),
                            // matching upstream's switch.
                            ignored_builds.insert(metadata_key.to_string());
                            should_run_scripts = false;
                        }
                        Some(true) => {}
                    }
                }

                // Compute the side-effects cache key once per
                // snapshot, before the `is_built` gate. The same
                // value is later consumed by the WRITE-path upload
                // call after `run_postinstall_hooks` succeeds, so
                // recomputing it there would just duplicate work —
                // `deps_state_cache` makes the second call free
                // anyway, but routing through one `let` keeps the
                // gate-side and write-side keys provably identical.
                //
                // `None` when the cache gate can't fire (no engine,
                // no graph, etc.); both downstream consumers
                // short-circuit on `None`.
                let cache_key = (dep_graph.as_ref().zip(engine_name)).map(|(graph, engine)| {
                    pacquet_graph_hasher::calc_dep_state(
                        graph,
                        &mut deps_state_cache,
                        &snapshot_key,
                        &pacquet_graph_hasher::CalcDepStateOptions {
                            engine_name: engine,
                            // Mirrors upstream's
                            // `patchFileHash: depNode.patch?.hash`
                            // at
                            // <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/during-install/src/index.ts#L201>.
                            // `None` for unpatched snapshots leaves
                            // the `;patch=...` segment off the cache
                            // key entirely, matching upstream when
                            // `depNode.patch == null`.
                            patch_file_hash: patch.map(|p| p.hash.as_str()),
                            // Mirrors `includeDepGraphHash: hasSideEffects`
                            // at upstream line 202. A patched-only
                            // snapshot (no scripts will run) leaves
                            // the deps-hash off so the cache key
                            // stays stable across dep-graph changes
                            // that don't affect this package's
                            // patched output.
                            include_dep_graph_hash: should_run_scripts,
                        },
                    )
                });

                // Side-effects-cache `is_built` gate. Mirrors
                // upstream's `!node.isBuilt` filter at
                // <https://github.com/pnpm/pnpm/blob/7e3145f9fc/building/during-install/src/index.ts#L73-L77>.
                // We're already past the policy gate, so this
                // snapshot would otherwise run its scripts — but if
                // the prefetch surfaced a matching side-effects-cache
                // entry, the build is already represented on disk
                // (pnpm seeded it on a previous install) and we
                // can skip.
                if side_effects_cache
                    && let Some(maps_by_snapshot) = side_effects_maps_by_snapshot
                    && let Some(maps) = maps_by_snapshot.get(&snapshot_key)
                    && let Some(key) = cache_key.as_deref()
                    && maps.contains_key(key)
                {
                    tracing::debug!(
                        target: "pacquet::build",
                        ?snapshot_key,
                        cache_key = key,
                        "side-effects cache hit; skipping build",
                    );
                    continue;
                }

                let pkg_dir = virtual_store_dir_for_key(virtual_store_dir, &snapshot_key);
                if !pkg_dir.exists() {
                    continue;
                }

                let optional = snapshots.get(&snapshot_key).is_some_and(|entry| entry.optional);

                // Apply the patch before running postinstall hooks.
                // Mirrors upstream at
                // <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/during-install/src/index.ts#L171-L178>:
                // ```
                // if (depNode.patch) {
                //   if (!depNode.patch.patchFilePath) throw PATCH_FILE_PATH_MISSING
                //   isPatched = applyPatchToDir(...)
                // }
                // ```
                // `is_patched` feeds the cache-write gate below
                // (`is_patched || has_side_effects`), matching
                // upstream's line 199 condition.
                let is_patched = if let Some(p) = patch {
                    let patch_file_path = p.patch_file_path.as_deref().ok_or_else(|| {
                        BuildModulesError::PatchFilePathMissing {
                            dep_path: snapshot_key.to_string(),
                        }
                    })?;
                    apply_patch_to_dir(&pkg_dir, patch_file_path)
                        .map_err(BuildModulesError::PatchApply)?;
                    true
                } else {
                    false
                };

                let has_side_effects = if should_run_scripts {
                    let result = run_postinstall_hooks::<R>(RunPostinstallHooks {
                        dep_path: &snapshot_key.to_string(),
                        pkg_root: &pkg_dir,
                        root_modules_dir: modules_dir,
                        init_cwd: lockfile_dir,
                        extra_bin_paths: &extra_bin_paths,
                        extra_env: &extra_env,
                        node_execpath: None,
                        npm_execpath: None,
                        node_gyp_path: None,
                        user_agent: None,
                        // Hard-coded until the `unsafe-perm` config knob
                        // is plumbed through. `true` skips both the
                        // TMPDIR creation and the uid/gid drop, matching
                        // pacquet's behavior before any of this landed.
                        unsafe_perm: true,
                        node_gyp_bin: None,
                        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
                        script_shell: None,
                        optional,
                    });

                    match result {
                        Ok(ran) => ran,
                        Err(err) => {
                            if optional {
                                // Mirrors `building/during-install/src/index.ts:226-238`:
                                // a build failure on an optional dep is logged
                                // through the `pnpm:skipped-optional-dependency`
                                // channel and swallowed so the install can
                                // continue. The `package.id` field upstream is
                                // `depNode.dir`; we use the same.
                                R::emit(&LogEvent::SkippedOptionalDependency(
                                    SkippedOptionalDependencyLog {
                                        level: LogLevel::Debug,
                                        details: Some(err.to_string()),
                                        package: SkippedOptionalPackage {
                                            id: pkg_dir.to_string_lossy().into_owned(),
                                            name: name.clone(),
                                            version: version.clone(),
                                        },
                                        prefix: lockfile_dir.to_string_lossy().into_owned(),
                                        reason: SkippedOptionalReason::BuildFailure,
                                    },
                                ));
                                continue;
                            }
                            return Err(BuildModulesError::LifecycleScript(err));
                        }
                    }
                } else {
                    false
                };

                // Side-effects-cache WRITE path. Mirrors
                // `<https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/during-install/src/index.ts#L198-L216>`:
                // after a successful `run_postinstall_hooks` (or a
                // patch application that mutated the dir),
                // re-hash the package directory and queue a
                // `PackageFilesIndex.sideEffects[cache_key] = diff`
                // mutation so a future install can skip the
                // rebuild.
                //
                // Upstream's gate is `(isPatched || hasSideEffects)
                // && opts.sideEffectsCacheWrite`. Pacquet mirrors
                // that — a patched-only snapshot still uploads its
                // post-patch state so subsequent installs hit the
                // cache.
                //
                // The other preconditions: cache_key composable
                // (engine + graph present), `packages` map
                // available for the integrity lookup, and the
                // metadata row carries an integrity (registry /
                // tarball resolutions — git / directory have no
                // integrity and pnpm doesn't cache those either).
                //
                // All errors are swallowed with a `tracing::warn!`,
                // matching upstream's `try { upload } catch (err) {
                // logger.warn(...) }` at lines 208-215. A failed
                // upload doesn't fail the install: the next install
                // re-runs the build.
                if (is_patched || has_side_effects)
                    && side_effects_cache_write
                    && let Some(writer) = store_index_writer
                    && let Some(store) = store_dir
                    && let Some(cache_key) = cache_key.as_deref()
                    && let Some(packages) = packages
                    && let Some(metadata) = packages.get(&metadata_key)
                    && let Some(integrity) = metadata.resolution.integrity()
                {
                    let files_index_file = pacquet_store_dir::store_index_key(
                        &integrity.to_string(),
                        &metadata_key.to_string(),
                    );
                    if let Err(err) = pacquet_store_dir::upload(
                        store,
                        &pkg_dir,
                        &files_index_file,
                        cache_key,
                        writer,
                    ) {
                        tracing::warn!(
                            target: "pacquet::build",
                            ?err,
                            dep_path = %snapshot_key,
                            "side-effects cache upload failed; build proceeds",
                        );
                    }
                }
            }
        }

        Ok(ignored_builds.into_iter().collect())
    }
}

/// Compute the package directory inside the virtual store for a snapshot key.
///
/// Uses `without_peer()` to strip any peer-dependency suffix before
/// computing the path, so keys like
/// `/@pnpm.e2e/foo@1.0.0(@pnpm.e2e/bar@2.0.0)` resolve correctly.
fn virtual_store_dir_for_key(virtual_store_dir: &Path, key: &PackageKey) -> PathBuf {
    let bare_key = key.without_peer();
    let key_str = bare_key.to_string();
    let name_version = key_str.strip_prefix('/').unwrap_or(&key_str);

    let at_idx = name_version.rfind('@').unwrap_or(name_version.len());
    let name = &name_version[..at_idx];

    let store_name = name_version.replace('/', "+");

    virtual_store_dir.join(&store_name).join("node_modules").join(name)
}

/// Parse `name` and `version` from a lockfile snapshot key like
/// `/@pnpm.e2e/install-script-example@1.0.0`.
pub(crate) fn parse_name_version_from_key(key: &str) -> (String, String) {
    let s = key.strip_prefix('/').unwrap_or(key);
    match s.rfind('@') {
        Some(idx) if idx > 0 => (s[..idx].to_string(), s[idx + 1..].to_string()),
        _ => (s.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests;
