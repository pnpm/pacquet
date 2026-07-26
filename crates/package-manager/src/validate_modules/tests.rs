//! Unit tests for [`super::validate_modules`].
//!
//! Each test exercises one drift axis in isolation. The strategy is
//! to seed `Modules` with a known shape, build a [`Config`] that
//! matches it, then mutate one field on the config (or the modules
//! manifest) and assert the matching error variant.

use super::{ValidateModulesError, validate_modules};
use pacquet_config::Config;
use pacquet_modules_yaml::{
    DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH, IncludedDependencies, LayoutVersion, Modules,
    NodeLinker as ModulesNodeLinker,
};
use pacquet_store_dir::StoreDir;
use pretty_assertions::assert_eq;
use std::{collections::BTreeMap, path::Path};

/// Seed a `Modules` value that matches a fresh `Config::new()` —
/// the round-trip baseline for every drift test.
fn baseline_modules(config: &Config) -> Modules {
    Modules {
        hoist_pattern: config.hoist_pattern.clone(),
        public_hoist_pattern: config.public_hoist_pattern.clone(),
        included: IncludedDependencies {
            dependencies: true,
            dev_dependencies: true,
            optional_dependencies: true,
        },
        layout_version: Some(LayoutVersion),
        node_linker: Some(ModulesNodeLinker::Isolated),
        store_dir: config.store_dir.display().to_string(),
        virtual_store_dir: config.virtual_store_dir.to_string_lossy().into_owned(),
        virtual_store_dir_max_length: DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH,
        registries: Some(BTreeMap::from([("default".to_string(), config.registry.clone())])),
        ..Default::default()
    }
}

fn requested_full_groups() -> IncludedDependencies {
    IncludedDependencies { dependencies: true, dev_dependencies: true, optional_dependencies: true }
}

/// Sanity: a `Modules` written by the same `Config` validates clean.
/// Catches the case where some axis check spuriously fires on a
/// matching baseline (regression risk if `patterns_equal` ever
/// over-tightens, etc.).
#[test]
fn baseline_round_trip_validates_clean() {
    let config = Config::new();
    let modules = baseline_modules(&config);
    validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect("baseline must validate clean");
}

/// Recorded `hoistPattern` differs from the current install's
/// `Config.hoist_pattern`. Typical user case: yaml flipped from
/// `['*']` to `['only-foo']` between two installs.
#[test]
fn hoist_pattern_diff_returns_error() {
    let config = Config::new();
    let mut modules = baseline_modules(&config);
    modules.hoist_pattern = Some(vec!["only-foo".to_string()]);
    let err = validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect_err("differing hoist_pattern must error");
    assert!(matches!(err, ValidateModulesError::HoistPatternDiff), "got {err:?}");
}

/// `publicHoistPattern` drift. Same shape as the private side.
#[test]
fn public_hoist_pattern_diff_returns_error() {
    let config = Config::new();
    let mut modules = baseline_modules(&config);
    modules.public_hoist_pattern = Some(vec!["different".to_string()]);
    let err = validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect_err("differing public_hoist_pattern must error");
    assert!(matches!(err, ValidateModulesError::PublicHoistPatternDiff), "got {err:?}");
}

/// `None` and `Some([])` mean the same thing ("no patterns") and
/// must compare equal — mirrors upstream's
/// `equals(modules.publicHoistPattern ?? [], opts.publicHoistPattern ?? [])`
/// where the `?? []` makes both nullish values look like empty
/// arrays.
#[test]
fn none_vs_some_empty_pattern_treated_as_equal() {
    let mut config = Config::new();
    config.hoist_pattern = None;
    config.public_hoist_pattern = None;
    let mut modules = baseline_modules(&config);
    modules.hoist_pattern = Some(Vec::new());
    modules.public_hoist_pattern = Some(Vec::new());
    validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/anywhere"),
        Path::new("/anywhere/node_modules"),
    )
    .expect("None and Some([]) compare equal");
}

