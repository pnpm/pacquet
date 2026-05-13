//! Pacquet port of upstream pnpm's `@pnpm/store.controller`
//! [`projectRegistry`](https://github.com/pnpm/pnpm/blob/94240bc046/store/controller/src/storeController/projectRegistry.ts).
//!
//! The project registry is a flat directory of symlinks at
//! `<store_dir>/projects/<short-hash>` that point back to every project
//! using the global virtual store. The prune sweep walks this directory
//! to learn which projects still reference the shared `<store_dir>/links`
//! slots — without it, a `pacquet store prune` (tracked separately) could
//! not distinguish abandoned packages from packages a project still uses.
//!
//! Stage 1 of pnpm/pacquet#432 only writes registry entries; the prune
//! sweep and `getRegisteredProjects` stale-entry cleanup are deferred.

use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::symlink_dir;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

/// Compute the project-registry slug for `input`. Mirrors upstream's
/// [`createShortHash`](https://github.com/pnpm/pnpm/blob/94240bc046/crypto/hash/src/index.ts):
/// the sha256 hex digest, truncated to the first 32 characters (16 bytes
/// of entropy — enough to make collisions across one user's projects
/// vanishingly unlikely).
pub fn create_short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut hex = format!("{digest:x}");
    hex.truncate(32);
    hex
}

/// Error type for [`register_project`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum RegisterProjectError {
    #[display("Failed to create the projects registry directory at {dir:?}: {error}")]
    #[diagnostic(code(pacquet_store_dir::register_project::create_registry_dir))]
    CreateRegistryDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display(
        "Failed to inspect the existing entry at {link_path:?} while registering project {project_dir:?}: {error}"
    )]
    #[diagnostic(code(pacquet_store_dir::register_project::inspect_existing))]
    InspectExisting {
        project_dir: PathBuf,
        link_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display(
        "Failed to remove stale entry at {link_path:?} (pointed at {old_target:?}, expected {project_dir:?}): {error}"
    )]
    #[diagnostic(code(pacquet_store_dir::register_project::remove_stale))]
    RemoveStale {
        project_dir: PathBuf,
        link_path: PathBuf,
        old_target: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display(
        "Failed to create the project registry symlink at {link_path:?} pointing to {project_dir:?}: {error}"
    )]
    #[diagnostic(code(pacquet_store_dir::register_project::create_symlink))]
    CreateSymlink {
        project_dir: PathBuf,
        link_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Register `project_dir` as a user of the global virtual store at
/// `store_dir` by writing a symlink at
/// `<store_dir>/projects/<create_short_hash(project_dir)>` pointing
/// back at `project_dir`. Mirrors upstream's
/// [`registerProject`](https://github.com/pnpm/pnpm/blob/94240bc046/store/controller/src/storeController/projectRegistry.ts).
///
/// Skips silently when `store_dir` lives inside `project_dir` — the
/// "store inside the project" case (legacy `--store-dir node_modules/.pnpm`
/// setups, or just a typo) would otherwise create a self-referential
/// symlink. Matches upstream's `isSubdir(projectDir, storeDir)` guard.
///
/// Idempotent: if the symlink already exists pointing at the same
/// project, the function is a no-op. If a previous entry under the
/// same short hash points elsewhere (very unlikely — would require a
/// sha256 collision in the first 32 hex chars), the stale entry is
/// removed and re-created so a re-run heals the registry.
pub fn register_project(
    store_dir: &StoreDir,
    project_dir: &Path,
) -> Result<(), RegisterProjectError> {
    // Upstream's `isSubdir(projectDir, storeDir)` is `(parent, child)`
    // — the npm `is-subdir` package signature. Skip when the store
    // root lives at or under the project dir.
    if path_contains(project_dir, store_dir.root()) {
        return Ok(());
    }

    let registry_dir = store_dir.projects();
    fs::create_dir_all(&registry_dir).map_err(|error| RegisterProjectError::CreateRegistryDir {
        dir: registry_dir.clone(),
        error,
    })?;

    let project_dir_str = project_dir.to_string_lossy();
    let link_path = registry_dir.join(create_short_hash(&project_dir_str));

    // Fast path: link doesn't exist yet — just create it.
    match symlink_dir(project_dir, &link_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            // Either the same project re-registering (no-op) or an
            // unrelated path that hashed to the same slug (heal).
            // Resolve and compare the existing link's target.
            let existing_target = fs::read_link(&link_path).map_err(|error| {
                RegisterProjectError::InspectExisting {
                    project_dir: project_dir.to_path_buf(),
                    link_path: link_path.clone(),
                    error,
                }
            })?;
            let canonical_existing = canonicalize_or_join(&link_path, &existing_target);
            let canonical_project =
                dunce::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
            if canonical_existing == canonical_project {
                return Ok(());
            }
            // Mismatch — remove the stale entry and recreate.
            fs::remove_file(&link_path).map_err(|error| RegisterProjectError::RemoveStale {
                project_dir: project_dir.to_path_buf(),
                link_path: link_path.clone(),
                old_target: existing_target.clone(),
                error,
            })?;
            symlink_dir(project_dir, &link_path).map_err(|error| {
                RegisterProjectError::CreateSymlink {
                    project_dir: project_dir.to_path_buf(),
                    link_path,
                    error,
                }
            })
        }
        Err(error) => Err(RegisterProjectError::CreateSymlink {
            project_dir: project_dir.to_path_buf(),
            link_path,
            error,
        }),
    }
}

