use crate::PackageManifests;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_cmd_shim::{
    FsCreateDirAll, FsEnsureExecutableBits, FsReadDir, FsReadFile, FsReadHead, FsReadString,
    FsSetExecutable, FsWalkFiles, FsWrite, LinkBinsError, PackageBinSource, RealApi,
    link_bins_of_packages,
};
use pacquet_lockfile::{PackageKey, PkgName, SnapshotEntry};
use rayon::prelude::*;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
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
            let manifest = match serde_json::from_slice(&bytes) {
                Ok(manifest) => manifest,
                Err(error) => {
                    return Some(Err(LinkBinsError::ParseManifest { path: manifest_path, error }));
                }
            };
            Some(Ok(PackageBinSource { location: location.clone(), manifest }))
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
        let LinkVirtualStoreBins { virtual_store_dir, snapshots, package_manifests } = self;
        if let Some(snapshots) = snapshots {
            run_lockfile_driven::<Api>(virtual_store_dir, snapshots, package_manifests)
        } else {
            run_with_readdir::<Api>(virtual_store_dir)
        }
    }
}

/// Walk the lockfile's `snapshots:` map, build each slot's bin output
/// directory lexically, and link every child's bins into it. The
/// child set comes from `snapshot.dependencies` +
/// `snapshot.optional_dependencies`; the corresponding manifest
/// comes from [`PackageManifests`] (no disk read) or, for cold-batch
/// packages that prefetch missed, a fallback `package.json` read
/// through the existing symlink at `<slot>/node_modules/<alias>`.
fn run_lockfile_driven<Api>(
    virtual_store_dir: &Path,
    snapshots: &HashMap<PackageKey, SnapshotEntry>,
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
    // Materialise as a `Vec` so rayon can split the work; iterating
    // a `HashMap` directly with `par_iter` would require collecting
    // anyway, and explicit collection here keeps the parallelism
    // contract obvious.
    let slot_entries: Vec<(&PackageKey, &SnapshotEntry)> = snapshots.iter().collect();
    slot_entries.par_iter().try_for_each(|(slot_key, snapshot)| {
        let slot_dir = virtual_store_dir.join(slot_key.to_virtual_store_name());
        let modules_dir = slot_dir.join("node_modules");
        let self_pkg_dir = slot_own_pkg_dir(&modules_dir, slot_key);
        let bins_dir = self_pkg_dir.join("node_modules/.bin");

        let children = snapshot
            .dependencies
            .iter()
            .flatten()
            .chain(snapshot.optional_dependencies.iter().flatten());

        let mut bin_sources: Vec<PackageBinSource> = Vec::new();
        for (alias, dep_ref) in children {
            let child_key = dep_ref.resolve(alias);
            let child_location = pkg_dir_under(&modules_dir, alias);
            let metadata_key = child_key.without_peer();
            if let Some(manifest) = package_manifests.get(&metadata_key) {
                // Hot path: parsed manifest already in memory from
                // the warm-cache prefetch. The clone is a recursive
                // copy of the BundledManifest subset (small — `bin`,
                // `directories`, `name`, `version`, …), and only
                // fires for children whose package is present in the
                // prefetch.
                bin_sources.push(PackageBinSource {
                    location: child_location,
                    manifest: Value::clone(manifest),
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
        let Some(self_pkg_dir) = find_slot_own_package_dir::<Api>(slot_dir, &modules_dir) else {
            return Ok(());
        };
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
/// Existence of the resolved candidate is probed via `Api::read_dir`
/// rather than `Path::is_dir`, so tests injecting a fake `Api` see the
/// fake's view of the world. A path that is not a directory (or that
/// does not exist) yields `Err` from the trait, which we map to
/// `None`.
fn find_slot_own_package_dir<Api: FsReadDir>(
    slot_dir: &Path,
    modules_dir: &Path,
) -> Option<PathBuf> {
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
    // `Api::read_dir` returns `impl Iterator<Item = PathBuf>` that may
    // borrow from `&pkg_dir`. Drop the iterator before moving
    // `pkg_dir` out; turn the `Ok`/`Err` into a `bool` first so the
    // borrow ends with the temporary.
    let exists = Api::read_dir(&pkg_dir).is_ok();
    exists.then_some(pkg_dir)
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
            let Ok(scope_entries) = Api::read_dir(&path) else {
                continue;
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
    Ok(Some(PackageBinSource { location: location.to_path_buf(), manifest }))
}

fn paths_eq(a: &Path, b: &Path) -> bool {
    // Lexical comparison is enough; both paths come from the same
    // `node_modules` walk and don't go through canonicalisation.
    a == b
}

#[cfg(test)]
mod tests;
