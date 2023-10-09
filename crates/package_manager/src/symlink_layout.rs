use crate::symlink_pkg;
use pacquet_lockfile::{PackageSnapshotDependency, PkgName, PkgNameVerPeer};
use rayon::prelude::*;
use std::{collections::HashMap, path::Path};

/// Create symlink layout of dependencies for a package in a virtual dir.
///
/// **NOTE:** `virtual_node_modules_dir` is assumed to already exist.
pub fn create_symlink_layout(
    dependencies: &HashMap<PkgName, PackageSnapshotDependency>,
    virtual_root: &Path,
    virtual_node_modules_dir: &Path,
) {
    dependencies.par_iter().for_each(|(name, spec)| {
        let virtual_store_name = match spec {
            PackageSnapshotDependency::PkgVerPeer(ver_peer) => {
                let package_specifier = PkgNameVerPeer::new(name.clone(), ver_peer.clone()); // TODO: remove copying here
                package_specifier.to_virtual_store_name()
            }
            PackageSnapshotDependency::DependencyPath(dependency_path) => {
                dependency_path.package_specifier.to_virtual_store_name()
            }
        };
        let name_str = name.to_string();
        symlink_pkg(
            &virtual_root.join(virtual_store_name).join("node_modules").join(&name_str),
            &virtual_node_modules_dir.join(&name_str),
        );
    });
}
