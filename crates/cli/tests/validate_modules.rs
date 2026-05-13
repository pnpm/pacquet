//! End-to-end coverage for `.modules.yaml` validation on re-install.
//!
//! Each test runs `pacquet install --frozen-lockfile` once to write
//! `.modules.yaml`, then mutates `pnpm-workspace.yaml` and re-runs
//! the install. The second install must error with a typed
//! `ValidateModulesError` variant rather than silently rebuilding
//! the layout under the new settings.
//!
//! Mirrors the scenarios at upstream's
//! [`installing/deps-installer/test/install/hoist.ts:209`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/test/install/hoist.ts#L209)
//! and [`:220`](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/test/install/hoist.ts#L220),
//! both of which were stubbed as `known_failures` against
//! pnpm/pacquet#433 in the hoist PR — they're actually blocked on
//! *this* validation, not partial install. Now that #464 §A landed,
//! they pass.

#![cfg(unix)] // pnpm CLI: 'program not found' on Windows runners.

pub mod _utils;
pub use _utils::*;

use assert_cmd::prelude::*;
use command_extra::CommandExtra;
use pacquet_testing_utils::bin::{AddMockedRegistry, CommandTempCwd};
use std::{fs, path::Path, process::Command};

fn generate_lockfile(pnpm: Command) {
    pnpm.with_args(["install", "--lockfile-only", "--ignore-scripts"]).assert().success();
}

fn write_workspace_yaml(workspace: &Path, extra: &str) {
    let yaml = format!("storeDir: ../pacquet-store\ncacheDir: ../pacquet-cache\n{extra}");
    fs::write(workspace.join("pnpm-workspace.yaml"), yaml).expect("write pnpm-workspace.yaml");
}

fn write_manifest(workspace: &Path, deps: serde_json::Value) {
    let manifest = serde_json::json!({ "dependencies": deps });
    fs::write(workspace.join("package.json"), manifest.to_string()).expect("write package.json");
}

/// First install with `hoistPattern: ['*']` (the default), second
/// install with `hoistPattern: ['only-this-thing']`. The second
/// install must error with `HOIST_PATTERN_DIFF` rather than
/// silently leaving the old hoist symlinks in place.
///
/// Mirrors upstream's
/// [`hoist.ts:209` "hoistPattern=* throws exception when executed on node_modules installed w/o the option"](https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/test/install/hoist.ts#L209)
/// — that test exercises the same drift in the opposite direction
/// (install without hoist, then install with `*`); the error
/// shape is symmetric.
#[test]
fn re_install_with_changed_hoist_pattern_errors() {
    let CommandTempCwd { pacquet: pacquet_first, pnpm, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    write_manifest(
        &workspace,
        serde_json::json!({ "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0" }),
    );
    generate_lockfile(pnpm);

    // First install — default hoist pattern (`['*']`).
    pacquet_first.with_args(["install", "--frozen-lockfile"]).assert().success();
    assert!(
        workspace.join("node_modules/.modules.yaml").exists(),
        "first install must write .modules.yaml",
    );

    // Re-install with a different hoist pattern via yaml override.
    write_workspace_yaml(&workspace, "hoistPattern:\n  - 'only-this-thing'\n");
    let pacquet_second = std::process::Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(&workspace);
    let output =
        pacquet_second.with_args(["install", "--frozen-lockfile"]).output().expect("run pacquet");
    assert!(
        !output.status.success(),
        "re-install with changed hoistPattern must fail; stderr=\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("hoist-pattern") || stderr.contains("hoist_pattern"),
        "error message must mention hoist-pattern; got:\n{stderr}",
    );

    drop((root, mock_instance));
}

/// First install with `publicHoistPattern: ['*']`, second install
/// with `publicHoistPattern: []`. The second install must error
/// with `PUBLIC_HOIST_PATTERN_DIFF`.
#[test]
fn re_install_with_changed_public_hoist_pattern_errors() {
    let CommandTempCwd { pacquet: pacquet_first, pnpm, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    write_manifest(
        &workspace,
        serde_json::json!({ "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0" }),
    );
    generate_lockfile(pnpm);
    write_workspace_yaml(&workspace, "publicHoistPattern:\n  - '*'\nhoistPattern: []\n");

    pacquet_first.with_args(["install", "--frozen-lockfile"]).assert().success();

    write_workspace_yaml(&workspace, "publicHoistPattern: []\nhoistPattern: []\n");
    let pacquet_second = std::process::Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(&workspace);
    let output =
        pacquet_second.with_args(["install", "--frozen-lockfile"]).output().expect("run pacquet");
    assert!(
        !output.status.success(),
        "re-install with changed publicHoistPattern must fail; stderr=\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("public-hoist-pattern") || stderr.contains("public_hoist_pattern"),
        "error must mention public-hoist-pattern; got:\n{stderr}",
    );

    drop((root, mock_instance));
}

/// Re-installing with the same effective layout (no yaml change)
/// must NOT error — only drift triggers the validation. Guards
/// against the validator firing on every re-install when nothing
/// changed.
#[test]
fn re_install_with_no_change_succeeds() {
    let CommandTempCwd { pacquet: pacquet_first, pnpm, root, workspace, npmrc_info, .. } =
        CommandTempCwd::init().add_mocked_registry();
    let AddMockedRegistry { mock_instance, .. } = npmrc_info;

    write_manifest(
        &workspace,
        serde_json::json!({ "@pnpm.e2e/hello-world-js-bin-parent": "1.0.0" }),
    );
    generate_lockfile(pnpm);

    pacquet_first.with_args(["install", "--frozen-lockfile"]).assert().success();

    let pacquet_second = std::process::Command::cargo_bin("pacquet")
        .expect("find the pacquet binary")
        .with_current_dir(&workspace);
    pacquet_second.with_args(["install", "--frozen-lockfile"]).assert().success();

    drop((root, mock_instance));
}
