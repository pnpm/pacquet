//! Validate the on-disk `node_modules/.modules.yaml` against the
//! current install's effective config.
//!
//! Mirrors upstream pnpm's
//! [`validateModules`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/validateModules.ts)
//! composed with
//! [`checkCompatibility`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/checkCompatibility/index.ts).
//!
//! Each install reads the previous run's `.modules.yaml` (when
//! present) and compares the recorded layout-driving fields against
//! the current install options. A mismatch means the layout on disk
//! was produced under different settings — re-running the install
//! with the new settings would silently leave artifacts of the old
//! shape behind. Pacquet errors out instead of silently doing the
//! wrong thing; today the user recovers by removing `node_modules/`
//! and re-running the install. The automatic purge path (upstream's
//! `forceNewModules` — `pnpm install --force`) is tracked under
//! #464 §B; pacquet doesn't expose a `--force` install flag yet.
//!
//! Today this module covers section §A of #464 — the **read-and-error**
//! pipeline. The `forceNewModules` purge path (§B) and the
//! `virtualStoreOnly` exemption (§C) are deferred until the
//! corresponding install modes ship.
//!
//! ## Axes covered
//!
//! - **`hoist_pattern`** — `HOIST_PATTERN_DIFF`. Most user-facing
//!   axis: changing `pnpm-workspace.yaml`'s `hoistPattern` between
//!   installs would otherwise leave stale private-hoist symlinks in
//!   `<vs>/node_modules/`.
//! - **`public_hoist_pattern`** — `PUBLIC_HOIST_PATTERN_DIFF`. Same
//!   as above for public hoist into `<root>/node_modules/`.
//! - **`included` (dependency groups)** — `INCLUDED_DEPS_CONFLICT`.
//!   `pacquet install --prod` followed by `pacquet install` would
//!   silently merge prod-only and dev layouts otherwise.
//! - **`virtual_store_dir_max_length`** — `VIRTUAL_STORE_DIR_MAX_LENGTH_DIFF`.
//!   Changing the value rewrites every slot's directory name; old
//!   slots become orphans without a re-import.
//! - **`store_dir`** / **`virtual_store_dir`** — `UNEXPECTED_STORE` /
//!   `UNEXPECTED_VIRTUAL_STORE_DIR`. The `<vs>/<slot>/node_modules`
//!   symlinks point at the recorded `store_dir`; changing it
//!   between installs strands those symlinks.
//! - **`layout_version`** — handled at deserialize-time by the
//!   `LayoutVersion` newtype's `try_from` impl, so a mismatched
//!   on-disk value surfaces as a `ReadModulesError` before this
//!   validator runs. Listed here so the issue's coverage table is
//!   complete.

use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_config::Config;
use pacquet_modules_yaml::{IncludedDependencies, Modules};
use std::path::{Path, PathBuf};

