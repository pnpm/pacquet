use crate::{CreateCasFilesError, SymlinkPackageError, create_cas_files, create_symlink_layout};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{PackageKey, SnapshotEntry};
use pacquet_npmrc::PackageImportMethod;
use pacquet_reporter::Reporter;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::AtomicU8,
};

/// This subroutine creates the virtual-store slot for one package and then
/// runs the two post-extraction tasks — CAS file import and intra-package
/// symlink creation — in parallel via `rayon::join`.
///
/// Symlinks don't depend on CAS file contents, only on the resolved dep graph,
/// so overlapping them with the import saves the serial symlink time per
/// snapshot (~1-3 ms). Across a big lockfile those savings stack up on the
/// install's critical-path tail.
#[must_use]
pub struct CreateVirtualDirBySnapshot<'a> {
    pub virtual_store_dir: &'a Path,
    pub cas_paths: &'a HashMap<String, PathBuf>,
    pub import_method: PackageImportMethod,
    /// Install-scoped dedupe state for `pnpm:package-import-method`.
    /// See the comment on `link_file::log_method_once` for why this
    /// is install-scoped rather than module-static.
    pub logged_methods: &'a AtomicU8,
    pub package_key: &'a PackageKey,
    pub snapshot: &'a SnapshotEntry,
}

/// Error type of [`CreateVirtualDirBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualDirError {
    #[display("Failed to recursively create node_modules directory at {dir:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::create_node_modules_dir))]
    CreateNodeModulesDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[diagnostic(transparent)]
    CreateCasFiles(#[error(source)] CreateCasFilesError),

    #[diagnostic(transparent)]
    SymlinkPackage(#[error(source)] SymlinkPackageError),
}

impl<'a> CreateVirtualDirBySnapshot<'a> {
    /// Execute the subroutine.
    pub fn run<R: Reporter>(self) -> Result<(), CreateVirtualDirError> {
        let CreateVirtualDirBySnapshot {
            virtual_store_dir,
            cas_paths,
            import_method,
            logged_methods,
            package_key,
            snapshot,
        } = self;

        let virtual_node_modules_dir =
            virtual_store_dir.join(package_key.to_virtual_store_name()).join("node_modules");
        fs::create_dir_all(&virtual_node_modules_dir).map_err(|error| {
            CreateVirtualDirError::CreateNodeModulesDir {
                dir: virtual_node_modules_dir.clone(),
                error,
            }
        })?;

        let save_path = virtual_node_modules_dir.join(package_key.name.to_string());

        // `rayon::join` runs both closures in parallel on rayon's pool,
        // returning only once both finish. `create_cas_files` is itself a
        // rayon par_iter over CAS entries; `create_symlink_layout` is a
        // small serial loop over dep refs. Overlapping them saves the
        // symlink time from the per-snapshot critical path without any
        // cross-thread data marshaling — both closures borrow from the
        // current stack frame.
        let (cas_result, symlink_result) = rayon::join(
            || {
                create_cas_files::<R>(logged_methods, import_method, &save_path, cas_paths)
                    .map_err(CreateVirtualDirError::CreateCasFiles)
            },
            || match snapshot.dependencies.as_ref() {
                Some(dependencies) => create_symlink_layout(
                    dependencies,
                    virtual_store_dir,
                    &virtual_node_modules_dir,
                )
                .map_err(CreateVirtualDirError::SymlinkPackage),
                None => Ok(()),
            },
        );
        cas_result?;
        symlink_result?;
        Ok(())
    }
}
