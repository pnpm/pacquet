//! Per-install installability pass.
//!
//! For each snapshot in a frozen-lockfile install, run
//! `pacquet-package-is-installable`'s `check_package` against the
//! matching `PackageMetadata` and the host environment, build the
//! [`SkippedSnapshots`] set, and emit
//! `pnpm:skipped-optional-dependency` for every optional+incompatible
//! one.
//!
//! Mirrors the union of upstream's:
//! - The resolver-side gate at
//!   <https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-resolver/src/resolveDependencies.ts#L1307-L1312>.
//! - The headless re-check at
//!   <https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L206-L215>.
//!
//! Pacquet's install path is lockfile-driven and has no resolver, so
//! the headless re-check is the only relevant emit site. Running it
//! every install also means the set is recomputed against the current
//! host — pnpm's `lockfileToDepGraph` does exactly the same, and the
//! comment at upstream's `:194-215` calls out that the host arch may
//! have changed since the previous install wrote `.modules.yaml`.

use std::collections::{HashMap, HashSet};

use pacquet_lockfile::{PackageKey, PackageMetadata, SnapshotEntry};
use pacquet_package_is_installable::{
    InstallabilityError, InstallabilityOptions, PackageInstallabilityManifest, SkipReason,
    SupportedArchitectures, WantedEngine, check_package,
};
use pacquet_reporter::{
    LogEvent, LogLevel, Reporter, SkippedOptionalDependencyLog, SkippedOptionalPackage,
    SkippedOptionalReason,
};

/// The set of snapshot keys skipped on this host.
#[derive(Debug, Default, Clone)]
pub struct SkippedSnapshots {
    set: HashSet<PackageKey>,
}

impl SkippedSnapshots {
    pub fn new() -> Self {
        Self { set: HashSet::new() }
    }

    /// Construct a [`SkippedSnapshots`] from an existing set. Test
    /// helper for callers that want to drive build-sequence /
    /// virtual-store filtering against a known skip set without
    /// running the full installability pass.
    #[cfg(test)]
    pub(crate) fn from_set(set: HashSet<PackageKey>) -> Self {
        Self { set }
    }

    pub fn contains(&self, key: &PackageKey) -> bool {
        self.set.contains(key)
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PackageKey> + '_ {
        self.set.iter()
    }
}

/// Host context for the installability check. Built once per install
/// so the per-snapshot calls don't each re-spawn `node --version`
/// or re-read `std::env::consts::OS`.
pub struct InstallabilityHost {
    pub node_version: String,
    /// `true` when `node_version` was discovered by spawning
    /// `node --version`; `false` when the field carries the synthetic
    /// fallback. The side-effects-cache key derives from this — a
    /// fallback version must not seed the cache because subsequent
    /// installs would key on the actual node major and miss every
    /// row written under the fallback.
    pub node_detected: bool,
    pub os: &'static str,
    pub cpu: &'static str,
    pub libc: &'static str,
    pub supported_architectures: Option<SupportedArchitectures>,
    pub engine_strict: bool,
}

impl InstallabilityHost {
    /// Resolve the host context from the running process.
    ///
    /// `node_version` is detected via
    /// [`pacquet_graph_hasher::detect_node_version`]; when detection
    /// fails (no `node` on PATH), pacquet falls back to a synthetic
    /// `99999.0.0` so `engines.node` ranges keep accepting packages.
    /// The alternative `0.0.0` would falsely-skip every optional
    /// dependency targeting any concrete node range, which is worse
    /// than the over-acceptance the very-high fallback produces.
    /// `node_detected` records which path was taken so callers can
    /// suppress side-effects-cache lookups when the version is
    /// synthetic. Slice 2 will wire a proper `nodeVersion` config
    /// setting and surface `ERR_PNPM_INVALID_NODE_VERSION` to match
    /// upstream's throw-on-detection-failure behavior.
    pub fn detect() -> Self {
        let detected = pacquet_graph_hasher::detect_node_version();
        let node_detected = detected.is_some();
        let node_version = detected.unwrap_or_else(|| "99999.0.0".to_string());
        Self {
            node_version,
            node_detected,
            os: pacquet_graph_hasher::host_platform(),
            cpu: pacquet_graph_hasher::host_arch(),
            libc: pacquet_graph_hasher::host_libc(),
            supported_architectures: None,
            engine_strict: false,
        }
    }
}

