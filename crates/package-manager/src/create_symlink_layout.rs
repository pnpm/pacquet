use crate::{symlink_package, SymlinkPackageError};
use pacquet_lockfile::{PkgName, SnapshotDepRef};
use std::{collections::HashMap, path::Path};

/// Create symlink layout of dependencies for a package in a virtual dir.
///
/// For npm-aliased dependencies (e.g. `string-width-cjs: string-width@4.2.3`),
/// the symlink filename under `node_modules/` uses the entry key (the alias),
/// while the virtual-store lookup uses the aliased target.
///
/// `virtual_node_modules_dir` does not have to exist — `symlink_package` calls
/// `fs::create_dir_all` on the symlink path's parent before each link. Callers
/// that already know the directory exists (e.g. `CreateVirtualStore::run`,
/// which `mkdir`s it just before calling this function) just pay redundant
/// stat syscalls, which is cheap and matches pnpm's own redundant-mkdir shape.
pub fn create_symlink_layout(
    dependencies: &HashMap<PkgName, SnapshotDepRef>,
    virtual_root: &Path,
    virtual_node_modules_dir: &Path,
) -> Result<(), SymlinkPackageError> {
    // Serial iteration: the symlink work per snapshot is small (a handful of
    // entries), so fanning out to rayon here would just add task-scheduling
    // overhead without a wider work queue to amortise it against. The
    // single-caller policy upstream is to run this stage single-threaded on a
    // `spawn_blocking` worker (see `CreateVirtualStore::run`), mirroring
    // pnpm's `symlinkAllModules` in `worker/src/start.ts`.
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
