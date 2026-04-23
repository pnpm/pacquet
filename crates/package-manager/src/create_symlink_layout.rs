use crate::symlink_package;
use pacquet_lockfile::{PkgName, PkgNameVerPeer, PkgVerPeer};
use rayon::prelude::*;
use std::{collections::HashMap, path::Path};

/// Create symlink layout of dependencies for a package in a virtual dir.
///
/// **NOTE:** `virtual_node_modules_dir` is assumed to already exist.
pub fn create_symlink_layout(
    dependencies: &HashMap<PkgName, PkgVerPeer>,
    virtual_root: &Path,
    virtual_node_modules_dir: &Path,
) {
    dependencies.par_iter().for_each(|(name, ver_peer)| {
        let package_specifier = PkgNameVerPeer::new(name.clone(), ver_peer.clone()); // TODO: remove copying here
        let virtual_store_name = package_specifier.to_virtual_store_name();
        let name_str = name.to_string();
        symlink_package(
            &virtual_root.join(virtual_store_name).join("node_modules").join(&name_str),
            &virtual_node_modules_dir.join(&name_str),
        )
        .expect("symlink pkg successful"); // TODO: properly propagate this error
    });
}
