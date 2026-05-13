//! Per-install installability pass.
//!
//! For each snapshot in a frozen-lockfile install, run
//! [`pacquet_package_is_installable::package_is_installable`] against
//! the matching `PackageMetadata` and the host environment, build
//! the [`SkippedSnapshots`] set, and emit
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
    InstallabilityError, InstallabilityOptions, InstallabilityVerdict,
    PackageInstallabilityManifest, SkipReason, SupportedArchitectures, WantedEngine,
    package_is_installable,
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
/// 3. Run [`package_is_installable`] with `optional = snapshot.optional`.
/// 4. Outcomes:
///    - `Installable`: nothing to do.
///    - `SkipOptional`: add to the set; emit
///      `pnpm:skipped-optional-dependency`.
///    - `ProceedWithWarning`: emit `tracing::warn!` and proceed.
///      (Upstream uses `pnpm:install-check` here, which pacquet's
///      reporter does not yet expose — slice 1 follow-up.)
///    - `Err`: returned to the caller. Pacquet's default has
///      `engine_strict = false`, so this path is currently
///      unreachable from production — kept for the slice that wires
///      the config setting.
pub fn compute_skipped_snapshots<R: Reporter>(
    snapshots: &HashMap<PackageKey, SnapshotEntry>,
    packages: &HashMap<PackageKey, PackageMetadata>,
    host: &InstallabilityHost,
    prefix: &str,
) -> Result<SkippedSnapshots, Box<InstallabilityError>> {
    let mut skipped = SkippedSnapshots::new();
    let mut seen_emit: HashSet<PackageKey> = HashSet::new();

    // Build the host-derived part of the options once. Only `optional`
    // varies per snapshot, so the loop just toggles that field instead
    // of cloning four Strings per iteration. `InstallabilityOptions`
    // borrows its string fields for exactly this reuse pattern.
    let base_options = InstallabilityOptions {
        engine_strict: host.engine_strict,
        optional: false,
        current_node_version: host.node_version.as_str(),
        pnpm_version: None,
        current_os: host.os,
        current_cpu: host.cpu,
        current_libc: host.libc,
        supported_architectures: host.supported_architectures.as_ref(),
    };

    for (snapshot_key, snapshot) in snapshots {
        let metadata_key = snapshot_key.without_peer();
        let Some(metadata) = packages.get(&metadata_key) else { continue };

        let manifest = manifest_from_metadata(metadata);
        let options = InstallabilityOptions { optional: snapshot.optional, ..base_options };

        let pkg_id = metadata_key.to_string();
        match package_is_installable(&pkg_id, &manifest, &options) {
            Ok(InstallabilityVerdict::Installable) => {}
            Ok(InstallabilityVerdict::SkipOptional { reason, details }) => {
                skipped.set.insert(snapshot_key.clone());
                // Many snapshots map to the same metadata row (one per
                // peer-dependency variant). Dedup so the reporter sees
                // one event per metadata key, matching upstream's
                // emit-per-pkgId at `index.ts:49-58`.
                if seen_emit.insert(metadata_key.clone()) {
                    emit_skipped::<R>(&pkg_id, reason, details, prefix);
                }
            }
            Ok(InstallabilityVerdict::ProceedWithWarning { message }) => {
                // TODO(slice 1 follow-up): add `pnpm:install-check`
                // to the reporter and emit there. For now the
                // tracing-level warning is the user-visible signal
                // that an incompatible non-optional dep slipped
                // through.
                tracing::warn!(
                    target: "pacquet::install",
                    package = %pkg_id,
                    "{message}",
                );
            }
            Err(err) => return Err(err),
        }
    }

    Ok(skipped)
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
