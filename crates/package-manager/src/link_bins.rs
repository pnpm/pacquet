use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_cmd_shim::{LinkBinsError, PackageBinSource, link_bins_of_packages};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

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
        let LinkVirtualStoreBins { virtual_store_dir } = self;

        let entries = match fs::read_dir(virtual_store_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(LinkVirtualStoreBinsError::ReadVirtualStore {
                    dir: virtual_store_dir.to_path_buf(),
                    error,
                });
            }
        };

        for entry in entries.flatten() {
            let slot_dir = entry.path();
            let modules_dir = slot_dir.join("node_modules");
            if !modules_dir.is_dir() {
                continue;
            }

            // Identify the slot's own package by walking `node_modules` and
            // recovering the directory that matches the slot name. Since
            // pacquet's virtual store always stores the slot's own package
            // at `<slot>/node_modules/<pkg>` (see
            // `create_virtual_dir_by_snapshot.rs`), the bin output dir is
            // `<slot>/node_modules/<pkg>/node_modules/.bin`. There's
            // exactly one such candidate per slot — the others are
            // `node_modules/<dep>` symlinks pointing at sibling slots.
            let Some(self_pkg_dir) = find_slot_own_package_dir(&slot_dir, &modules_dir) else {
                continue;
            };
            let bins_dir = self_pkg_dir.join("node_modules/.bin");

            // Children of this slot are everything under `node_modules`
            // *other than* the slot's own package. `link_bins` already
            // skips dot-prefixed entries (`.bin`, `.modules.yaml`, …).
            link_bins_excluding(&modules_dir, &bins_dir, &self_pkg_dir)
                .map_err(LinkVirtualStoreBinsError::LinkBins)?;
        }

        Ok(())
    }
}

/// Locate the slot's own package directory inside `<slot>/node_modules`.
///
/// The slot directory's name encodes the package name as
/// `<scope>+<name>@<version>` (see [`pacquet_lockfile::PkgNameVerPeer::to_virtual_store_name`]).
/// The own-package directory is the one whose location is a real directory
/// (not a symlink) and whose path matches the slot name decoded back to
/// `<scope>/<name>` form.
fn find_slot_own_package_dir(slot_dir: &Path, modules_dir: &Path) -> Option<PathBuf> {
    let slot_name = slot_dir.file_name()?.to_str()?;
    // Strip the `@<version>` tail.
    let at = slot_name.rfind('@')?;
    let name_part = &slot_name[..at];
    // `+` separates `<scope>+<name>` for scoped packages; non-scoped names
    // contain no `+`.
    let pkg_dir = match name_part.split_once('+') {
        Some((scope, name)) => modules_dir.join(scope).join(name),
        None => modules_dir.join(name_part),
    };
    pkg_dir.is_dir().then_some(pkg_dir)
}

/// Like [`pacquet_cmd_shim::link_bins`] but skipping the slot's own package
/// from the candidate set. Without this, a slot for `tsc@5.0.0` would link
/// its own `tsc` bin into its own `node_modules/.bin`, which pnpm doesn't.
fn link_bins_excluding(
    modules_dir: &Path,
    bins_dir: &Path,
    exclude: &Path,
) -> Result<(), LinkBinsError> {
    let mut packages: Vec<PackageBinSource> = Vec::new();

    let entries = match fs::read_dir(modules_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LinkBinsError::CreateBinDir { dir: modules_dir.to_path_buf(), error });
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();

        if name_str.starts_with('@') {
            let Ok(scope_entries) = fs::read_dir(&path) else {
                continue;
            };
            for sub in scope_entries.flatten() {
                let sub_path = sub.path();
                if paths_eq(&sub_path, exclude) {
                    continue;
                }
                if let Some(pkg) = read_package(&sub_path)? {
                    packages.push(pkg);
                }
            }
            continue;
        }

        if paths_eq(&path, exclude) {
            continue;
        }
        if let Some(pkg) = read_package(&path)? {
            packages.push(pkg);
        }
    }

    if packages.is_empty() {
        return Ok(());
    }

    link_bins_of_packages(&packages, bins_dir)
}

