//! A minimal subset of pnpm's workspace-manifest schema, used by the
//! integrated benchmark to produce a `pnpm-workspace.yaml` that pnpm
//! and pacquet both read consistently.
//!
//! The full upstream schema (see
//! `crates/npmrc/src/workspace_yaml.rs::WorkspaceSettings`, mirroring
//! pnpm/pnpm@8eb1be4988 `config/reader/src/Config.ts`) carries dozens
//! of fields. This module models only the keys the benchmark reads or
//! writes; unknown keys in a user-provided fixture are dropped on
//! deserialization (no `deny_unknown_fields`), which is acceptable
//! because the benchmark's job is to drive a known-good install, not
//! preserve arbitrary user config.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MinimalWorkspaceManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_install_peers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_scripts: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lockfile: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_architectures: Option<SupportedArchitectures>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub allow_builds: BTreeMap<String, bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SupportedArchitectures {
    pub os: Vec<String>,
    pub cpu: Vec<String>,
    pub libc: Vec<String>,
}

impl MinimalWorkspaceManifest {
    /// The default manifest the benchmark uses when no `--fixture-dir`
    /// is provided. Replaces the previous static
    /// `tasks/integrated-benchmark/src/fixtures/pnpm-workspace.yaml`
    /// text fixture.
    pub fn default_for_benchmark() -> Self {
        // Force pnpm to install every optional dependency in the
        // lockfile regardless of the runner's own os/cpu/libc, so its
        // install payload matches pacquet's (which currently installs
        // every snapshot in the lockfile unconditionally — it doesn't
        // filter optionals by platform). Without this, pnpm on Linux
        // CI skips darwin-only optionals like `fsevents` and ends up
        // doing less work than pacquet, which quietly tilts the
        // benchmark in pnpm's favour. The exact set mirrors pnpm's own
        // release matrix.
        let supported_architectures = SupportedArchitectures {
            os: ["darwin", "linux", "win32"].iter().map(|s| (*s).to_string()).collect(),
            cpu: ["x64", "arm64"].iter().map(|s| (*s).to_string()).collect(),
            libc: ["glibc", "musl"].iter().map(|s| (*s).to_string()).collect(),
        };
        // `core-js`, `es5-ext`, and `fsevents` ship native or generated
        // postinstalls that would fire by default. The benchmark's
        // `.npmrc` sets `ignore-scripts=true`, so pnpm emits
        // `ERR_PNPM_IGNORED_BUILDS` and exits 1 on the first warmup
        // run, taking the whole benchmark down. Explicitly declining
        // their builds silences the error. `fsevents` is reachable on
        // Linux too because `supportedArchitectures` above pulls it in
        // on every platform.
        let allow_builds = [("core-js", false), ("es5-ext", false), ("fsevents", false)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        Self {
            supported_architectures: Some(supported_architectures),
            allow_builds,
            ..Self::default()
        }
    }
}
