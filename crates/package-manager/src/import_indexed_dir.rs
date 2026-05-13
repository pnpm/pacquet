use crate::{CreateCasFilesError, create_cas_files};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_config::PackageImportMethod;
use pacquet_reporter::Reporter;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Error type for [`import_indexed_dir`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum ImportIndexedDirError {
    #[diagnostic(transparent)]
    CreateCasFiles(#[error(source)] CreateCasFilesError),
    #[display("failed to inspect existing target {path:?}: {error}")]
    InspectTarget {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("failed to clear non-directory dirent at {path:?}: {error}")]
    ClearNonDirEntry {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display(
        "failed to move existing {from:?} into staging directory {to:?} while preserving node_modules: {error}"
    )]
    PreserveModulesDir {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display(
        "the indexed file map already contains a node_modules/ entry at {path:?}, which would conflict with the directory being preserved"
    )]
    NodeModulesCollision { path: PathBuf },
    #[display("failed to remove existing directory {path:?} prior to swap: {error}")]
    RemoveExisting {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("failed to rename staging directory {from:?} to {to:?}: {error}")]
    Swap {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Materialize an indexed package as real files inside `dir_path`,
/// overwriting any pre-existing contents while preserving the package's
/// `node_modules/` subdirectory.
///
/// Mirrors pnpm v11's `importIndexedDir(..., { keepModulesDir: true })`
/// at `fs/indexed-pkg-importer/src/importIndexedDir.ts` — the fixed
/// option set used by the hoisted-linker's `linkHoistedModules` call
/// site, which always passes `force: true` and `keepModulesDir: true`
/// (`installing/deps-restorer/src/linkHoistedModules.ts`).
///
/// Behavior:
///
/// * If `dir_path` does not yet exist, this is equivalent to
///   [`create_cas_files()`] — make parent dirs, then link files in
///   parallel.
/// * If `dir_path` exists as a directory, the new contents are staged
///   in a sibling directory (so the rename stays on one filesystem),
///   any existing `dir_path/node_modules/` is moved into the staging
///   directory to preserve nested deps, the old directory is removed,
///   and the staging directory is renamed into place. On any failure
///   after the `node_modules/` move, the staged copy is restored to
///   `dir_path/node_modules/` before the staging directory is cleaned
///   up — staging never holds the user's only copy of nested deps.
/// * If `dir_path` exists as a regular file or a symlink, the dirent
///   is removed first and then the fresh-target path is taken. The
///   hoisted-linker won't produce that state in practice, but
///   refusing to clobber it would leave the install wedged.
///
/// Files in the package's `cas_paths` are materialized by
/// [`link_file()`] using `import_method`'s preference order
/// (hardlink → reflink → copy, etc.), and the per-method
/// `pnpm:package-import-method` log is emitted via `logged_methods` the
/// same way [`create_cas_files()`] does.
///
/// [`link_file()`]: crate::link_file()
pub fn import_indexed_dir<R: Reporter>(
    logged_methods: &AtomicU8,
    import_method: PackageImportMethod,
    dir_path: &Path,
    cas_paths: &HashMap<String, PathBuf>,
) -> Result<(), ImportIndexedDirError> {
    let existing_kind = match fs::symlink_metadata(dir_path) {
        Ok(meta) => Some(meta.file_type()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(ImportIndexedDirError::InspectTarget {
                path: dir_path.to_path_buf(),
                error,
            });
        }
    };

    match existing_kind {
        None => create_cas_files::<R>(logged_methods, import_method, dir_path, cas_paths)
            .map_err(ImportIndexedDirError::CreateCasFiles),
        Some(file_type) if !file_type.is_dir() => {
            // A regular file or a symlink occupies the target. Remove
            // the dirent and take the fresh-target path. Use
            // `remove_file` (not `remove_dir`) so symlinks-to-directory
            // are unlinked rather than recursed into.
            fs::remove_file(dir_path).map_err(|error| ImportIndexedDirError::ClearNonDirEntry {
                path: dir_path.to_path_buf(),
                error,
            })?;
            create_cas_files::<R>(logged_methods, import_method, dir_path, cas_paths)
                .map_err(ImportIndexedDirError::CreateCasFiles)
        }
        Some(_) => stage_and_swap::<R>(logged_methods, import_method, dir_path, cas_paths),
    }
}

fn stage_and_swap<R: Reporter>(
    logged_methods: &AtomicU8,
    import_method: PackageImportMethod,
    dir_path: &Path,
    cas_paths: &HashMap<String, PathBuf>,
) -> Result<(), ImportIndexedDirError> {
    let stage = pick_stage_path(dir_path);
    let target_modules = dir_path.join("node_modules");
    let stage_modules = stage.join("node_modules");

    // 1. Populate the staging directory with the new contents. On
    //    failure, the staging directory is the only thing on disk we
    //    own — a blanket rimraf is safe.
    if let Err(error) = create_cas_files::<R>(logged_methods, import_method, &stage, cas_paths) {
        let _ = fs::remove_dir_all(&stage);
        return Err(ImportIndexedDirError::CreateCasFiles(error));
    }

    // 2. Inspect the existing `node_modules/` so nested deps survive
    //    the swap. Only `NotFound` is benign — `PermissionDenied` and
    //    other transient I/O failures must surface, otherwise the
    //    user's nested deps get silently clobbered when the directory
    //    is removed in step 4.
    let nm_kind = match fs::symlink_metadata(&target_modules) {
        Ok(meta) => Some(meta.file_type()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(ImportIndexedDirError::InspectTarget { path: target_modules, error });
        }
    };

    // 3. Preserve `node_modules/` if it's a real directory. Track the
    //    move so steps 4 and 5 can rescue it on failure.
    //
    //    Indexed file maps for npm tarballs never contain
    //    `node_modules/` entries (npm and pnpm strip them at pack
    //    time), so a pre-existing `<stage>/node_modules/` would be
    //    pathological; surface it as an error rather than silently
    //    merging. Upstream's `moveOrMergeModulesDirs` performs a real
    //    merge for this case, but the hoisted-linker call site does
    //    not exercise it in practice.
    let nm_moved = match nm_kind {
        Some(file_type) if file_type.is_dir() => {
            if stage_modules.exists() {
                let _ = fs::remove_dir_all(&stage);
                return Err(ImportIndexedDirError::NodeModulesCollision { path: stage_modules });
            }
            if let Err(error) = fs::rename(&target_modules, &stage_modules) {
                let _ = fs::remove_dir_all(&stage);
                return Err(ImportIndexedDirError::PreserveModulesDir {
                    from: target_modules,
                    to: stage_modules,
                    error,
                });
            }
            true
        }
        Some(_) | None => false,
    };

    // 4. Remove the old contents. If this fails after step 3, the
    //    staged copy of `node_modules/` is the user's only copy —
    //    move it back into place before bailing.
    if let Err(error) = fs::remove_dir_all(dir_path) {
        rescue_node_modules(nm_moved, &stage_modules, &target_modules);
        let _ = fs::remove_dir_all(&stage);
        return Err(ImportIndexedDirError::RemoveExisting { path: dir_path.to_path_buf(), error });
    }

    // 5. Move the staged tree into place. There's a brief window
    //    between `remove_dir_all` and `rename` where `dir_path` does
    //    not exist on disk — acceptable given pacquet runs one
    //    install per process and the hoisted linker holds the install
    //    graph's coarse lock. If the rename fails, recreate
    //    `dir_path` so the rescued `node_modules/` has somewhere to
    //    land.
    if let Err(error) = fs::rename(&stage, dir_path) {
        if nm_moved && fs::create_dir_all(dir_path).is_ok() {
            rescue_node_modules(nm_moved, &stage_modules, &target_modules);
        }
        let _ = fs::remove_dir_all(&stage);
        return Err(ImportIndexedDirError::Swap { from: stage, to: dir_path.to_path_buf(), error });
    }
    Ok(())
}

/// Best-effort restoration of the preserved `node_modules/` directory
/// onto its original path after a partial stage-and-swap failure. The
/// caller has already decided to return an error to its caller; any
/// failure here is logged-and-swallowed because the surfaced error
/// already explains the underlying problem.
fn rescue_node_modules(nm_moved: bool, stage_modules: &Path, target_modules: &Path) {
    if !nm_moved {
        return;
    }
    if let Err(error) = fs::rename(stage_modules, target_modules) {
        tracing::warn!(
            target: "pacquet::import_indexed_dir",
            ?stage_modules,
            ?target_modules,
            %error,
            "failed to restore preserved node_modules/ after a partial stage-and-swap; \
             the staged copy is at the source path until cleanup runs",
        );
    }
}

/// Build a sibling path next to `target` that is unique within the
/// process. Mirrors pnpm's `fastPathTemp(newDir)` from the `path-temp`
/// package — same parent (so the final rename stays on one filesystem)
/// and a base name derived from the target so leaked staging dirs are
/// recognisable. Uniqueness across concurrent calls comes from PID +
/// wall-clock nanos + an atomic counter; we only need a process-local
/// guarantee because rayon worker threads are the only concurrent
/// callers.
fn pick_stage_path(target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("dir");
    let pid = std::process::id();
    let ctr = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    parent.join(format!("{name}_pacquet-stage_{pid}_{nanos}_{ctr}"))
}

#[cfg(test)]
mod tests;
