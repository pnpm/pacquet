use crate::{create_cas_files, create_symlink_layout, CreateCasFilesError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{IoSendError, IoSendValue, IoTask, IoThread};
use pacquet_lockfile::{DependencyPath, PackageSnapshot};
use pacquet_npmrc::PackageImportMethod;
use pipe_trait::Pipe;
use std::{
    collections::HashMap,
    ffi::OsString,
    iter,
    path::{Path, PathBuf},
};

/// This subroutine installs the files from [`cas_paths`](Self::cas_paths) then creates the symlink layout.
#[must_use]
pub struct CreateVirtualDirBySnapshot<'a> {
    pub io_thread: &'a IoThread,
    pub virtual_store_dir: &'a Path,
    pub cas_paths: &'a HashMap<OsString, PathBuf>,
    pub import_method: PackageImportMethod,
    pub dependency_path: &'a DependencyPath,
    pub package_snapshot: &'a PackageSnapshot,
}

/// Error type of [`CreateVirtualDirBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualDirError {
    #[display(
        "Failed to send command to recursively create node_modules directory at {dir:?}: {error}"
    )]
    #[diagnostic(code(pacquet_package_manager::send_create_node_modules_dir))]
    SendCreateNodeModulesDir {
        dir: PathBuf,
        #[error(source)]
        error: IoSendError,
    },

    #[diagnostic(transparent)]
    CreateCasFiles(#[error(source)] CreateCasFilesError),
}

impl<'a> CreateVirtualDirBySnapshot<'a> {
    /// Execute the subroutine.
    pub fn run(self) -> Result<impl Iterator<Item = IoSendValue>, CreateVirtualDirError> {
        let CreateVirtualDirBySnapshot {
            io_thread, // TODO: use this
            virtual_store_dir,
            cas_paths,
            import_method,
            dependency_path,
            package_snapshot,
        } = self;

        // node_modules/.pacquet/pkg-name@x.y.z/node_modules
        let virtual_node_modules_dir = virtual_store_dir
            .join(dependency_path.package_specifier.to_virtual_store_name())
            .join("node_modules");
        let create_modules_dir_receiver = io_thread
            .send_and_listen(IoTask::CreateDirAll { dir_path: virtual_node_modules_dir.clone() })
            .map_err(|error| CreateVirtualDirError::SendCreateNodeModulesDir {
                dir: virtual_node_modules_dir.clone(),
                error,
            })?;

        // 1. Install the files from `cas_paths`
        let save_path =
            virtual_node_modules_dir.join(dependency_path.package_specifier.name.to_string());
        let create_cas_files_receiver =
            create_cas_files(io_thread, import_method, &save_path, cas_paths)
                .map_err(CreateVirtualDirError::CreateCasFiles)?;

        // 2. Create the symlink layout
        if let Some(dependencies) = &package_snapshot.dependencies {
            create_symlink_layout(dependencies, virtual_store_dir, &virtual_node_modules_dir)
        }

        create_modules_dir_receiver.pipe(iter::once).chain(create_cas_files_receiver).pipe(Ok)
    }
}
