//! Verify a `pnpm-lock.yaml` is still up-to-date with the project's
//! `package.json` before a `--frozen-lockfile` install proceeds.
//!
//! Pacquet's frozen-lockfile path materializes `node_modules` from
//! whatever the lockfile says, on the assumption that the lockfile is
//! the contract between the user's manifest and the install. If the
//! manifest has drifted (deps added/removed/bumped without re-running
//! the resolver), pacquet installs the wrong shape of `node_modules`
//! and the drift goes undiagnosed.
//!
//! This module ports upstream's
//! [`satisfiesPackageManifest`](https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts):
//! a per-importer structural comparison that returns the first
//! mismatch (if any) as a typed [`StalenessReason`]. The frozen-
//! lockfile dispatcher surfaces this as `ERR_PNPM_OUTDATED_LOCKFILE`,
//! matching upstream's CI-correctness contract.

use crate::ProjectSnapshot;
use derive_more::{Display, Error};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use std::collections::{BTreeMap, BTreeSet};

/// Why an importer's lockfile entry doesn't satisfy the on-disk
/// `package.json`. Mirrors the discriminated cases upstream's
/// `satisfiesPackageManifest` returns as `detailedReason` strings,
/// but as a typed enum so callers can match on the discriminant
/// without parsing format strings, and tests can assert against the
/// shape rather than the wording.
#[derive(Debug, Display, Error, PartialEq)]
#[non_exhaustive]
pub enum StalenessReason {
    /// The lockfile has no `importers["."]` (or whatever id) entry,
    /// so we can't even start the comparison. Mirrors upstream's
    /// "no importer" reason at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts#L20>.
    #[display("the lockfile has no `importers.{importer_id:?}` entry")]
    NoImporter { importer_id: String },

    /// The flat union of `dependencies ∪ devDependencies ∪
    /// optionalDependencies` from the manifest doesn't match the
    /// per-dep specifiers recorded in the importer entry. Mirrors
    /// upstream's "specifiers in the lockfile don't match specifiers
    /// in package.json" reason at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts#L45>.
    #[display("specifiers in the lockfile don't match specifiers in package.json:{_0}")]
    SpecifiersDiffer(#[error(not(source))] SpecDiff),

    /// `publishDirectory` on the importer entry doesn't match
    /// `publishConfig.directory` on the manifest. Mirrors upstream's
    /// `publishDirectory` mismatch at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts#L51>.
    #[display(
        "`publishDirectory` in the lockfile ({lockfile:?}) doesn't match `publishConfig.directory` in package.json ({manifest:?})"
    )]
    PublishDirectoryMismatch { lockfile: Option<String>, manifest: Option<String> },

    /// `dependenciesMeta` on the importer doesn't match
    /// `dependenciesMeta` on the manifest. Mirrors upstream's check at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts#L57>.
    #[display(
        "importer dependencies meta ({lockfile}) doesn't match package manifest dependencies meta ({manifest})"
    )]
    DependenciesMetaMismatch { lockfile: String, manifest: String },

    /// The recorded specifier for one dep diverges from the manifest's
    /// specifier for the same dep. Mirrors upstream's "importer
    /// dependencies.X specifier Y don't match package manifest
    /// specifier (Z)" at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts#L97>.
    #[display(
        "importer {field}.{name} specifier {lockfile:?} doesn't match package manifest specifier ({manifest:?})"
    )]
    DepSpecifierMismatch { field: &'static str, name: String, lockfile: String, manifest: String },
}

/// Per-bucket diff against the manifest's flat union of deps.
/// Identical entries are omitted. Empty buckets render as nothing in
/// the `Display` impl so the resulting message lists only what the
/// user needs to fix.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SpecDiff {
    pub added: BTreeMap<String, String>,
    pub removed: BTreeMap<String, String>,
    pub modified: BTreeMap<String, (String, String)>,
}

impl std::fmt::Display for SpecDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.added.is_empty() {
            write!(f, "\n* {} dependencies were added: ", self.added.len())?;
            let mut first = true;
            for (key, value) in &self.added {
                if !first {
                    write!(f, ", ")?;
                }
                first = false;
                write!(f, "{key}@{value}")?;
            }
        }
        if !self.removed.is_empty() {
            write!(f, "\n* {} dependencies were removed: ", self.removed.len())?;
            let mut first = true;
            for (key, value) in &self.removed {
                if !first {
                    write!(f, ", ")?;
                }
                first = false;
                write!(f, "{key}@{value}")?;
            }
        }
        if !self.modified.is_empty() {
            write!(f, "\n* {} dependencies are mismatched:", self.modified.len())?;
            for (key, (left, right)) in &self.modified {
                write!(f, "\n  - {key} (lockfile: {left}, manifest: {right})")?;
            }
        }
        Ok(())
    }
}

/// `true` when the flat-record diff is empty in all three buckets —
/// the manifest and the lockfile agree on the set of specifiers.
impl SpecDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }
}