/// Error returned by [`validate_modules`].
///
/// One variant per axis. Codes match upstream's `ERR_PNPM_*` /
/// kebab-case symbols where possible — the user-facing strings come
/// straight from
/// [`validateModules.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/validateModules.ts)
/// and
/// [`checkCompatibility/index.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/checkCompatibility/index.ts)
/// so a user who sees the message in pnpm and pacquet gets the same
/// wording.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum ValidateModulesError {
    /// Recorded `hoistPattern` differs from the current install
    /// options. Mirrors upstream's `HOIST_PATTERN_DIFF` throw at
    /// [`validateModules.ts:88-92`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/validateModules.ts#L88-L92).
    /// Upstream's `--force` would purge and rebuild; pacquet's
    /// `--force` install flag is tracked under #464 §B and isn't
    /// exposed yet, so the user-facing help points at the manual
    /// recovery path.
    #[display(
        "This modules directory was created using a different hoist-pattern value. Run \"pnpm install\" to recreate the modules directory."
    )]
    #[diagnostic(
        code(pacquet_package_manager::hoist_pattern_diff),
        help(
            "Restore the previous `hoistPattern` in `pnpm-workspace.yaml`, or remove `node_modules/` and re-run `pacquet install --frozen-lockfile`."
        )
    )]
    HoistPatternDiff,

    /// Recorded `publicHoistPattern` differs. Mirrors upstream's
    /// `PUBLIC_HOIST_PATTERN_DIFF` at
    /// [`validateModules.ts:74-79`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/validateModules.ts#L74-L79).
    #[display(
        "This modules directory was created using a different public-hoist-pattern value. Run \"pnpm install\" to recreate the modules directory."
    )]
    #[diagnostic(
        code(pacquet_package_manager::public_hoist_pattern_diff),
        help(
            "Restore the previous `publicHoistPattern` in `pnpm-workspace.yaml`, or remove `node_modules/` and re-run `pacquet install --frozen-lockfile`."
        )
    )]
    PublicHoistPatternDiff,

    /// Recorded `included` dependency groups differ. Mirrors
    /// upstream's `INCLUDED_DEPS_CONFLICT` at
    /// [`validateModules.ts:108-113`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/validateModules.ts#L108-L113).
    /// The `lockfile_dir` in the message is upstream's wording for
    /// the install root.
    #[display(
        "modules directory (at \"{}\") was installed with {recorded}. Current install wants {requested}.",
        lockfile_dir.display()
    )]
    #[diagnostic(
        code(pacquet_package_manager::included_deps_conflict),
        help(
            "Re-run the install with the same dependency groups (`--prod` / default / `--dev`) as before, or remove `node_modules/` and re-run `pacquet install --frozen-lockfile`."
        )
    )]
    IncludedDepsConflict { lockfile_dir: PathBuf, recorded: String, requested: String },

    /// Recorded `virtualStoreDirMaxLength` differs. Mirrors upstream's
    /// `VIRTUAL_STORE_DIR_MAX_LENGTH_DIFF` at
    /// [`validateModules.ts:55-65`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/validateModules.ts#L55-L65).
    /// Pacquet doesn't yet read `virtualStoreDirMaxLength` from
    /// `pnpm-workspace.yaml` (the loader pins
    /// [`pacquet_modules_yaml::DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH`]
    /// = 120 unconditionally), so this variant is reachable today
    /// only when the on-disk `.modules.yaml` was written by a
    /// pnpm install with a non-default value. Once pacquet honors
    /// the yaml key, the help text below should switch to
    /// "restore the previous value or remove and re-install".
    #[display(
        "This modules directory was created using a different virtual-store-dir-max-length value. Run \"pnpm install\" to recreate the modules directory."
    )]
    #[diagnostic(
        code(pacquet_package_manager::virtual_store_dir_max_length_diff),
        help(
            "Remove `node_modules/` and re-run `pacquet install --frozen-lockfile`. Pacquet doesn't yet read `virtualStoreDirMaxLength` from `pnpm-workspace.yaml`, so the recorded value can only be matched by re-creating the modules directory."
        )
    )]
    VirtualStoreDirMaxLengthDiff,

    /// Recorded `storeDir` differs from the current store root.
    /// Mirrors upstream's `UnexpectedStoreError` at
    /// [`checkCompatibility/index.ts:25-39`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/checkCompatibility/index.ts#L25-L39).
    /// The store is referenced by every `<vs>/<slot>/node_modules`
    /// symlink — relocating it strands those.
    #[display(
        "Unexpected store location. The modules directory at \"{}\" was created with a different store: \"{recorded}\" (current install uses \"{requested}\").",
        modules_dir.display()
    )]
    #[diagnostic(
        code(pacquet_package_manager::unexpected_store),
        help(
            "Set `storeDir` back to the recorded value, or remove `node_modules/` and re-run `pacquet install --frozen-lockfile` against the new store."
        )
    )]
    UnexpectedStore { modules_dir: PathBuf, recorded: String, requested: String },

    /// Recorded `virtualStoreDir` differs. Mirrors upstream's
    /// `UnexpectedVirtualStoreDirError` at
    /// [`checkCompatibility/index.ts:41-47`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/checkCompatibility/index.ts#L41-L47).
    /// Less common than `storeDir` drift; usually only triggers
    /// when the user adds an explicit `virtualStoreDir` to yaml
    /// that didn't match what pacquet was using as the implicit
    /// `<modules_dir>/.pnpm` default.
    #[display(
        "Unexpected virtual store location. The modules directory at \"{}\" was created at virtual store \"{recorded}\" (current install uses \"{requested}\").",
        modules_dir.display()
    )]
    #[diagnostic(
        code(pacquet_package_manager::unexpected_virtual_store_dir),
        help(
            "Set `virtualStoreDir` back to the recorded value, or remove `node_modules/` and re-run `pacquet install --frozen-lockfile`."
        )
    )]
    UnexpectedVirtualStoreDir { modules_dir: PathBuf, recorded: String, requested: String },
}

