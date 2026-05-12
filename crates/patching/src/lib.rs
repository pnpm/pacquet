//! Configuration and matching logic for pnpm's `patchedDependencies`.
//!
//! Ports the upstream `@pnpm/patching.types` and `@pnpm/patching.config`
//! workspaces (commit
//! [`b4f8f47ac2`](https://github.com/pnpm/pnpm/tree/b4f8f47ac2)) plus
//! the patch-file hashing in `@pnpm/lockfile.settings-checker`'s
//! [`calcPatchHashes`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/lockfile/settings-checker/src/calcPatchHashes.ts).
//!
//! Pacquet's `patchedDependencies` work (pacquet#397 item 9) lands
//! across multiple slices. This PR (slices A + B) covers everything
//! up to threading the patch hash into the side-effects-cache key:
//!
//! 1. Types, parser, grouping, matcher, verify, hashing, and the
//!    workspace-dir-anchored [`resolve_and_group`] helper.
//! 2. `pacquet-config` exposes
//!    [`Config::resolved_patched_dependencies`][crate-config] and the
//!    install pipeline (`InstallFrozenLockfile::run`) calls it once
//!    per install, looks each snapshot up with [`get_patch_info()`],
//!    and threads the resulting map into `BuildModules`.
//! 3. `BuildModules` passes the per-snapshot
//!    [`ExtendedPatchInfo::hash`] into
//!    `pacquet_graph_hasher::CalcDepStateOptions::patch_file_hash`
//!    so the cache key includes `;patch=<hash>` when a snapshot is
//!    patched.
//!
//! Slice C will land the remaining pieces: the build-trigger update
//! (`requires_build || patch.is_some()`) and the actual patch
//! application to extracted package dirs before postinstall hooks.
//!
//! pnpm v11 reads `patchedDependencies` from `pnpm-workspace.yaml`,
//! not from `package.json`'s `pnpm` field. [`resolve_and_group`]
//! accordingly takes a workspace dir and a pre-parsed
//! [`IndexMap`][indexmap::IndexMap] — the caller is responsible for
//! surfacing the map (today: from yaml; in the lockfile-only path,
//! from `pnpm-lock.yaml`'s top-level `patchedDependencies` field).
//!
//! [crate-config]: ../pacquet_config/struct.Config.html#method.resolved_patched_dependencies

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
