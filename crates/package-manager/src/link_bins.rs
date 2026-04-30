use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_cmd_shim::{
    FsCreateDirAll, FsReadDir, FsReadFile, FsReadHead, FsReadString, FsSetPermissions, FsWrite,
    LinkBinsError, PackageBinSource, RealApi, link_bins_of_packages,
};
use rayon::prelude::*;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Read the `package.json` of every direct dependency under `modules_dir`
/// and link its bins into `<modules_dir>/.bin`.
///
/// `dep_names` is the list of direct-dependency keys as they appear in
/// `package.json` — the same names already symlinked under `<modules_dir>/`
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
    let bin_sources: Vec<PackageBinSource> = direct_dep_locations
        .par_iter()
        .filter_map(|location| {
            let manifest_path = location.join("package.json");
            let bytes = fs::read(&manifest_path).ok()?;
            let manifest = serde_json::from_slice(&bytes).ok()?;
            Some(PackageBinSource { location: location.clone(), manifest })
        })
        .collect();
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
/// siblings via `create_symlink_layout`. So once the symlinks exist, walking
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
#[must_use]
pub struct LinkVirtualStoreBins<'a> {
    pub virtual_store_dir: &'a Path,
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
            + FsWrite
            + FsSetPermissions,
    {
        let LinkVirtualStoreBins { virtual_store_dir } = self;

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

        // Per-slot work is independent: each writes shims under the slot's
        // own `<pkg>/node_modules/.bin` directory and reads only the slot's
        // own children. With ~1300 slots in a real lockfile (the integrated
        // benchmark fixture), the serial loop was the dominant chunk of
        // the bin-linking pass — driving it on rayon brings that cost down
        // to roughly `total_work / num_cpus`.
        slots.par_iter().try_for_each(|slot_dir| {
            let modules_dir = slot_dir.join("node_modules");
            if !modules_dir.is_dir() {
                return Ok(());
            }

            // Identify the slot's own package by walking `node_modules` and
            // recovering the directory that matches the slot name. Since
            // pacquet's virtual store always stores the slot's own package
            // at `<slot>/node_modules/<pkg>` (see
            // `create_virtual_dir_by_snapshot.rs`), the bin output dir is
            // `<slot>/node_modules/<pkg>/node_modules/.bin`. There's
            // exactly one such candidate per slot — the others are
            // `node_modules/<dep>` symlinks pointing at sibling slots.
            let Some(self_pkg_dir) = find_slot_own_package_dir(slot_dir, &modules_dir) else {
                return Ok(());
            };
            let bins_dir = self_pkg_dir.join("node_modules/.bin");

            // Children of this slot are everything under `node_modules`
            // *other than* the slot's own package. `link_bins` already
            // skips dot-prefixed entries (`.bin`, `.modules.yaml`, …).
            link_bins_excluding::<Api>(&modules_dir, &bins_dir, &self_pkg_dir)
                .map_err(LinkVirtualStoreBinsError::LinkBins)
        })
    }
}

/// Locate the slot's own package directory inside `<slot>/node_modules`.
///
/// The slot directory's name encodes the package name as
/// `<scope>+<name>@<version>` for the simple case (see
/// [`pacquet_lockfile::PkgNameVerPeer::to_virtual_store_name`]). For
/// peer-resolved slots the version segment itself contains additional
/// `@`-separated peer specs joined by `_`, e.g.
/// `ts-node@10.9.1_@types+node@18.7.19_typescript@5.1.6` — the `@` after
/// `typescript` is part of a peer's version, not the package-name
/// boundary. Parsing from the right (`rfind('@')`) would split there
/// and silently break peer-resolved slots; parse from the left
/// instead, skipping a leading `@` that belongs to a scoped package.
fn find_slot_own_package_dir(slot_dir: &Path, modules_dir: &Path) -> Option<PathBuf> {
    let slot_name = slot_dir.file_name()?.to_str()?;

    // The package-name half is everything before the **first** `@`,
    // ignoring a single leading `@` that belongs to a scoped name
    // (`@scope+pkg@...` → start the `@` search at offset 1).
    // After `to_virtual_store_name`, `/` in scoped names becomes `+`,
    // so the package-name half can never contain `@` itself.
    let search_start = if slot_name.starts_with('@') { 1 } else { 0 };
    let at = search_start + slot_name[search_start..].find('@')?;
    let name_part = &slot_name[..at];

    // `+` separates `<scope>+<name>` for scoped packages; non-scoped
    // names contain no `+`.
    let pkg_dir = match name_part.split_once('+') {
        Some((scope, name)) => modules_dir.join(scope).join(name),
        None => modules_dir.join(name_part),
    };
    pkg_dir.is_dir().then_some(pkg_dir)
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
        + FsWrite
        + FsSetPermissions,
{
    let mut packages: Vec<PackageBinSource> = Vec::new();

    let entries = match Api::read_dir(modules_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LinkBinsError::CreateBinDir { dir: modules_dir.to_path_buf(), error });
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
    // Lexical comparison is enough — both paths come from the same
    // `node_modules` walk and don't go through canonicalisation.
    a == b
}

#[cfg(test)]
mod tests;
