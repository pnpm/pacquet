//! Configuration and matching logic for pnpm's `patchedDependencies`.
//!
//! Ports the upstream `@pnpm/patching.types` and `@pnpm/patching.config`
//! workspaces (commit
//! [`b4f8f47ac2`](https://github.com/pnpm/pnpm/tree/b4f8f47ac2)) plus
//! the patch-file hashing in `@pnpm/lockfile.settings-checker`'s
//! [`calcPatchHashes`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/lockfile/settings-checker/src/calcPatchHashes.ts).
//!
//! Slice A of pacquet's `patchedDependencies` work (pacquet#397 item 9):
//! this crate is pure foundation. Nothing in the install pipeline
//! consumes it yet — slice B threads the per-snapshot patch into the
//! build trigger and the side-effects-cache key, and slice C applies
//! patches to extracted package directories.
//!
//! pnpm v11 reads `patchedDependencies` from `pnpm-workspace.yaml`,
//! not from `package.json`'s `pnpm` field. [`resolve_and_group`]
//! accordingly takes a workspace dir and a pre-parsed map — the
//! caller is responsible for surfacing the map (today: from yaml; in
//! the lockfile-only path, from `pnpm-lock.yaml`'s top-level
//! `patchedDependencies` field).

mod get_patch_info;
mod group;
mod hash;
mod key;
mod resolve;
mod types;
mod verify;

pub use get_patch_info::{PatchKeyConflictError, get_patch_info};
pub use group::{PatchInput, PatchNonSemverRangeError, group_patched_dependencies};
pub use hash::{CalcPatchHashError, calc_patch_hashes, create_hex_hash_from_file};
pub use key::{ParsedKey, parse_key};
pub use resolve::{ResolvePatchedDependenciesError, resolve_and_group};
pub use types::{ExtendedPatchInfo, PatchGroup, PatchGroupRangeItem, PatchGroupRecord, PatchInfo};
pub use verify::{UnusedPatchError, UnusedPatches, all_patch_keys, verify_patches};
