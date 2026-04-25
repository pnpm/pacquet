use crate::{link_file, LinkFileError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Error type for [`create_cas_files`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateCasFilesError {
    #[diagnostic(transparent)]
    LinkFile(#[error(source)] LinkFileError),
}

/// Below this file count, [`create_cas_files`] skips rayon's
/// `par_iter` and runs the `link_file` loop on the calling thread.
///
/// Rationale: rayon's per-element dispatch (push to the work-
/// stealing deque, scheduler poll, worker pickup, result fold-back)
/// has a fixed overhead that's amortized across "many" elements but
/// dominates "a few". Below ~16 files the work fits comfortably on a
/// single thread before rayon's overhead would even pay back. Pacquet
/// runs `create_cas_files` once per snapshot and the lockfile fixture
/// has thousands of snapshots, many of them ≤ 8 files (single-file
/// shims, tiny utility packages, scoped re-exports). Skipping rayon
/// for those keeps the global pool free for the actually-chunky
/// packages where parallelism matters (`typescript`, `webpack`, ...).
///
/// Threshold picked conservatively. The break-even point varies with
/// the link strategy (`reflink` is microseconds; `copy` of a few KiB
/// is also microseconds), so `8` is a deliberate "definitely too few
/// to be worth dispatching" cutoff rather than a tuned ratio.
const SEQUENTIAL_FAN_OUT_THRESHOLD: usize = 8;

/// If `dir_path` doesn't exist, create and populate it with files from `cas_paths`.
///
/// If `dir_path` already exists, do nothing.
pub fn create_cas_files(
    import_method: PackageImportMethod,
    dir_path: &Path,
    cas_paths: &HashMap<String, PathBuf>,
) -> Result<(), CreateCasFilesError> {
    if dir_path.exists() {
        return Ok(());
    }

    let link_one = |(cleaned_entry, store_path): (&String, &PathBuf)| -> Result<(), LinkFileError> {
        link_file(import_method, store_path, &dir_path.join(cleaned_entry))
    };

    if cas_paths.len() < SEQUENTIAL_FAN_OUT_THRESHOLD {
        cas_paths.iter().try_for_each(link_one).map_err(CreateCasFilesError::LinkFile)
    } else {
        cas_paths.par_iter().try_for_each(link_one).map_err(CreateCasFilesError::LinkFile)
    }
}
