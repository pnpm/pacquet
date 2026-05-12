use crate::PackageManifests;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_cmd_shim::{
    FsCreateDirAll, FsEnsureExecutableBits, FsReadDir, FsReadFile, FsReadHead, FsReadString,
    FsSetExecutable, FsWalkFiles, FsWrite, LinkBinsError, PackageBinSource, RealApi,
    link_bins_of_packages,
};
use pacquet_lockfile::{PackageKey, PackageMetadata, PkgName, SnapshotEntry};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Read the `package.json` of every direct dependency under `modules_dir`
/// and link its bins into `<modules_dir>/.bin`.
///
/// `dep_names` is the list of direct-dependency keys as they appear in
/// `package.json`, the same names already symlinked under `<modules_dir>/`
/// by [`crate::SymlinkDirectDependencies`]. We resolve `package.json` via
/// the symlink (`fs::read` follows it transparently) so the read targets
/// the real package contents in the virtual store.
///
/// Driven on rayon because each location's read+parse is independent.
/// Mirrors pnpm v11's `linkBinsOfPackages` call site for direct deps:
/// <https://github.com/pnpm/pnpm/blob/4750fd370c/installing/deps-installer/src/install/index.ts#L1539>.
pub fn link_direct_dep_bins(modules_dir: &Path, dep_names: &[String]) -> Result<(), LinkBinsError> {
    let direct_dep_locations: Vec<PathBuf> =
        dep_names.iter().map(|name| modules_dir.join(name)).collect();
    // Swallow only `NotFound`: a direct-dep symlink target can
    // legitimately be missing right after a partial pacquet run, or
    // be an in-progress install. Every other IO error (permission
    // denied, EIO, etc.) and every JSON parse error must surface as
    // `LinkBinsError::{ReadManifest, ParseManifest}` so the failure
    // is diagnosable rather than hiding behind a missing `.bin`
    // entry. Matches the read-side error policy in
    // `pacquet_cmd_shim::link_bins`.
    let bin_sources: Vec<PackageBinSource> = direct_dep_locations
        .par_iter()
        .filter_map(|location| {
            let manifest_path = location.join("package.json");
            let bytes = match fs::read(&manifest_path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
                Err(error) => {
                    return Some(Err(LinkBinsError::ReadManifest { path: manifest_path, error }));
                }
            };
            let manifest: serde_json::Value = match serde_json::from_slice(&bytes) {
                Ok(manifest) => manifest,
                Err(error) => {
                    return Some(Err(LinkBinsError::ParseManifest { path: manifest_path, error }));
                }
            };
            Some(Ok(PackageBinSource { location: location.clone(), manifest: Arc::new(manifest) }))
        })
        .collect::<Result<_, _>>()?;
    if bin_sources.is_empty() {
        return Ok(());
    }
    link_bins_of_packages::<RealApi>(&bin_sources, &modules_dir.join(".bin"))
}

