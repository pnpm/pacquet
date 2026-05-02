use super::NpmrcAuth;
use crate::Npmrc;
use pretty_assertions::assert_eq;

#[test]
fn picks_up_registry_and_normalises_trailing_slash() {
    let ini = "registry=https://r.example\n";
    let auth = NpmrcAuth::from_ini(ini);
    assert_eq!(auth.registry.as_deref(), Some("https://r.example"));

    let mut npmrc = Npmrc::new();
    auth.apply_to(&mut npmrc);
    assert_eq!(npmrc.registry, "https://r.example/");
}

#[test]
fn preserves_existing_trailing_slash() {
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini("registry=https://r.example/\n").apply_to(&mut npmrc);
    assert_eq!(npmrc.registry, "https://r.example/");
}

#[test]
fn ignores_non_auth_keys() {
    // These are all project-structural settings that pnpm 11 only reads
    // from pnpm-workspace.yaml now. Writing them to .npmrc should be a
    // no-op.
    //
    // `Npmrc::new()` reads `PNPM_HOME` / `XDG_DATA_HOME` to compute
    // `store_dir`, and the env-mutating tests in `custom_deserializer`
    // toggle those vars under `EnvGuard`. Hold the same lock so a
    // parallel test can't change the env between the two `Npmrc::new()`
    // snapshots compared below. Proper fix is dependency injection —
    // see the TODO on `default_store_dir`.
    let _g = crate::test_env_guard::EnvGuard::snapshot(["PNPM_HOME", "XDG_DATA_HOME"]);
    let ini = "
store-dir=/should/not/apply
lockfile=false
hoist=false
node-linker=hoisted
";
    let npmrc_before = Npmrc::new();
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini(ini).apply_to(&mut npmrc);
    assert_eq!(npmrc.store_dir, npmrc_before.store_dir);
    assert_eq!(npmrc.lockfile, npmrc_before.lockfile);
    assert_eq!(npmrc.hoist, npmrc_before.hoist);
    assert_eq!(npmrc.node_linker, npmrc_before.node_linker);
}

#[test]
fn ignores_comments_and_empty_lines() {
    let ini = "
# this is a comment
; another comment

registry=https://r.example
# trailing comment
";
    let auth = NpmrcAuth::from_ini(ini);
    assert_eq!(auth.registry.as_deref(), Some("https://r.example"));
}

#[test]
fn ignores_malformed_lines() {
    let ini = "not_a_key_value\nregistry=https://r.example\n=orphan_equals\n";
    let auth = NpmrcAuth::from_ini(ini);
    assert_eq!(auth.registry.as_deref(), Some("https://r.example"));
}
