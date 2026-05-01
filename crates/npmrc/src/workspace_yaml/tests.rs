use super::*;
use pretty_assertions::assert_eq;

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
    // allow-lists, …). This guards against regressions that would make
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
    let mut npmrc = Npmrc::new();
    npmrc.lockfile = true;
    let before_registry = npmrc.registry.clone();

    settings.apply_to(&mut npmrc, Path::new("/irrelevant-for-absolute-paths"));

    assert_eq!(npmrc.store_dir.display().to_string(), "/absolute/store");
    assert!(!npmrc.lockfile);
    assert_eq!(npmrc.registry, "https://reg.example/");
    assert_ne!(before_registry, npmrc.registry);
}

#[test]
fn apply_resolves_relative_paths_against_base_dir() {
    let yaml = "storeDir: ../shared-store\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let mut npmrc = Npmrc::new();
    let base = Path::new("/workspace/root");

    settings.apply_to(&mut npmrc, base);

    // Build the expected path via the same join machinery the code
    // under test uses so the component separator matches on every
    // platform (Windows uses `\` between joined components).
    assert_eq!(npmrc.store_dir, StoreDir::from(base.join("../shared-store")));
}

/// pnpm reads `fetchRetries` / `fetchRetryFactor` /
/// `fetchRetryMintimeout` / `fetchRetryMaxtimeout` from
/// `pnpm-workspace.yaml` as camelCase keys (mirrors of the kebab-case
/// `.npmrc` form). Confirm both deserialization and `apply_to` push
/// the overrides onto the `Npmrc`, since pacquet has to honour them
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

    let mut npmrc = Npmrc::new();
    settings.apply_to(&mut npmrc, Path::new("/irrelevant"));
    assert_eq!(npmrc.fetch_retries, 5);
    assert_eq!(npmrc.fetch_retry_factor, 3);
    assert_eq!(npmrc.fetch_retry_mintimeout, 1000);
    assert_eq!(npmrc.fetch_retry_maxtimeout, 4000);
}

/// `verifyStoreIntegrity` is a camelCase key that serde's rename
/// has to pick up, and the `apply_to` wiring has to thread it onto
/// the `Npmrc` field. Parse a yaml that flips the default-true
/// setting to false and assert both steps. Guards against silent
/// regressions in the key mapping or the apply step (a copy-paste
/// omission in `apply_to` would leave `npmrc.verify_store_integrity`
/// at its default).
#[test]
fn parses_verify_store_integrity_from_yaml_and_applies() {
    let yaml = "verifyStoreIntegrity: false\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(settings.verify_store_integrity, Some(false));

    let mut npmrc = Npmrc::new();
    assert!(npmrc.verify_store_integrity, "the default is `true` to match pnpm");
    settings.apply_to(&mut npmrc, Path::new("/irrelevant"));
    assert!(!npmrc.verify_store_integrity, "yaml override wins");
}

#[test]
fn apply_leaves_unset_fields_alone() {
    let yaml = "storeDir: /s\n";
    let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    let mut npmrc = Npmrc::new();
    let before = (npmrc.hoist, npmrc.lockfile, npmrc.registry.clone(), npmrc.auto_install_peers);

    settings.apply_to(&mut npmrc, Path::new("/anywhere"));

    assert_eq!(
        (npmrc.hoist, npmrc.lockfile, npmrc.registry.clone(), npmrc.auto_install_peers),
        before
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

#[test]
fn find_returns_none_when_no_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(WorkspaceSettings::find_and_load(tmp.path()).unwrap().is_none());
}