fn read_package(location: &Path) -> Result<Option<PackageBinSource>, LinkBinsError> {
    let manifest_path = location.join("package.json");
    let bytes = match fs::read(&manifest_path) {
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
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    /// End-to-end exercise of [`LinkVirtualStoreBins`] against a hand-built
    /// virtual store. Slot `parent@1.0.0` has one child `child` declaring a
    /// bin; after the run, the child's shim must land at
    /// `parent@1.0.0/node_modules/parent/node_modules/.bin/child` and *not*
    /// at the slot's own `node_modules/.bin` (which is what would happen if
    /// we accidentally pointed at the wrong directory).
    #[test]
    fn writes_child_bins_into_slot_own_package_node_modules() {
        let tmp = tempdir().unwrap();
        let virtual_dir = tmp.path().join(".pacquet");

        // The slot for `parent@1.0.0`. pnpm uses `+` for scope separator.
        let slot = virtual_dir.join("parent@1.0.0");
        let modules = slot.join("node_modules");
        let parent_dir = modules.join("parent");
        let child_dir = modules.join("child");
        fs::create_dir_all(&parent_dir).unwrap();
        fs::create_dir_all(&child_dir).unwrap();

        fs::write(
            parent_dir.join("package.json"),
            json!({"name": "parent", "version": "1.0.0"}).to_string(),
        )
        .unwrap();
        fs::write(
            child_dir.join("package.json"),
            json!({"name": "child", "version": "1.0.0", "bin": "cli.js"}).to_string(),
        )
        .unwrap();
        fs::write(child_dir.join("cli.js"), "#!/usr/bin/env node\n").unwrap();

        LinkVirtualStoreBins { virtual_store_dir: &virtual_dir }.run().unwrap();

        let shim_path = parent_dir.join("node_modules/.bin/child");
        assert!(shim_path.exists(), "expected shim at {shim_path:?}");
        let body = fs::read_to_string(&shim_path).unwrap();
        // Layout, with shim at A and target at B, relative path `A → B`:
        //
        //   <slot>/node_modules/parent/node_modules/.bin/child   (shim, A)
        //   <slot>/node_modules/child/cli.js                     (target, B)
        //
        // Common prefix is `<slot>/node_modules`. A has three extra
        // segments after that (`parent`, `node_modules`, `.bin`); B has
        // two (`child`, `cli.js`). Relative = `../../../child/cli.js`.
        assert!(
            body.contains("\"$basedir/../../../child/cli.js\""),
            "shim must reference the sibling child via the right number of `..`s, got:\n{body}",
        );
    }

    /// A slot whose own package also declares a bin must NOT have that bin
    /// linked into its own `node_modules/.bin`. pnpm only links *children*
    /// of a slot, so a tsc slot does not redundantly produce a shim for
    /// its own tsc binary.
    #[test]
    fn skips_slot_own_package_when_walking_children() {
        let tmp = tempdir().unwrap();
        let virtual_dir = tmp.path().join(".pacquet");

        let slot = virtual_dir.join("tsc@5.0.0");
        let modules = slot.join("node_modules");
        let pkg_dir = modules.join("tsc");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(
            pkg_dir.join("package.json"),
            json!({"name": "tsc", "version": "5.0.0", "bin": "tsc.js"}).to_string(),
        )
        .unwrap();
        fs::write(pkg_dir.join("tsc.js"), "#!/usr/bin/env node\n").unwrap();

        LinkVirtualStoreBins { virtual_store_dir: &virtual_dir }.run().unwrap();

        let bin_dir = pkg_dir.join("node_modules/.bin");
        // No children → bin dir should not exist at all (`link_bins_of_packages`
        // is a no-op when the package set is empty).
        assert!(!bin_dir.exists(), "self-bin must not be linked into own slot");
    }
}