/// Error type of [`LinkVirtualStoreBins`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum LinkVirtualStoreBinsError {
    #[display("Failed to read virtual store directory at {dir:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::read_virtual_store))]
    ReadVirtualStore {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[diagnostic(transparent)]
    LinkBins(#[error(source)] LinkBinsError),
}

/// For every package slot under `<virtual_store_dir>/<pkg>@<ver>/node_modules`,
/// link the bins of that slot's child packages into the slot's *own*
/// `node_modules/.bin` directory.
///
/// This mirrors `linkBinsOfDependencies` in pnpm's `building/during-install`
/// (see <https://github.com/pnpm/pnpm/blob/4750fd370c/building/during-install/src/index.ts#L258-L309>).
/// pnpm walks each `depNode`, takes its `children` (its direct deps in the
/// resolved graph) and writes their bins into
/// `<depNode.dir>/node_modules/.bin`.
///
/// Pacquet's virtual store layout already exposes a slot's children as
/// siblings via [`create_symlink_layout`](crate::create_symlink_layout()).
/// So once the symlinks exist, walking
/// the slot's `node_modules` and excluding the package itself gives the same
/// child-set pnpm uses, and the bins go into the package's own
/// `node_modules/.bin` (i.e. nested *one level deeper* than the slot's
/// `node_modules` directory).
///
/// Path layout produced for a slot `A@1.0.0`:
///
/// ```text
/// <virtual>/A@1.0.0/node_modules/A/node_modules/.bin/<bin>
/// ```
///
/// When `snapshots` is `Some` (the frozen-lockfile case), the slot
/// set is taken from the lockfile and each child's manifest is
/// looked up in `package_manifests` rather than read off disk —
/// matching pnpm's `linkBinsOfDependencies` which consumes
/// `bundledManifest` straight out of the SQLite store index (see
/// <https://github.com/pnpm/pnpm/blob/4750fd370c/building/during-install/src/index.ts#L289>).
/// When `snapshots` is `None` (install without a lockfile), the
/// linker falls back to enumerating slots and reading manifests via
/// the filesystem, the shape this code had before the
/// lockfile-driven path landed.
#[must_use]
pub struct LinkVirtualStoreBins<'a> {
    pub virtual_store_dir: &'a Path,
    /// `Some` when the install is lockfile-driven. Iterating the
    /// snapshot map (instead of `read_dir(virtual_store_dir)`)
    /// removes the per-slot directory enumeration and lets us walk
    /// each slot's children from its `dependencies` /
    /// `optionalDependencies` lists without touching the filesystem.
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    /// Lockfile `packages:` section, indexed by `PkgNameVerPeer`
    /// (without peer suffix). Used to filter children by
    /// `hasBin == true` *before* any per-child IO — mirrors pnpm's
    /// `dep.hasBin` filter in
    /// [`linkBinsOfDependencies`](https://github.com/pnpm/pnpm/blob/4750fd370c/building/during-install/src/index.ts#L283).
    /// Most packages don't declare a bin, so this short-circuits the
    /// bulk of the per-slot work before any path-building or manifest
    /// lookup happens.
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    /// Bundled manifests recovered from the warm-cache prefetch of
    /// `index.db` ([`crate::PackageManifests`]). A hit lets the
    /// linker skip the `package.json` read for that child entirely;
    /// a miss falls back to a disk read so cold-batch packages
    /// installed earlier in the same run still get their bins
    /// linked.
    pub package_manifests: &'a PackageManifests,
}

impl<'a> LinkVirtualStoreBins<'a> {
    pub fn run(self) -> Result<(), LinkVirtualStoreBinsError> {
        self.run_with::<RealApi>()
    }

    /// DI-driven entry. Production callers go through [`Self::run`] which
    /// turbofishes [`RealApi`]; tests inject fakes that fail specific fs
    /// operations to cover error paths the real fs can't trigger
    /// portably. See the per-capability DI pattern at
    /// <https://github.com/pnpm/pacquet/pull/332#issuecomment-4345054524>.
    pub fn run_with<Api>(self) -> Result<(), LinkVirtualStoreBinsError>
    where
        Api: FsReadDir
            + FsReadFile
            + FsReadString
            + FsReadHead
            + FsCreateDirAll
            + FsWalkFiles
            + FsWrite
            + FsSetExecutable
            + FsEnsureExecutableBits,
    {
        let LinkVirtualStoreBins { virtual_store_dir, snapshots, packages, package_manifests } =
            self;
        if let Some(snapshots) = snapshots {
            let has_bin_set = build_has_bin_set(packages);
            run_lockfile_driven::<Api>(
                virtual_store_dir,
                snapshots,
                has_bin_set.as_ref(),
                package_manifests,
            )
        } else {
            run_with_readdir::<Api>(virtual_store_dir)
        }
    }
}

/// Pre-compute the set of package keys whose lockfile metadata sets
/// `hasBin: true`. Mirrors pnpm's filter at
/// [`during-install/src/index.ts:283`](https://github.com/pnpm/pnpm/blob/4750fd370c/building/during-install/src/index.ts#L283):
/// most packages don't declare a bin, so short-circuiting the
/// per-child manifest lookup with this set is the cheapest win on
/// warm-cache installs.
///
/// Return-value semantics distinguish "lockfile metadata absent"
/// from "lockfile metadata says no package has a bin":
///
/// - `None` — the lockfile's `packages:` section wasn't supplied
///   (pathological lockfile shape). We have no info, so the bin
///   linker falls back to the conservative "process every child"
///   path and lets the per-package bin resolver sort it out.
/// - `Some(set)` — the section was present and we used it. The
///   `set` contains only entries with `hasBin == Some(true)`; an
///   *empty* `Some(set)` is authoritative: the lockfile says no
///   package has a bin, and every slot should short-circuit
///   immediately. Conflating this case with `None` (the bug Copilot
///   flagged at <https://github.com/pnpm/pacquet/pull/333#discussion_r3222807548>)
///   would force per-child work the lockfile already ruled out.
fn build_has_bin_set(
    packages: Option<&HashMap<PackageKey, PackageMetadata>>,
) -> Option<HashSet<PackageKey>> {
    let packages = packages?;
    Some(
        packages
            .iter()
            .filter(|(_, meta)| meta.has_bin == Some(true))
            .map(|(key, _)| key.clone())
            .collect(),
    )
}