/// `included` drift: install with all groups, then re-install with
/// `--prod` (`dev_dependencies: false`).
#[test]
fn included_deps_conflict_returns_error_with_payload() {
    let config = Config::new();
    let modules = baseline_modules(&config);
    let prod_only = IncludedDependencies {
        dependencies: true,
        dev_dependencies: false,
        optional_dependencies: false,
    };
    let err = validate_modules(
        &modules,
        &config,
        prod_only,
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect_err("differing included must error");
    let ValidateModulesError::IncludedDepsConflict { lockfile_dir, recorded, requested } = err
    else {
        panic!("expected IncludedDepsConflict, got {err:?}");
    };
    assert_eq!(lockfile_dir, Path::new("/lockfile-dir"));
    // Recorded baseline includes all three groups; requested is prod-only.
    assert_eq!(recorded, "dependencies, devDependencies, optionalDependencies");
    assert_eq!(requested, "dependencies");
}

/// `storeDir` drift. The recorded path on disk is absolutized
/// strings (modules-yaml resolves them on load), so a string-level
/// equality is enough.
#[test]
fn store_dir_drift_returns_unexpected_store() {
    let config = Config::new();
    let mut modules = baseline_modules(&config);
    modules.store_dir = "/some-other-store".to_string();
    let err = validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect_err("store_dir drift must error");
    let ValidateModulesError::UnexpectedStore { recorded, requested, .. } = err else {
        panic!("expected UnexpectedStore, got {err:?}");
    };
    assert_eq!(recorded, "/some-other-store");
    assert_eq!(requested, config.store_dir.display().to_string());
}

/// `virtualStoreDir` drift. Less common in practice (most users
/// don't pin `virtualStoreDir` explicitly), but still a real drift.
#[test]
fn virtual_store_dir_drift_returns_unexpected_virtual_store_dir() {
    let config = Config::new();
    let mut modules = baseline_modules(&config);
    modules.virtual_store_dir = "/some-other-vs".to_string();
    let err = validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect_err("virtual_store_dir drift must error");
    assert!(matches!(err, ValidateModulesError::UnexpectedVirtualStoreDir { .. }), "got {err:?}");
}

/// `virtualStoreDirMaxLength` drift. Pacquet pins the default 120
/// today, but a yaml override would cause this.
#[test]
fn virtual_store_dir_max_length_drift_returns_error() {
    let config = Config::new();
    let mut modules = baseline_modules(&config);
    modules.virtual_store_dir_max_length = 200;
    let err = validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect_err("virtual_store_dir_max_length drift must error");
    assert!(matches!(err, ValidateModulesError::VirtualStoreDirMaxLengthDiff), "got {err:?}");
}

/// Path-equality is component-wise — a recorded path with a trailing
/// slash compares equal to one without (Rust's `Path::eq` ignores
/// the trailing separator).
#[test]
fn store_dir_path_equality_ignores_trailing_slash() {
    let mut config = Config::new();
    config.store_dir = StoreDir::from(Path::new("/store").to_path_buf());
    let mut modules = baseline_modules(&config);
    modules.store_dir = "/store/".to_string();
    validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect("trailing-slash diff must compare equal via Path::eq");
}

/// Upstream order: `virtualStoreDirMaxLength` is checked first, then
/// `publicHoistPattern`, then `hoistPattern`, then `checkCompatibility`,
/// then `included`. When multiple axes drift, the first one in
/// upstream's order surfaces. This test has both
/// `virtualStoreDirMaxLength` and `hoistPattern` drift — the
/// max-length error must win.
#[test]
fn first_drift_in_upstream_order_wins() {
    let config = Config::new();
    let mut modules = baseline_modules(&config);
    modules.virtual_store_dir_max_length = 200;
    modules.hoist_pattern = Some(vec!["only-foo".to_string()]);
    let err = validate_modules(
        &modules,
        &config,
        requested_full_groups(),
        Path::new("/lockfile-dir"),
        Path::new("/lockfile-dir/node_modules"),
    )
    .expect_err("must error");
    assert!(
        matches!(err, ValidateModulesError::VirtualStoreDirMaxLengthDiff),
        "max-length must surface first, got {err:?}",
    );
}