/// Compare the on-disk [`Modules`] manifest against the current
/// install's effective options. `Ok(())` means the layout on disk is
/// compatible with the current install; any other return is a typed
/// drift the caller surfaces to the user.
///
/// Drift axes are checked in upstream's order
/// ([`validateModules.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/validateModules.ts)
/// then per-importer
/// [`checkCompatibility`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/checkCompatibility/index.ts)),
/// so the first error a user sees matches the first error pnpm
/// would surface for the same drift.
///
/// `lockfile_dir` is the install root (the directory containing
/// `pnpm-lock.yaml` / `pnpm-workspace.yaml`) — used in the
/// `INCLUDED_DEPS_CONFLICT` message and as the per-importer scope
/// for upstream's per-importer loop. Pacquet's first slice runs
/// the per-importer check once at the install root; widening to
/// every workspace project tracks alongside per-importer
/// `included` overrides.
///
/// **Out of scope for this slice (§B / §C of #464):**
///
/// - The `--force` purge path. When `force` is passed today, the
///   caller still sees a typed drift and decides; later work plumbs
///   `force` into this function and either purges + returns Ok or
///   propagates the drift error.
/// - The `virtualStoreOnly` exemption. Pacquet doesn't implement
///   `virtualStoreOnly` install yet, but the field is on `Modules`
///   — guard against future activation without changing this path.
pub fn validate_modules(
    modules: &Modules,
    config: &Config,
    requested_included: IncludedDependencies,
    lockfile_dir: &Path,
    modules_dir: &Path,
) -> Result<(), ValidateModulesError> {
    // 1. `virtualStoreDirMaxLength` — upstream checks this first
    //    because it implies every slot path on disk has a different
    //    name from what the current install would compute. Pacquet
    //    pins the default 120 today, but a yaml override would
    //    surface here.
    if modules.virtual_store_dir_max_length
        != pacquet_modules_yaml::DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH
    {
        return Err(ValidateModulesError::VirtualStoreDirMaxLengthDiff);
    }

    // 2. `publicHoistPattern` — upstream order: public before
    //    private (`validateModules.ts:67-92`). The recorded value is
    //    `Option<Vec<String>>`; pacquet's `Config.public_hoist_pattern`
    //    matches the same shape. `None`-vs-`Some([])` is the same
    //    "no public hoist" semantic and treated as equal.
    if !patterns_equal(
        modules.public_hoist_pattern.as_deref(),
        config.public_hoist_pattern.as_deref(),
    ) {
        return Err(ValidateModulesError::PublicHoistPatternDiff);
    }

    // 3. `hoistPattern` — same shape as the public side. Upstream
    //    runs this inside a try/catch so the error can be converted
    //    to a per-importer purge under `forceNewModules`; pacquet's
    //    first slice just bubbles the error up to the install
    //    pipeline.
    if !patterns_equal(modules.hoist_pattern.as_deref(), config.hoist_pattern.as_deref()) {
        return Err(ValidateModulesError::HoistPatternDiff);
    }

    // 4. `checkCompatibility` axes (per-importer in upstream;
    //    pacquet's first slice runs at the install root). The
    //    `layoutVersion` axis is enforced by `LayoutVersion::try_from`
    //    at deserialize time — a mismatched on-disk value surfaces
    //    as `ReadModulesError::ParseYaml` before this validator
    //    even runs, so there's no axis to add here for it.

    // 4a. `storeDir` — `path.relative(a, b) === ''` upstream;
    //    pacquet collapses to lex equality after canonicalising
    //    via `Path::new` + `==`. Both fields are absolute strings
    //    on disk so trailing-slash drift isn't a concern.
    let recorded_store_dir = modules.store_dir.as_str();
    let requested_store_dir = config.store_dir.display().to_string();
    if !paths_equal(recorded_store_dir, &requested_store_dir) {
        return Err(ValidateModulesError::UnexpectedStore {
            modules_dir: modules_dir.to_path_buf(),
            recorded: recorded_store_dir.to_string(),
            requested: requested_store_dir,
        });
    }

    // 4b. `virtualStoreDir` — same path-equal check. The recorded
    //    value is the post-`resolve_virtual_store_dir` absolute
    //    path (modules-yaml does that resolution at load time),
    //    so comparing against `config.virtual_store_dir` directly
    //    works.
    let recorded_vs_dir = modules.virtual_store_dir.as_str();
    let requested_vs_dir = config.virtual_store_dir.to_string_lossy();
    if !paths_equal(recorded_vs_dir, &requested_vs_dir) {
        return Err(ValidateModulesError::UnexpectedVirtualStoreDir {
            modules_dir: modules_dir.to_path_buf(),
            recorded: recorded_vs_dir.to_string(),
            requested: requested_vs_dir.into_owned(),
        });
    }

    // 5. `included` — pacquet's first slice runs at the root since
    //    upstream's per-importer loop primarily exists for
    //    workspace projects with their own dependency-group
    //    overrides (which pacquet doesn't surface yet). The
    //    install pipeline writes the same `IncludedDependencies`
    //    for every importer.
    if modules.included != requested_included {
        return Err(ValidateModulesError::IncludedDepsConflict {
            lockfile_dir: lockfile_dir.to_path_buf(),
            recorded: stringify_included(modules.included),
            requested: stringify_included(requested_included),
        });
    }

    Ok(())
}