/// Walk the lockfile's `snapshots:` map, build each slot's bin output
/// directory lexically, and link every child's bins into it. The
/// child set comes from `snapshot.dependencies` +
/// `snapshot.optional_dependencies`, filtered by `has_bin_set` so
/// packages that don't declare a bin never make it into the
/// per-slot path-building or manifest-lookup work. The corresponding
/// manifest comes from [`PackageManifests`] (no disk read) or, for
/// cold-batch packages that prefetch missed, a fallback
/// `package.json` read through the existing symlink at
/// `<slot>/node_modules/<alias>`.
fn run_lockfile_driven<Api>(
    virtual_store_dir: &Path,
    snapshots: &HashMap<PackageKey, SnapshotEntry>,
    has_bin_set: Option<&HashSet<PackageKey>>,
    package_manifests: &PackageManifests,
) -> Result<(), LinkVirtualStoreBinsError>
where
    Api: FsReadFile
        + FsReadString
        + FsReadHead
        + FsCreateDirAll
        + FsWalkFiles
        + FsWrite
        + FsSetExecutable
        + FsEnsureExecutableBits,
{
    // `has_bin_set` is `Some` exactly when the lockfile's `packages:`
    // section was present at install start — in which case the set
    // is authoritative and every slot is filtered through it (an
    // empty `Some(set)` means "no package declares a bin", which
    // short-circuits every slot below). When the section was
    // missing we have no info and fall through to processing every
    // child. See [`build_has_bin_set`] for the rationale.
    // Materialise as a `Vec` so rayon can split the work; iterating
    // a `HashMap` directly with `par_iter` would require collecting
    // anyway, and explicit collection here keeps the parallelism
    // contract obvious.
    let slot_entries: Vec<(&PackageKey, &SnapshotEntry)> = snapshots.iter().collect();
    slot_entries.par_iter().try_for_each(|(slot_key, snapshot)| {
        let children = snapshot
            .dependencies
            .iter()
            .flatten()
            .chain(snapshot.optional_dependencies.iter().flatten());

        // First pass: figure out which children (if any) have a bin
        // declared. Cheap — just hash-set lookups against the
        // pre-built `has_bin_set` and a `without_peer` materialisation
        // per child. If no child has a bin, skip the slot entirely —
        // we don't even build the slot's path. Slots in this category
        // are the bulk of a real lockfile (~95% in the integrated
        // benchmark fixture); skipping them removes the dominant
        // chunk of the per-install bin-link work.
        let with_bin: Vec<(&PkgName, PackageKey)> = children
            .filter_map(|(alias, dep_ref)| {
                let child_key = dep_ref.resolve(alias);
                let metadata_key = child_key.without_peer();
                let keep = match has_bin_set {
                    Some(set) => set.contains(&metadata_key),
                    None => true,
                };
                keep.then_some((alias, metadata_key))
            })
            .collect();
        if with_bin.is_empty() {
            return Ok(());
        }

        let slot_dir = virtual_store_dir.join(slot_key.to_virtual_store_name());
        let modules_dir = slot_dir.join("node_modules");
        let self_pkg_dir = slot_own_pkg_dir(&modules_dir, slot_key);
        let bins_dir = self_pkg_dir.join("node_modules/.bin");

        let mut bin_sources: Vec<PackageBinSource> = Vec::with_capacity(with_bin.len());
        for (alias, metadata_key) in with_bin {
            let child_location = pkg_dir_under(&modules_dir, alias);
            if let Some(manifest) = package_manifests.get(&metadata_key) {
                // Hot path: parsed manifest already in memory from
                // the warm-cache prefetch. Both the prefetch map
                // and `PackageBinSource` hold the manifest via
                // [`Arc`], so this is a refcount bump rather than a
                // deep clone of the JSON tree. Avoids the
                // `slots × children`-sized clone fan-out that
                // dominated the previous version of this path on
                // warm-cache installs.
                bin_sources.push(PackageBinSource {
                    location: child_location,
                    manifest: Arc::clone(manifest),
                });
            } else {
                // Cold-batch fallback: package was downloaded
                // earlier in the run, so its row isn't in the
                // prefetched manifest map yet. Reading from disk
                // here is the same code path as the non-lockfile
                // install — see [`run_with_readdir`].
                match read_package::<Api>(&child_location) {
                    Ok(Some(pkg)) => bin_sources.push(pkg),
                    Ok(None) => {}
                    Err(error) => return Err(LinkVirtualStoreBinsError::LinkBins(error)),
                }
            }
        }

        if bin_sources.is_empty() {
            return Ok(());
        }
        link_bins_of_packages::<Api>(&bin_sources, &bins_dir)
            .map_err(LinkVirtualStoreBinsError::LinkBins)
    })
}

