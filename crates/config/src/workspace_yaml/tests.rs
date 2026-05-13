use super::{LoadWorkspaceYamlError, WORKSPACE_MANIFEST_FILENAME, WorkspaceSettings};
use crate::{Config, NodeLinker, ScriptsPrependNodePath};
use pacquet_store_dir::StoreDir;
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use std::{fs, path::Path};

#[test]
fn parses_common_settings_from_yaml() {
    let yaml = r#"
storeDir: ../my-store
registry: https://reg.example
lockfile: false
autoInstallPeers: true
nodeLinker: hoisted
packages:
  - packages/*
"#;
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.store_dir.as_deref(), Some("../my-store"));
    assert_eq!(settings.registry.as_deref(), Some("https://reg.example"));
    assert_eq!(settings.lockfile, Some(false));
    assert_eq!(settings.auto_install_peers, Some(true));
    assert!(matches!(settings.node_linker, Some(NodeLinker::Hoisted)));
}

#[test]
fn swallows_unknown_top_level_keys() {
    let yaml = r#"
catalog:
  react: ^18
onlyBuiltDependencies:
  - esbuild
packages:
  - "apps/*"
"#;
    // `pnpm-workspace.yaml` commonly contains top-level keys we do not
    // model in `WorkspaceSettings` (packages list, catalogs, build
    // allow-lists, ...). This guards against regressions that would make
    // serde reject those unknown keys during deserialization — i.e.
    // someone adding `deny_unknown_fields` to the struct.
    let _settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
}

#[test]
fn apply_overrides_npmrc_defaults() {
    let yaml = r#"
storeDir: /absolute/store
lockfile: false
registry: https://reg.example
"#;
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let mut config = Config::new();
    config.lockfile = true;
    let before_registry = config.registry.clone();

    settings.apply_to(&mut config, Path::new("/irrelevant-for-absolute-paths"));

    assert_eq!(config.store_dir, StoreDir::from(Path::new("/absolute/store").to_path_buf()));
    assert!(!config.lockfile);
    assert_eq!(config.registry, "https://reg.example/");
    assert_ne!(before_registry, config.registry);
}

#[test]
fn apply_resolves_relative_paths_against_base_dir() {
    let yaml = "storeDir: ../shared-store\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let mut config = Config::new();
    let base = Path::new("/workspace/root");

    settings.apply_to(&mut config, base);

    // Build the expected path via the same join machinery the code
    // under test uses so the component separator matches on every
    // platform (Windows uses `\` between joined components).
    assert_eq!(config.store_dir, StoreDir::from(base.join("../shared-store")));
}

/// pnpm reads `fetchRetries` / `fetchRetryFactor` /
/// `fetchRetryMintimeout` / `fetchRetryMaxtimeout` from
/// `pnpm-workspace.yaml` as camelCase keys (mirrors of the kebab-case
/// `.npmrc` form). Confirm both deserialization and `apply_to` push
/// the overrides onto the `Config`, since pacquet has to honour them
/// for parity with pnpm and for the install-time retry plumbing in
/// crates/tarball.
#[test]
fn parses_fetch_retry_settings_from_yaml_and_applies() {
    let yaml = r#"
fetchRetries: 5
fetchRetryFactor: 3
fetchRetryMintimeout: 1000
fetchRetryMaxtimeout: 4000
"#;
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.fetch_retries, Some(5));
    assert_eq!(settings.fetch_retry_factor, Some(3));
    assert_eq!(settings.fetch_retry_mintimeout, Some(1000));
    assert_eq!(settings.fetch_retry_maxtimeout, Some(4000));

    let mut config = Config::new();
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert_eq!(config.fetch_retries, 5);
    assert_eq!(config.fetch_retry_factor, 3);
    assert_eq!(config.fetch_retry_mintimeout, 1000);
    assert_eq!(config.fetch_retry_maxtimeout, 4000);
}

/// `verifyStoreIntegrity` is a camelCase key that serde's rename
/// has to pick up, and the `apply_to` wiring has to thread it onto
/// the `Config` field. Parse a yaml that flips the default-true
/// setting to false and assert both steps. Guards against silent
/// regressions in the key mapping or the apply step (a copy-paste
/// omission in `apply_to` would leave `config.verify_store_integrity`
/// at its default).
#[test]
fn parses_verify_store_integrity_from_yaml_and_applies() {
    let yaml = "verifyStoreIntegrity: false\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.verify_store_integrity, Some(false));

    let mut config = Config::new();
    assert!(config.verify_store_integrity, "the default is `true` to match pnpm");
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert!(!config.verify_store_integrity, "yaml override wins");
}

/// `sideEffectsCache` is the side-effects cache READ-path knob from
/// pnpm-workspace.yaml. Same shape as `verifyStoreIntegrity`:
/// camelCase rename + `apply_to` wiring. Parsing a yaml that flips
/// the default-true setting to false must end up at
/// `config.side_effects_cache == false`.
#[test]
fn parses_side_effects_cache_from_yaml_and_applies() {
    let yaml = "sideEffectsCache: false\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.side_effects_cache, Some(false));

    let mut config = Config::new();
    assert!(config.side_effects_cache, "the default is `true` to match pnpm");
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert!(!config.side_effects_cache, "yaml override wins");
}

/// `sideEffectsCacheReadonly` is pnpm's read-only flag for the
/// side-effects cache. Same camelCase + `apply_to` wiring as
/// `sideEffectsCache`. Default is `false`, so flipping it on via
/// yaml must end at `config.side_effects_cache_readonly == true`.
#[test]
fn parses_side_effects_cache_readonly_from_yaml_and_applies() {
    let yaml = "sideEffectsCacheReadonly: true\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.side_effects_cache_readonly, Some(true));

    let mut config = Config::new();
    assert!(!config.side_effects_cache_readonly, "the default is `false`");
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert!(config.side_effects_cache_readonly, "yaml override wins");
}

/// READ / WRITE gate helpers must combine the two knobs the way
/// upstream's [`config/reader/src/index.ts`](https://github.com/pnpm/pnpm/blob/7e3145f9fc/config/reader/src/index.ts#L614-L615)
/// does for the canonical state combinations:
///
/// - default (`cache=true`, `readonly=false`)  → read=on, write=on
/// - cache off  (`cache=false`, `readonly=false`) → read=off, write=off
/// - readonly on (`cache=true`, `readonly=true`)  → read=on, write=off
/// - cache off + readonly on                      → read=on, write=off
#[test]
fn side_effects_cache_gates_truth_table() {
    let mut config = Config::new();
    assert!(config.side_effects_cache_read());
    assert!(config.side_effects_cache_write());

    config.side_effects_cache = false;
    config.side_effects_cache_readonly = false;
    assert!(!config.side_effects_cache_read());
    assert!(!config.side_effects_cache_write());

    config.side_effects_cache = true;
    config.side_effects_cache_readonly = true;
    assert!(config.side_effects_cache_read());
    assert!(!config.side_effects_cache_write());

    config.side_effects_cache = false;
    config.side_effects_cache_readonly = true;
    assert!(config.side_effects_cache_read());
    assert!(!config.side_effects_cache_write());
}

/// `patchedDependencies` in `pnpm-workspace.yaml` is a string→string
/// map where keys carry an optional `@version` suffix and values are
/// patch-file paths. pacquet captures it raw on `WorkspaceSettings`;
/// path resolution + hashing + grouping happen at install time via
/// `Config::resolved_patched_dependencies` (which delegates to
/// `pacquet_patching::resolve_and_group`). This test guards the
/// deserialization shape only — the camelCase rename, optionality,
/// and value-as-string-path.
#[test]
fn parses_patched_dependencies_from_yaml() {
    let yaml = r#"
patchedDependencies:
  "lodash@4.17.21": patches/lodash@4.17.21.patch
  "foo@^1.0.0": patches/foo.patch
  bar: patches/bar.patch
"#;
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let map = settings.patched_dependencies.expect("field present");
    assert_eq!(map.get("lodash@4.17.21").map(String::as_str), Some("patches/lodash@4.17.21.patch"));
    assert_eq!(map.get("foo@^1.0.0").map(String::as_str), Some("patches/foo.patch"));
    assert_eq!(map.get("bar").map(String::as_str), Some("patches/bar.patch"));
}

#[test]
fn patched_dependencies_absent_yields_none() {
    let yaml = "storeDir: /s\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert!(settings.patched_dependencies.is_none());
}

/// `apply_to` records the workspace dir on `Config.workspace_dir`
/// (needed by `Config::resolved_patched_dependencies` so patch
/// file paths resolve against the same dir as upstream) and pushes
/// the raw map verbatim.
#[test]
fn apply_pushes_patched_dependencies_and_workspace_dir() {
    let yaml = r#"
patchedDependencies:
  "lodash@4.17.21": patches/lodash@4.17.21.patch
"#;
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let mut config = Config::new();
    let base = Path::new("/workspace/root");
    settings.apply_to(&mut config, base);

    assert_eq!(config.workspace_dir.as_deref(), Some(base));
    let map = config.patched_dependencies.expect("present");
    assert_eq!(map.get("lodash@4.17.21").map(String::as_str), Some("patches/lodash@4.17.21.patch"));
}

/// `allowBuilds` is a map of `name[@version]` → bool. Same camelCase
/// rename + `apply_to` wiring as the other yaml-sourced settings.
/// pnpm 10+ moved this out of `package.json#pnpm` (matches
/// pnpm/pacquet#397 item 5).
#[test]
fn parses_allow_builds_from_yaml_and_applies() {
    let yaml = r#"
allowBuilds:
  esbuild: true
  "foo@1.0.0": true
  bar: false
"#;
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let raw = settings.allow_builds.clone().expect("field present");
    assert_eq!(raw.get("esbuild").copied(), Some(true));
    assert_eq!(raw.get("foo@1.0.0").copied(), Some(true));
    assert_eq!(raw.get("bar").copied(), Some(false));

    let mut config = Config::new();
    assert!(config.allow_builds.is_empty(), "default is empty");
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert_eq!(config.allow_builds.get("esbuild").copied(), Some(true));
}

/// `dangerouslyAllowAllBuilds` is a single boolean — default `false`
/// to match pnpm 11.
#[test]
fn parses_dangerously_allow_all_builds_from_yaml_and_applies() {
    let yaml = "dangerouslyAllowAllBuilds: true\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.dangerously_allow_all_builds, Some(true));

    let mut config = Config::new();
    assert!(!config.dangerously_allow_all_builds, "default is false");
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert!(config.dangerously_allow_all_builds);
}

/// `scriptsPrependNodePath` is the tri-state from upstream
/// [`Config.scriptsPrependNodePath: boolean | 'warn-only'`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/Config.ts#L108).
/// `true` → Always, `false` → Never, `"warn-only"` → WarnOnly.
/// Pacquet's default is Never (matches upstream's
/// [`StrictBuildOptions.scriptsPrependNodePath: false`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/after-install/src/extendBuildOptions.ts#L78)).
#[test]
fn parses_scripts_prepend_node_path_true_from_yaml() {
    let yaml = "scriptsPrependNodePath: true\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.scripts_prepend_node_path, Some(ScriptsPrependNodePath::Always));

    let mut config = Config::new();
    assert_eq!(config.scripts_prepend_node_path, ScriptsPrependNodePath::Never, "default Never");
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert_eq!(config.scripts_prepend_node_path, ScriptsPrependNodePath::Always);
}

#[test]
fn parses_scripts_prepend_node_path_false_from_yaml() {
    let yaml = "scriptsPrependNodePath: false\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.scripts_prepend_node_path, Some(ScriptsPrependNodePath::Never));
}

#[test]
fn parses_scripts_prepend_node_path_warn_only_from_yaml() {
    let yaml = "scriptsPrependNodePath: warn-only\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.scripts_prepend_node_path, Some(ScriptsPrependNodePath::WarnOnly));
}

#[test]
fn rejects_invalid_scripts_prepend_node_path() {
    let yaml = "scriptsPrependNodePath: nonsense\n";
    serde_saphyr::from_str::<WorkspaceSettings>(yaml).expect_err("must reject");
}

/// `unsafePerm: false` from yaml propagates to `Config.unsafe_perm`
/// on POSIX. Mirrors upstream's [`Config.unsafePerm: boolean`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/Config.ts).
/// The starting `Config::new()` value depends on the runtime uid
/// (see [`default_unsafe_perm`]) — `true` for non-root, `false`
/// for root. Either way, `apply_to` with `Some(false)` ends in
/// `false`.
#[test]
fn parses_unsafe_perm_from_yaml_and_applies() {
    // POSIX-only: the Windows force-override below would mask this
    // test's behavior. See [`WorkspaceSettings::apply_to`].
    if cfg!(windows) {
        return;
    }
    let yaml = "unsafePerm: false\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.unsafe_perm, Some(false));

    let mut config = Config::new();
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert!(!config.unsafe_perm, "yaml override wins on POSIX");
}

/// On Windows, `apply_to` ignores the yaml value and forces
/// `unsafe_perm = true`. Mirrors upstream's
/// [`process.platform === 'win32'` override](https://github.com/pnpm/npm-lifecycle/blob/d2d8e790/index.js#L204-L220)
/// — running lifecycle scripts under a uid/gid drop is POSIX-only.
#[cfg(windows)]
#[test]
fn unsafe_perm_force_true_on_windows() {
    let yaml = "unsafePerm: false\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let mut config = Config::new();
    settings.apply_to(&mut config, Path::new("C:/irrelevant"));
    assert!(config.unsafe_perm, "Windows forces unsafe_perm true regardless of yaml");
}

/// A positive `childConcurrency` is taken verbatim — mirrors
/// upstream's [`getWorkspaceConcurrency`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.ts#L25-L34).
#[test]
fn parses_positive_child_concurrency_from_yaml_and_applies() {
    let yaml = "childConcurrency: 8\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.child_concurrency, Some(8));

    let mut config = Config::new();
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    assert_eq!(config.child_concurrency, 8);
}

/// A non-positive `childConcurrency` is interpreted as
/// `max(1, parallelism - |value|)`. The exact result depends on
/// the host's reported parallelism, so we just bound-check it:
/// negative offsets must produce at least 1 and at most
/// `parallelism()`.
#[test]
fn parses_negative_child_concurrency_from_yaml_and_resolves() {
    let yaml = "childConcurrency: -1\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.child_concurrency, Some(-1));

    let mut config = Config::new();
    settings.apply_to(&mut config, Path::new("/irrelevant"));
    let parallelism = crate::available_parallelism();
    assert!(config.child_concurrency >= 1, "must floor at 1");
    assert!(config.child_concurrency <= parallelism, "must not exceed available parallelism");
}

#[test]
fn apply_leaves_unset_fields_alone() {
    let yaml = "storeDir: /s\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let mut config = Config::new();
    let before =
        (config.hoist, config.lockfile, config.registry.clone(), config.auto_install_peers);

    settings.apply_to(&mut config, Path::new("/anywhere"));

    assert_eq!(
        (config.hoist, config.lockfile, config.registry.clone(), config.auto_install_peers),
        before,
    );
}

#[test]
fn find_walks_up_to_parent_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let nested = tmp.path().join("a/b/c");
    fs::create_dir_all(&nested).unwrap();
    fs::write(tmp.path().join("pnpm-workspace.yaml"), "storeDir: /s\n").unwrap();

    let (found, settings) = WorkspaceSettings::find_and_load(&nested).unwrap().unwrap();
    assert_eq!(found, tmp.path().join("pnpm-workspace.yaml"));
    assert_eq!(settings.store_dir.as_deref(), Some("/s"));
}

/// Pnpm's `readManifestRaw` only treats `ENOENT` as "no manifest" and
/// propagates every other failure. A directory entry named
/// `pnpm-workspace.yaml` is not a missing file, so `find_and_load`
/// must surface it as `ReadFile` rather than silently walking up.
#[test]
fn find_propagates_when_manifest_path_is_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    tmp.path().join(WORKSPACE_MANIFEST_FILENAME).pipe(fs::create_dir).unwrap();

    let err = tmp
        .path()
        .pipe_as_ref(WorkspaceSettings::find_and_load)
        .expect_err("a directory at the manifest path is not a missing file");
    assert!(
        matches!(err, LoadWorkspaceYamlError::ReadFile { .. }),
        "expected ReadFile, got {err:?}",
    );

    drop(tmp); // clean up
}

#[test]
fn find_returns_none_when_no_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(WorkspaceSettings::find_and_load(tmp.path()).unwrap().is_none());
}

#[test]
fn apply_replaces_git_shallow_hosts_defaults() {
    // pnpm replaces the built-in default array wholesale rather than
    // merging it, so we mirror that. See `default_git_shallow_hosts`.
    let yaml = r#"
gitShallowHosts:
  - corp-git.example.com
"#;
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let mut config = Config::new();

    // Sanity-check the default before applying — `github.com` is the
    // first entry in pnpm's list, and replacement (not merging) is the
    // bit we want to verify.
    assert!(config.git_shallow_hosts.iter().any(|h| h == "github.com"));

    settings.apply_to(&mut config, Path::new("/irrelevant"));

    assert_eq!(config.git_shallow_hosts, vec!["corp-git.example.com".to_string()]);
}