/// Compare two `Option<&[String]>` pattern lists. `None` and
/// `Some([])` both mean "no patterns" and compare equal. Mirrors
/// upstream's `equals(modules.publicHoistPattern ?? [], opts.publicHoistPattern ?? [])`
/// (deepEquals on the unwrapped arrays).
fn patterns_equal(a: Option<&[String]>, b: Option<&[String]>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x == y,
        (Some(x), None) | (None, Some(x)) => x.is_empty(),
    }
}

/// Compare two filesystem paths for equality, mirroring upstream's
/// `path.relative(a, b) === ''` check.
///
/// `path.relative` returns `''` only when the two paths resolve to
/// the same point regardless of how they're spelled. For pacquet,
/// both inputs come from already-absolutized strings the
/// modules-yaml loader produced, so `Path::new(a) == Path::new(b)`
/// is a faithful equivalent — Rust's `Path::eq` is component-wise
/// (one `/foo` is one `/foo`, not `/foo/` or `/foo/.`).
fn paths_equal(a: &str, b: &str) -> bool {
    Path::new(a) == Path::new(b)
}

/// Render an [`IncludedDependencies`] as the comma-separated label
/// upstream uses in the error message. Order matches upstream's
/// `DEPENDENCIES_FIELDS` constant: `dependencies, devDependencies,
/// optionalDependencies`.
fn stringify_included(included: IncludedDependencies) -> String {
    let mut parts = Vec::with_capacity(3);
    if included.dependencies {
        parts.push("dependencies");
    }
    if included.dev_dependencies {
        parts.push("devDependencies");
    }
    if included.optional_dependencies {
        parts.push("optionalDependencies");
    }
    if parts.is_empty() { "no dependency groups".to_string() } else { parts.join(", ") }
}

#[cfg(test)]
mod tests;