/// Compute `<slot>/node_modules/<pkg-or-@scope/pkg>` for the slot's
/// own package. The slot's package name lives on the lockfile key,
/// so no filesystem probing is needed (the directory is an invariant
/// maintained by [`crate::create_virtual_dir_by_snapshot`]). Scoped
/// names land at `<modules>/@scope/<name>`, unscoped names at
/// `<modules>/<name>`.
fn slot_own_pkg_dir(modules_dir: &Path, slot_key: &PackageKey) -> PathBuf {
    pkg_dir_under(modules_dir, &slot_key.name)
}

/// Join a package name onto a `node_modules` directory, handling the
/// `@scope/name` split into two path components. Operates on the raw
/// [`PkgName`] (whose `scope` and `bare` fields are already split),
/// not on the virtual-store-name form — for instance the input
/// represents `@types/node`, **not** `@types+node`.
fn pkg_dir_under(modules_dir: &Path, name: &PkgName) -> PathBuf {
    match &name.scope {
        Some(scope) => modules_dir.join(format!("@{scope}")).join(&name.bare),
        None => modules_dir.join(&name.bare),
    }
}

/// Fallback (non-lockfile) path: enumerate slots via `read_dir`,
/// then walk each slot's `node_modules` to discover children. Used
/// only by [`crate::InstallWithoutLockfile`] today; the lockfile
/// path bypasses every directory enumeration in here.
fn run_with_readdir<Api>(virtual_store_dir: &Path) -> Result<(), LinkVirtualStoreBinsError>
where
    Api: FsReadDir
        + FsReadFile
        + FsReadString
        + FsReadHead
        + FsCreateDirAll
        + FsWalkFiles
        + FsWrite
        + FsSetExecutable
        + FsEnsureExecutableBits,
{
    let slots = match Api::read_dir(virtual_store_dir) {
        Ok(slots) => slots,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LinkVirtualStoreBinsError::ReadVirtualStore {
                dir: virtual_store_dir.to_path_buf(),
                error,
            });
        }
    };
    let slots: Vec<PathBuf> = slots.collect();
    slots.par_iter().try_for_each(|slot_dir| {
        let modules_dir = slot_dir.join("node_modules");
        let Some(self_pkg_dir) = find_slot_own_package_dir(slot_dir, &modules_dir) else {
            return Ok(());
        };
        // Probe the slot's own package directory before walking its
        // children. Without the probe, an incomplete slot whose
        // `node_modules/<pkg>` is missing but whose sibling deps are
        // still present would have `link_bins_excluding` collect the
        // siblings and `create_dir_all` the missing `<pkg>` chain to
        // hold the shims, leaving an orphan package directory on
        // disk. This path runs only for [`crate::InstallWithoutLockfile`]
        // and visits ~direct-deps slots (small N), so the probe cost
        // is trivial; the lockfile-driven path bypasses this by
        // treating the slot's own pkg dir as an invariant of
        // [`crate::create_virtual_dir_by_snapshot`].
        if Api::read_dir(&self_pkg_dir).is_err() {
            return Ok(());
        }
        let bins_dir = self_pkg_dir.join("node_modules/.bin");
        link_bins_excluding::<Api>(&modules_dir, &bins_dir, &self_pkg_dir)
            .map_err(LinkVirtualStoreBinsError::LinkBins)
    })
}