/// Compute the [`SkippedSnapshots`] set for a frozen-lockfile install.
///
/// For each `(snapshot_key, snapshot)`:
/// 1. Look up the matching `PackageMetadata` (skipping snapshots
///    without one — `CreateVirtualStore` will error on them
///    separately).
/// 2. Build a [`PackageInstallabilityManifest`] from `metadata.engines`,
///    `metadata.cpu`, `metadata.os`, `metadata.libc`.
/// 3. Run `check_package` against the host triple.
/// 4. Apply the per-snapshot dispatch:
///    - `Ok(None)`: compatible, nothing to do.
///    - `Ok(Some(err))` + `snapshot.optional`: add to the set; emit
///      `pnpm:skipped-optional-dependency`.
///    - `Ok(Some(err))` + `engine_strict`: return as the install
///      error. Pacquet's default has `engine_strict = false`, so
///      this path is currently unreachable from production — wired
///      for the slice that lands the config setting.
///    - `Ok(Some(err))` otherwise: emit `tracing::warn!` and proceed.
///      Upstream uses `pnpm:install-check` here, which pacquet's
///      reporter does not yet expose — slice 1 follow-up.
///    - `Err(InvalidNodeVersionError)`: surface as
///      `ERR_PNPM_INVALID_NODE_VERSION`.
pub fn compute_skipped_snapshots<R: Reporter>(
    snapshots: &HashMap<PackageKey, SnapshotEntry>,
    packages: &HashMap<PackageKey, PackageMetadata>,
    host: &InstallabilityHost,
    prefix: &str,
) -> Result<SkippedSnapshots, Box<InstallabilityError>> {
    // Fast path: if no package in the lockfile declares any
    // installability constraint, every snapshot is trivially
    // installable. Skip the per-snapshot
    // `without_peer()` / `to_string()` / `check_package` loop
    // entirely. Pacquet has no resolver so the lockfile's packages
    // map is fixed for the duration of the install; one linear scan
    // early is much cheaper than walking the snapshots map and
    // decomposing each metadata row only to find no constraints to
    // evaluate.
    //
    // Concretely on the integrated benchmark (1352 packages with no
    // platform / engine constraints): drops ~1352 `String` and
    // `PackageKey` allocations and the matching number of
    // `check_package` calls. The scan is O(N) on `packages` — same
    // shape as the loop it short-circuits — but does at most four
    // `Option::is_some` checks per row and short-circuits on the
    // first declared constraint.
    if !any_installability_constraint(packages) {
        return Ok(SkippedSnapshots::new());
    }

    let mut skipped = SkippedSnapshots::new();
    let mut seen_emit: HashSet<PackageKey> = HashSet::new();

    // Build the host-derived part of the options once. Only the
    // (`engine_strict`-irrelevant) `optional` flag varies per
    // snapshot, but the result of [`check_package`] — "does this
    // manifest satisfy the host?" — does not. We compute and cache
    // the check verdict per peer-stripped `metadata_key`; the
    // per-snapshot loop then only needs to apply the
    // optional / engine-strict dispatch.
    //
    // The cache pays off on lockfiles with peer-resolved variants of
    // the same package (`react-dom@17(react@17)` /
    // `react-dom@17(react@18)`, etc.) — every variant shares the
    // same `metadata_key`, so the check only runs once.
    // `InstallabilityOptions` borrows its string fields for exactly
    // this reuse pattern.
    let base_options = InstallabilityOptions {
        engine_strict: host.engine_strict,
        // Cache-shared check: `optional` is applied per-snapshot
        // below, not inside `check_package`.
        optional: false,
        current_node_version: host.node_version.as_str(),
        pnpm_version: None,
        current_os: host.os,
        current_cpu: host.cpu,
        current_libc: host.libc,
        supported_architectures: host.supported_architectures.as_ref(),
    };

    // `None` = compatible. `Some(err)` = incompatible, with the
    // diagnostic the caller would surface (used as both the
    // `SkipOptional` details payload and the `ProceedWithWarning`
    // message body, matching upstream's `warn.toString()` / `warn.message`
    // at `index.ts:50` / `:44`).
    let mut check_cache: HashMap<PackageKey, Option<InstallabilityError>> = HashMap::new();

    for (snapshot_key, snapshot) in snapshots {
        let metadata_key = snapshot_key.without_peer();
        let Some(metadata) = packages.get(&metadata_key) else { continue };

        // Cache miss → run `check_package` once for this metadata
        // row. The clone-on-insert is a single `Option<InstallabilityError>`
        // (small) and only happens on the first peer-variant of each
        // package. Subsequent peer-variants land in the `else` arm
        // and read back the cached verdict.
        let warn = if let Some(cached) = check_cache.get(&metadata_key) {
            cached.clone()
        } else {
            let manifest = manifest_from_metadata(metadata);
            let pkg_id = metadata_key.to_string();
            let result = check_package(&pkg_id, &manifest, &base_options)
                .map_err(|invalid| Box::new(InstallabilityError::InvalidNodeVersion(invalid)))?;
            check_cache.insert(metadata_key.clone(), result.clone());
            result
        };

        let Some(warn) = warn else { continue };

        if snapshot.optional {
            skipped.set.insert(snapshot_key.clone());
            // Dedup events per metadata key, matching upstream's
            // emit-per-pkgId at `index.ts:49-58`.
            if seen_emit.insert(metadata_key.clone()) {
                emit_skipped::<R>(
                    &metadata_key.to_string(),
                    warn.skip_reason(),
                    warn.to_string(),
                    prefix,
                );
            }
            continue;
        }

        if host.engine_strict {
            return Err(Box::new(warn));
        }

        // Non-optional, non-strict: upstream emits `pnpm:install-check`
        // warn (TODO: add channel to the reporter). For now the
        // tracing-level warning is the user-visible signal that an
        // incompatible non-optional dep slipped through.
        tracing::warn!(
            target: "pacquet::install",
            package = %metadata_key,
            "{}",
            warn,
        );
    }

    Ok(skipped)
}