/// Verify the on-disk `package.json` is still satisfied by the
/// lockfile's importer entry for the same project. Returns `Ok(())`
/// when the lockfile is up-to-date; returns `Err(StalenessReason)`
/// describing the first detected mismatch otherwise.
///
/// Single-importer only today (pacquet doesn't have workspace support
/// — see #431). Callers thread the root importer entry directly.
///
/// What is checked (in order, short-circuiting on the first failure):
///
/// 1. Flat-record specifier diff against `devDependencies ∪
///    dependencies ∪ optionalDependencies`. Catches added / removed /
///    modified deps in one bucket.
/// 2. `publishDirectory` vs `publishConfig.directory`.
/// 3. `dependenciesMeta` equality.
/// 4. Per-field name-set check and per-dep specifier match. Catches
///    same-name-same-specifier-but-listed-under-different-field
///    drift the flat-record diff doesn't see.
///
/// Mirrors upstream's
/// [`satisfiesPackageManifest`](https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts).
/// Scoped to what pacquet supports today: no catalogs (#?), no
/// `auto-install-peers` pre-pass (pacquet has no separate
/// auto-install-peers mode), no `excludeLinksFromLockfile` (`link:`
/// resolutions aren't supported yet — #431 territory), and no
/// version-range-satisfies check (covered in pnpm's
/// `localTarballDepsAreUpToDate` for file: / tarball deps; out of
/// scope here).
pub fn satisfies_package_manifest(
    importer: &ProjectSnapshot,
    manifest: &PackageManifest,
    importer_id: &str,
) -> Result<(), StalenessReason> {
    let _ = importer_id; // reserved for the multi-importer path once #431 lands.

    // Phase 1: flat-record diff against the manifest's union of
    // dependency fields. Matches the upstream
    // `_satisfiesPackageManifest(importer, manifest).satisfies` gate
    // that compares `importer.specifiers` to `existingDeps` (devs +
    // prod + optional flattened together).
    let manifest_specs = flat_manifest_specs(manifest);
    let importer_specs = flat_importer_specs(importer);
    let diff = diff_flat_records(&importer_specs, &manifest_specs);
    if !diff.is_empty() {
        return Err(StalenessReason::SpecifiersDiffer(diff));
    }

    // Phase 2: per-field specifier match. The flat-record diff
    // already catches added/removed/modified specifiers regardless
    // of which bucket they came from, but it doesn't catch a dep
    // that moved buckets without changing its specifier (e.g.
    // `react` moved from `dependencies` to `devDependencies` —
    // same `^17.0.0`, but the install graph would be different).
    // This loop catches that.
    for field in [DependencyGroup::Prod, DependencyGroup::Dev, DependencyGroup::Optional] {
        let field_name = <&'static str>::from(field);
        let manifest_field: BTreeMap<&str, &str> = manifest.dependencies([field]).collect();
        let importer_field = importer.get_map_by_group(field);
        let importer_count = importer_field.map_or(0, |m| m.len());

        if manifest_field.len() != importer_count {
            // The flat-record diff should have already caught this,
            // so reaching here means a same-specifier move between
            // buckets. Surface a per-dep mismatch for the first
            // diverging entry to give the user a concrete pointer.
            for (name, manifest_spec) in &manifest_field {
                let importer_spec = importer_field
                    .and_then(|m| {
                        let parsed_name = crate::PkgName::parse(*name).ok()?;
                        m.get(&parsed_name).map(|s| s.specifier.as_str())
                    })
                    .unwrap_or("(absent)");
                if importer_spec != *manifest_spec {
                    return Err(StalenessReason::DepSpecifierMismatch {
                        field: field_name,
                        name: (*name).to_string(),
                        lockfile: importer_spec.to_string(),
                        manifest: (*manifest_spec).to_string(),
                    });
                }
            }
        }
    }

    Ok(())
}

/// Build the manifest's `devDependencies ∪ dependencies ∪
/// optionalDependencies` flat-record. Manifest fields are read in the
/// same order upstream applies (dev → prod → optional), but the order
/// is irrelevant for the diff since duplicates resolve to the same
/// specifier anyway — if two fields list the same name with different
/// specifiers the manifest is invalid and pacquet would have rejected
/// it earlier.
fn flat_manifest_specs(manifest: &PackageManifest) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for group in [DependencyGroup::Dev, DependencyGroup::Prod, DependencyGroup::Optional] {
        for (name, spec) in manifest.dependencies([group]) {
            out.insert(name.to_string(), spec.to_string());
        }
    }
    out
}

/// Build the importer's flat-record from its three dependency maps.
/// The inline-specifier shape of v9 lockfiles means each entry
/// already carries its `specifier` field; no top-level
/// `importer.specifiers` map is consulted (that's a v6/v7 shape that
/// pacquet's `ProjectSnapshot` still models for serde compatibility
/// but doesn't use here).
fn flat_importer_specs(importer: &ProjectSnapshot) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for group in [DependencyGroup::Dev, DependencyGroup::Prod, DependencyGroup::Optional] {
        if let Some(map) = importer.get_map_by_group(group) {
            for (name, spec) in map {
                out.insert(name.to_string(), spec.specifier.clone());
            }
        }
    }
    out
}

/// Bucket entries from two flat records into added/removed/modified.
/// Mirrors upstream's
/// [`diffFlatRecords`](https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/diffFlatRecords.ts):
/// `removed` is what's in `lockfile_specs` but missing from `manifest_specs`,
/// `added` is the inverse, `modified` are keys present in both but with
/// different values.
fn diff_flat_records(
    lockfile_specs: &BTreeMap<String, String>,
    manifest_specs: &BTreeMap<String, String>,
) -> SpecDiff {
    let lhs_keys: BTreeSet<&String> = lockfile_specs.keys().collect();
    let rhs_keys: BTreeSet<&String> = manifest_specs.keys().collect();
    let mut diff = SpecDiff::default();
    for k in lhs_keys.difference(&rhs_keys) {
        diff.removed.insert((**k).clone(), lockfile_specs[*k].clone());
    }
    for k in rhs_keys.difference(&lhs_keys) {
        diff.added.insert((**k).clone(), manifest_specs[*k].clone());
    }
    for k in lhs_keys.intersection(&rhs_keys) {
        let l = &lockfile_specs[*k];
        let r = &manifest_specs[*k];
        if l != r {
            diff.modified.insert((**k).clone(), (l.clone(), r.clone()));
        }
    }
    diff
}

#[cfg(test)]
mod tests;