/// Locate the slot's own package directory inside `<slot>/node_modules`.
///
/// The slot directory's name encodes the package name as
/// `<scope>+<name>@<version>` for the simple case (see
/// [`pacquet_lockfile::PkgNameVerPeer::to_virtual_store_name`]). For
/// peer-resolved slots the version segment itself contains additional
/// `@`-separated peer specs joined by `_`, e.g.
/// `ts-node@10.9.1_@types+node@18.7.19_typescript@5.1.6`. The `@` after
/// `typescript` is part of a peer's version, not the package-name
/// boundary. Parsing from the right (`rfind('@')`) would split there
/// and silently break peer-resolved slots; parse from the left
/// instead, skipping a leading `@` that belongs to a scoped package.
///
/// Returns `None` only when the slot name fails to parse — there's no
/// filesystem probe for the resolved candidate. The previous version
/// stat-equivalent-ed the path with `Api::read_dir` to short-circuit
/// missing slots, but on a 1267-package fixture that was 1267
/// wasted `open(O_DIRECTORY) + close` round-trips on the hot path of
/// every warm install. The slot's own package directory is an
/// invariant of [`crate::create_virtual_dir_by_snapshot`]; the
/// downstream `link_bins_excluding` handles `NotFound` from its own
/// `read_dir` of `<slot>/node_modules` cleanly when the invariant
/// ever does break, so the probe is pure overhead.
fn find_slot_own_package_dir(slot_dir: &Path, modules_dir: &Path) -> Option<PathBuf> {
    let slot_name = slot_dir.file_name()?.to_str()?;

    // The package-name half is everything before the **first** `@`,
    // ignoring a single leading `@` that belongs to a scoped name
    // (`@scope+pkg@...` → start the `@` search at offset 1).
    // After `to_virtual_store_name`, `/` in scoped names becomes `+`,
    // so the package-name half can never contain `@` itself.
    let scoped = slot_name.starts_with('@');
    let search_start = usize::from(scoped);
    let at = search_start + slot_name[search_start..].find('@')?;
    let name_part = &slot_name[..at];

    // `+` separates `<scope>+<name>` for scoped packages, and *only*
    // for scoped packages. Gating on `scoped` avoids misparsing a
    // hypothetical unscoped name that contains `+`: `PkgName::parse`
    // does not reject non-URL-safe characters (only npm's
    // `validate-npm-package-name` warns about them), so an unscoped
    // name like `foo+bar` could in principle reach here and would
    // otherwise be split into `foo` / `bar`.
    let pkg_dir = match scoped.then(|| name_part.split_once('+')).flatten() {
        Some((scope, name)) => modules_dir.join(scope).join(name),
        None => modules_dir.join(name_part),
    };
    Some(pkg_dir)
}

/// Like [`pacquet_cmd_shim::link_bins`] but skipping the slot's own package
/// from the candidate set. Without this, a slot for `tsc@5.0.0` would link
/// its own `tsc` bin into its own `node_modules/.bin`, which pnpm doesn't.
fn link_bins_excluding<Api>(
    modules_dir: &Path,
    bins_dir: &Path,
    exclude: &Path,
) -> Result<(), LinkBinsError>
where
    Api: FsReadDir
        + FsReadFile
        + FsReadString
        + FsReadHead
        + FsCreateDirAll
        + FsWalkFiles
        + FsWrite
        + FsSetExecutable
        + FsEnsureExecutableBits,
{
    let mut packages: Vec<PackageBinSource> = Vec::new();

    let entries = match Api::read_dir(modules_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LinkBinsError::ReadModulesDir { dir: modules_dir.to_path_buf(), error });
        }
    };

    for path in entries {
        let Some(name) = path.file_name() else {
            continue;
        };
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }

        if name_str.starts_with('@') {
            // Only `NotFound` is plausibly skippable here (a
            // concurrent scope-dir delete). Other errors —
            // permission denied, EIO, AppArmor deny — would mean
            // the bins for every package under this scope silently
            // disappear, so surface them instead of letting them
            // hide.
            let scope_entries = match Api::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LinkBinsError::ReadModulesDir { dir: path.clone(), error });
                }
            };
            for sub_path in scope_entries {
                if paths_eq(&sub_path, exclude) {
                    continue;
                }
                if let Some(pkg) = read_package::<Api>(&sub_path)? {
                    packages.push(pkg);
                }
            }
            continue;
        }

        if paths_eq(&path, exclude) {
            continue;
        }
        if let Some(pkg) = read_package::<Api>(&path)? {
            packages.push(pkg);
        }
    }

    if packages.is_empty() {
        return Ok(());
    }

    link_bins_of_packages::<Api>(&packages, bins_dir)
}

fn read_package<Api: FsReadFile>(
    location: &Path,
) -> Result<Option<PackageBinSource>, LinkBinsError> {
    let manifest_path = location.join("package.json");
    let bytes = match Api::read_file(&manifest_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(LinkBinsError::ReadManifest { path: manifest_path, error }),
    };
    let manifest: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| LinkBinsError::ParseManifest { path: manifest_path, error })?;
    Ok(Some(PackageBinSource { location: location.to_path_buf(), manifest: Arc::new(manifest) }))
}

fn paths_eq(a: &Path, b: &Path) -> bool {
    // Lexical comparison is enough; both paths come from the same
    // `node_modules` walk and don't go through canonicalisation.
    a == b
}

#[cfg(test)]
mod tests;
