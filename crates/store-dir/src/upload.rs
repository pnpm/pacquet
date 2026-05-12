//! WRITE-path orchestrator: re-CAFS a post-build package directory,
//! diff it against the pristine `PackageFilesIndex.files` row, and
//! seed the side-effects cache by re-queueing the mutated row through
//! [`StoreIndexWriter`].
//!
//! Ports pnpm's
//! [`storeController.upload`](https://github.com/pnpm/pnpm/blob/7e3145f9fc/store/controller/src/storeController/index.ts#L90-L99)
//! and the worker-side body at
//! [`worker/src/start.ts:312-383`](https://github.com/pnpm/pnpm/blob/7e3145f9fc/worker/src/start.ts#L312-L383).

use crate::{
    AddFilesFromDirError, CafsFileInfo, SideEffectsDiff, StoreDir, StoreIndex, StoreIndexError,
    StoreIndexWriter, add_files_from_dir,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

/// Error type of [`upload`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum UploadError {
    #[diagnostic(transparent)]
    AddFilesFromDir(#[error(source)] AddFilesFromDirError),

    #[diagnostic(transparent)]
    OpenIndex(#[error(source)] StoreIndexError),

    #[diagnostic(transparent)]
    ReadIndex(#[error(source)] StoreIndexError),

    #[display(
        "side-effects cache digest algorithm mismatch: row has {row_algo:?}, want {expected:?}"
    )]
    #[diagnostic(code(pacquet_store_dir::upload::algo_mismatch))]
    AlgoMismatch { row_algo: String, expected: String },
}

/// Digest algorithm pacquet writes into `PackageFilesIndex.algo`.
/// Held as a constant so the read-modify-write path can check the
/// existing row's algorithm before appending a side-effects diff,
/// matching upstream's [`ALGO_MISMATCH`](https://github.com/pnpm/pnpm/blob/7e3145f9fc/worker/src/start.ts#L358-L364)
/// guard.
pub const HASH_ALGORITHM: &str = "sha512";

/// Re-hash the built package directory, compute the diff against
/// the existing `PackageFilesIndex.files`, and re-queue the row
/// with `side_effects[side_effects_cache_key] = diff`.
///
/// Behaviour mirrors `pnpm/pnpm@7e3145f9fc:worker/src/start.ts:342-371`:
///
/// - If no existing row is found under `files_index_file`, returns
///   `Ok(())` without writing — upstream's `if (!existingFilesIndex) return`
///   bail-out. The base row will be populated by some other code path
///   (or never, if the package was already in CAFS); the next install
///   re-runs the build.
/// - If the existing row's `algo` differs from the constant
///   [`HASH_ALGORITHM`], returns `Err(UploadError::AlgoMismatch)`.
/// - Otherwise inserts `(side_effects_cache_key → diff)` into the
///   row's `side_effects` map (creating the map if absent) and
///   re-queues the row via `writer.queue`.
///
/// `requires_build` is left as-is on the existing row.  Upstream
/// recomputes it from `(manifest, filesMap)` when the field is
/// `None`; pacquet's row already carries a real value from the
/// download path so this is a no-op for the typical case.
pub fn upload(
    store_dir: &StoreDir,
    built_pkg_location: &Path,
    files_index_file: &str,
    side_effects_cache_key: &str,
    writer: &Arc<StoreIndexWriter>,
) -> Result<(), UploadError> {
    let added =
        add_files_from_dir(store_dir, built_pkg_location).map_err(UploadError::AddFilesFromDir)?;

    let index = StoreIndex::open_readonly_in(store_dir).map_err(UploadError::OpenIndex)?;
    let Some(mut existing) = index.get(files_index_file).map_err(UploadError::ReadIndex)? else {
        tracing::debug!(
            target: "pacquet::upload",
            files_index_file,
            "no existing package_index row; skipping side-effects upload",
        );
        return Ok(());
    };

    if existing.algo != HASH_ALGORITHM {
        return Err(UploadError::AlgoMismatch {
            row_algo: existing.algo.clone(),
            expected: HASH_ALGORITHM.to_string(),
        });
    }

    let diff = calculate_diff(&existing.files, &added.files);
    existing
        .side_effects
        .get_or_insert_with(HashMap::new)
        .insert(side_effects_cache_key.to_string(), diff);

    writer.queue(files_index_file.to_string(), existing);
    Ok(())
}

/// Set-difference over file digests + modes.  Mirrors
/// `pnpm/pnpm@7e3145f9fc:worker/src/start.ts:411-434`.
///
/// `base`     — the pristine `PackageFilesIndex.files` map (pre-build).
/// `current`  — the rehashed map produced by [`add_files_from_dir`].
///
/// Returns a [`SideEffectsDiff`] whose `added` entry covers files
/// present in `current` that either don't appear in `base` or whose
/// `digest`/`mode` differ from the base, and whose `deleted` entry
/// lists files present in `base` but absent in `current`. Both
/// fields use `Option<…>` with `skip_serializing_if = is_none`
/// (see `SideEffectsDiff`), so an empty side of the diff
/// round-trips through msgpack the same way pnpm's does.
pub fn calculate_diff(
    base: &HashMap<String, CafsFileInfo>,
    current: &HashMap<String, CafsFileInfo>,
) -> SideEffectsDiff {
    let mut added: HashMap<String, CafsFileInfo> = HashMap::new();
    let mut deleted: Vec<String> = Vec::new();
    let all_files: HashSet<&str> = base.keys().chain(current.keys()).map(String::as_str).collect();
    for file in all_files {
        match (base.get(file), current.get(file)) {
            (Some(_), None) => deleted.push(file.to_string()),
            (None, Some(now)) => {
                added.insert(file.to_string(), clone_info(now));
            }
            (Some(before), Some(now)) if before.digest != now.digest || before.mode != now.mode => {
                added.insert(file.to_string(), clone_info(now));
            }
            _ => {}
        }
    }
    SideEffectsDiff {
        added: (!added.is_empty()).then_some(added),
        deleted: (!deleted.is_empty()).then_some(deleted),
    }
}

fn clone_info(info: &CafsFileInfo) -> CafsFileInfo {
    CafsFileInfo {
        digest: info.digest.clone(),
        mode: info.mode,
        size: info.size,
        checked_at: info.checked_at,
    }
}

#[cfg(test)]
mod tests;
