use crate::{symlink_package, SymlinkPackageError};
use pacquet_lockfile::{PkgName, SnapshotDepRef};
use std::{collections::HashMap, path::Path};

/// Create symlink layout of dependencies for a package in a virtual dir.
///
/// For npm-aliased dependencies (e.g. `string-width-cjs: string-width@4.2.3`),
/// the symlink filename under `node_modules/` uses the entry key (the alias),
/// while the virtual-store lookup uses the aliased target.
///
/// **NOTE:** `virtual_node_modules_dir` is assumed to already exist.
pub fn create_symlink_layout(
    dependencies: &HashMap<PkgName, SnapshotDepRef>,
    virtual_root: &Path,
    virtual_node_modules_dir: &Path,
) -> Result<(), SymlinkPackageError> {
    // Serial iteration here: the caller is expected to parallelise across
    // snapshots, so nesting a rayon `par_iter` inside would only add task-
    // scheduling overhead without giving rayon a wider work queue.
    dependencies.iter().try_for_each(|(alias_name, dep_ref)| {
        let target = dep_ref.resolve(alias_name);
        let virtual_store_name = target.to_virtual_store_name();
        let target_name_str = target.name.to_string();
        let alias_name_str = alias_name.to_string();
        symlink_package(
            &virtual_root.join(virtual_store_name).join("node_modules").join(&target_name_str),
            &virtual_node_modules_dir.join(&alias_name_str),
        )
    })
}