/// Port of npm `is-subdir`: returns `true` when `inner` is `outer`
/// itself or any descendant of it. Renamed from upstream's
/// `isSubdir(parent, child)` because the bare name reads ambiguously
/// at the call site — `path_contains(outer, inner)` reads
/// unambiguously as "does `outer` contain `inner`".
///
/// Both paths are compared by their canonical (resolved) form so
/// symlinks don't fool the check. When either path can't be
/// canonicalized (typically the store dir hasn't been created yet),
/// fall back to a lexical comparison so the guard stays defensive
/// against the legacy "store inside the project" case.
fn path_contains(outer: &Path, inner: &Path) -> bool {
    let outer_canonical = dunce::canonicalize(outer).unwrap_or_else(|_| outer.to_path_buf());
    let inner_canonical = dunce::canonicalize(inner).unwrap_or_else(|_| inner.to_path_buf());
    inner_canonical.starts_with(&outer_canonical)
}

/// Best-effort canonicalization for a symlink target: if the target is
/// absolute and canonicalizable, return its canonical form; otherwise
/// resolve it relative to the link's parent dir and try again; on any
/// failure return the lexically resolved path. Mirrors how upstream's
/// `getRegisteredProjects` handles `path.isAbsolute(target) ? target :
/// path.resolve(path.dirname(linkPath), target)`.
fn canonicalize_or_join(link_path: &Path, target: &Path) -> PathBuf {
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path.parent().map(|p| p.join(target)).unwrap_or_else(|| target.to_path_buf())
    };
    dunce::canonicalize(&absolute).unwrap_or(absolute)
}

#[cfg(test)]
mod tests {
    use super::{create_short_hash, register_project};
    use crate::StoreDir;
    use std::fs;
    use tempfile::tempdir;

    /// `create_short_hash` is sha256-hex truncated to 32 chars.
    /// Matches upstream's
    /// [`createShortHash`](https://github.com/pnpm/pnpm/blob/94240bc046/crypto/hash/src/index.ts):
    /// `crypto.hash('sha256', input, 'hex').substring(0, 32)`. Pinned
    /// vector for parity:
    ///
    /// ```sh
    /// printf pacquet | shasum -a 256 | head -c 32
    /// # => 6784def0191a0dd68103a05ab700b31c
    /// ```
    #[test]
    fn short_hash_is_first_32_hex_chars_of_sha256() {
        let got = create_short_hash("pacquet");
        assert_eq!(got, "6784def0191a0dd68103a05ab700b31c");
        assert_eq!(got.len(), 32, "short hash must be exactly 32 hex chars");
        assert_ne!(got, create_short_hash("pacquet "));
    }

    /// A fresh registry: writing the entry creates the projects dir
    /// and a symlink whose target resolves to the project dir.
    #[test]
    fn register_creates_symlink_to_project_dir() {
        let project = tempdir().unwrap();
        let store = tempdir().unwrap();
        let store_dir = StoreDir::new(store.path().to_path_buf());

        register_project(&store_dir, project.path()).expect("register succeeds");

        let registry_dir = store_dir.projects();
        assert!(registry_dir.is_dir(), "projects dir must be created");
        let mut entries: Vec<_> = fs::read_dir(&registry_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "exactly one entry per project");
        let entry = entries.pop().unwrap().unwrap();
        let target = fs::read_link(entry.path()).expect("entry is a symlink");
        assert_eq!(
            dunce::canonicalize(&target).unwrap(),
            dunce::canonicalize(project.path()).unwrap(),
            "symlink resolves back to the project dir",
        );
    }

    /// Re-registering the same project is a no-op: no duplicate slot,
    /// no error.
    #[test]
    fn register_is_idempotent_on_repeat() {
        let project = tempdir().unwrap();
        let store = tempdir().unwrap();
        let store_dir = StoreDir::new(store.path().to_path_buf());

        register_project(&store_dir, project.path()).expect("first register");
        register_project(&store_dir, project.path()).expect("second register (idempotent)");

        let registry_dir = store_dir.projects();
        let entries: Vec<_> = fs::read_dir(&registry_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "still exactly one entry after re-register");
    }

    /// Subdir guard: when the store lives inside the project, the
    /// function is a silent no-op — registering would otherwise create
    /// a self-referential symlink.
    #[test]
    fn register_skips_when_store_is_inside_project() {
        let project = tempdir().unwrap();
        let store_path = project.path().join("nested-store");
        fs::create_dir_all(&store_path).unwrap();
        let store_dir = StoreDir::new(store_path);

        register_project(&store_dir, project.path()).expect("subdir case is a no-op");
        // No projects/ dir should have been created.
        assert!(
            !store_dir.projects().exists(),
            "subdir guard must skip the registry-dir creation entirely",
        );
    }
}