/// True if any package metadata row in the lockfile declares an
/// `engines` / `cpu` / `os` / `libc` constraint pacquet would need
/// to evaluate. Short-circuits on the first hit. When this returns
/// false, both [`compute_skipped_snapshots`] and the caller can
/// short-circuit: no need to spawn `node --version` or build the
/// host context, because the verdict is unconditionally an empty
/// skip set.
///
/// `pub` so `install_frozen_lockfile` can gate the host detection
/// on it — the spawn is otherwise on the critical path of
/// `CreateVirtualStore::run` and serializes ~100ms of node-binary
/// startup with extraction it used to overlap with.
pub fn any_installability_constraint(packages: &HashMap<PackageKey, PackageMetadata>) -> bool {
    packages.values().any(metadata_has_meaningful_constraint)
}

/// True if a single metadata row carries a constraint pacquet would
/// actually evaluate. Distinguishes "field present" from "field present
/// AND meaningful":
///
/// - `engines`: only `node` / `pnpm` keys matter. A package that
///   declares `engines.npm = ">=8"` (and nothing else) has no
///   constraint pacquet evaluates — pacquet isn't npm.
/// - `cpu` / `os` / `libc`: a `["any"]` value short-circuits to
///   "accept" inside `check_platform`'s `check_list`, and an empty
///   list cannot exclude the host either. Treat both as no-constraint.
fn metadata_has_meaningful_constraint(m: &PackageMetadata) -> bool {
    let engines_meaningful =
        m.engines.as_ref().is_some_and(|e| e.contains_key("node") || e.contains_key("pnpm"));
    engines_meaningful
        || platform_axis_meaningful(m.cpu.as_deref())
        || platform_axis_meaningful(m.os.as_deref())
        || platform_axis_meaningful(m.libc.as_deref())
}

/// One axis of `cpu` / `os` / `libc` carries no constraint when the
/// list is absent, empty, or exactly the `["any"]` sentinel that
/// `check_list` short-circuits as "accept everything".
fn platform_axis_meaningful(axis: Option<&[String]>) -> bool {
    match axis {
        None | Some([]) => false,
        Some([only]) if only == "any" => false,
        Some(_) => true,
    }
}

fn manifest_from_metadata(metadata: &PackageMetadata) -> PackageInstallabilityManifest {
    PackageInstallabilityManifest {
        engines: metadata
            .engines
            .as_ref()
            .map(|m| WantedEngine { node: m.get("node").cloned(), pnpm: m.get("pnpm").cloned() }),
        cpu: metadata.cpu.clone(),
        os: metadata.os.clone(),
        libc: metadata.libc.clone(),
    }
}

fn emit_skipped<R: Reporter>(pkg_id: &str, reason: SkipReason, details: String, prefix: &str) {
    let (name, version) = split_name_version(pkg_id);
    let wire_reason = match reason {
        SkipReason::UnsupportedEngine => SkippedOptionalReason::UnsupportedEngine,
        SkipReason::UnsupportedPlatform => SkippedOptionalReason::UnsupportedPlatform,
    };
    R::emit(&LogEvent::SkippedOptionalDependency(SkippedOptionalDependencyLog {
        level: LogLevel::Debug,
        details: Some(details),
        package: SkippedOptionalPackage { id: pkg_id.to_string(), name, version },
        prefix: prefix.to_string(),
        reason: wire_reason,
    }));
}

/// Split a `name@version` (with possible leading `@` for scoped
/// packages) into `(name, version)`. Mirrors the `lastIndexOf('@')`
/// rule pacquet's manifest parser already uses.
fn split_name_version(pkg_id: &str) -> (String, String) {
    match pkg_id.rfind('@') {
        Some(idx) if idx > 0 => (pkg_id[..idx].to_string(), pkg_id[idx + 1..].to_string()),
        _ => (pkg_id.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests;
