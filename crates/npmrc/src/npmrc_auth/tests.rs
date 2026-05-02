use super::{EnvVar, NpmrcAuth, RawCreds, base64_decode, base64_encode};
use crate::Npmrc;
use pretty_assertions::assert_eq;

/// Test fake: the process environment is empty. Per the DI
/// pattern from
/// [pnpm/pacquet#339](https://github.com/pnpm/pacquet/issues/339),
/// the fake is a unit struct scoped to the test module; tests
/// turbofish it through the generic slot.
struct NoEnv;
impl EnvVar for NoEnv {
    fn var(_: &str) -> Option<String> {
        None
    }
}

#[test]
fn picks_up_registry_and_normalises_trailing_slash() {
    let ini = "registry=https://r.example\n";
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert_eq!(auth.registry.as_deref(), Some("https://r.example"));

    let mut npmrc = Npmrc::new();
    auth.apply_to(&mut npmrc);
    assert_eq!(npmrc.registry, "https://r.example/");
}

#[test]
fn preserves_existing_trailing_slash() {
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini::<NoEnv>("registry=https://r.example/\n").apply_to(&mut npmrc);
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
    NpmrcAuth::from_ini::<NoEnv>(ini).apply_to(&mut npmrc);
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
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert_eq!(auth.registry.as_deref(), Some("https://r.example"));
}

#[test]
fn ignores_malformed_lines() {
    let ini = "not_a_key_value\nregistry=https://r.example\n=orphan_equals\n";
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert_eq!(auth.registry.as_deref(), Some("https://r.example"));
}

#[test]
fn parses_per_registry_auth_token() {
    let ini = "//npm.pkg.github.com/pnpm/:_authToken=ghp_xxx\n";
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert_eq!(
        auth.creds_by_uri
            .get("//npm.pkg.github.com/pnpm/")
            .map(|creds| creds.auth_token.as_deref()),
        Some(Some("ghp_xxx")),
    );
}

#[test]
fn parses_default_auth_token_and_keys_to_registry() {
    let ini = "_authToken=top-secret\n";
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert_eq!(auth.default_creds.auth_token.as_deref(), Some("top-secret"));

    let mut npmrc = Npmrc::new();
    auth.apply_to(&mut npmrc);
    assert_eq!(
        npmrc.auth_headers.for_url("https://registry.npmjs.org/foo/-/foo-1.0.0.tgz").as_deref(),
        Some("Bearer top-secret"),
    );
}

#[test]
fn env_replace_substitutes_token() {
    struct EnvWithToken;
    impl EnvVar for EnvWithToken {
        fn var(name: &str) -> Option<String> {
            (name == "TOKEN").then(|| "abc123".to_owned())
        }
    }
    let ini = "//reg.com/:_authToken=${TOKEN}\n";
    let auth = NpmrcAuth::from_ini::<EnvWithToken>(ini);
    assert_eq!(
        auth.creds_by_uri.get("//reg.com/").map(|creds| creds.auth_token.as_deref()),
        Some(Some("abc123")),
    );
}

#[test]
fn env_replace_failure_warns_and_keeps_raw_value() {
    let ini = "//reg.com/:_authToken=${MISSING}\n";
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert_eq!(
        auth.creds_by_uri.get("//reg.com/").map(|creds| creds.auth_token.as_deref()),
        Some(Some("${MISSING}")),
    );
    assert_eq!(auth.warnings.len(), 1);
    assert!(auth.warnings[0].contains("${MISSING}"));
}

#[test]
fn basic_auth_built_from_username_and_password() {
    // Pnpm's `_password` is base64(raw_password). Header should
    // be `Basic base64(username:raw_password)`.
    let raw_password = "p@ss";
    let password_b64 = base64_encode(raw_password);
    let ini = format!("//reg.com/:username=alice\n//reg.com/:_password={password_b64}\n",);
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini::<NoEnv>(&ini).apply_to(&mut npmrc);
    assert_eq!(
        npmrc.auth_headers.for_url("https://reg.com/").as_deref(),
        Some(format!("Basic {}", base64_encode("alice:p@ss")).as_str()),
    );
}

#[test]
fn auth_pair_base64_passes_through_to_basic_header() {
    let pair = base64_encode("alice:p@ss");
    let ini = format!("//reg.com/:_auth={pair}\n");
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini::<NoEnv>(&ini).apply_to(&mut npmrc);
    assert_eq!(
        npmrc.auth_headers.for_url("https://reg.com/").as_deref(),
        Some(format!("Basic {pair}").as_str()),
    );
}

/// `[section]`-style headers are not legal `.npmrc` syntax (npm's
/// rc files are flat key/value pairs). Smoke-test that they are
/// dropped silently. They fall through the no-`=` branch in
/// [`NpmrcAuth::from_ini`] so the parser never tries to interpret
/// them.
#[test]
fn ini_section_headers_are_dropped_silently() {
    let ini = "[default]\nregistry=https://r.example\n[other]\n";
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert_eq!(auth.registry.as_deref(), Some("https://r.example"));
    assert_eq!(auth.warnings, Vec::<String>::new());
}

/// When a `${VAR}` placeholder appears in the *key* and cannot be
/// resolved, the parser keeps the raw key verbatim and pushes a
/// warning. Mirrors `substituteEnv` in pnpm's `loadNpmrcFiles.ts`.
#[test]
fn env_replace_failure_on_key_warns_and_keeps_raw_key() {
    // `${MISSING}_authToken` resolves to a literal key, so it lands
    // in `default_creds` rather than being recognised as the typed
    // `_authToken` field. The point of this test is to exercise the
    // warning + raw-key branch at the top of `from_ini`.
    let ini = "${MISSING}_authToken=abc\n";
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert!(auth.warnings.iter().any(|warning| warning.contains("${MISSING}")));
}

/// Top-level `_auth=`, `username=`, and `_password=` lines should
/// land on [`NpmrcAuth::default_creds`] so the resolved registry's
/// nerf-darted URI gets a `Basic` header.
#[test]
fn top_level_auth_pair_keys_to_default_registry_basic_header() {
    let pair = base64_encode("bob:hunter2");
    let ini = format!("_auth={pair}\n");
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini::<NoEnv>(&ini).apply_to(&mut npmrc);
    assert_eq!(
        npmrc.auth_headers.for_url("https://registry.npmjs.org/").as_deref(),
        Some(format!("Basic {pair}").as_str()),
    );
}

#[test]
fn top_level_username_password_keys_to_default_registry_basic_header() {
    let raw_password = "hunter2";
    let password_b64 = base64_encode(raw_password);
    let ini = format!("username=bob\n_password={password_b64}\n");
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini::<NoEnv>(&ini).apply_to(&mut npmrc);
    assert_eq!(
        npmrc.auth_headers.for_url("https://registry.npmjs.org/").as_deref(),
        Some(format!("Basic {}", base64_encode("bob:hunter2")).as_str()),
    );
}

/// A `//host/:_password=…` line on its own (no matching `username`)
/// produces no `Basic` header. The credential shape needs both
/// halves. Hits the `None` fallthrough in [`creds_to_header`].
#[test]
fn lone_per_registry_password_produces_no_header() {
    let ini = format!("//reg.com/:_password={}\n", base64_encode("solo"));
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini::<NoEnv>(&ini).apply_to(&mut npmrc);
    assert_eq!(npmrc.auth_headers.for_url("https://reg.com/"), None);
}

/// Per-registry creds with a recognisable suffix should be carried
/// through [`NpmrcAuth::build_auth_headers`] and surface as a
/// `Basic` header for matching URLs. Exercises the
/// `auth_header_by_uri.insert(...)` branch.
#[test]
fn per_registry_username_password_apply_through_build_auth_headers() {
    let raw_password = "hunter2";
    let password_b64 = base64_encode(raw_password);
    let ini =
        format!("//reg.example/:username=alice\n//reg.example/:_password={password_b64}\n",);
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini::<NoEnv>(&ini).apply_to(&mut npmrc);
    assert_eq!(
        npmrc.auth_headers.for_url("https://reg.example/foo").as_deref(),
        Some(format!("Basic {}", base64_encode("alice:hunter2")).as_str()),
    );
}

/// `//host/:somethingUnknown=value` lines are dropped silently.
/// [`split_creds_key`] returns `None` for anything outside
/// [`CREDS_SUFFIXES`], and the line then falls through to
/// [`apply_creds_field`] on [`NpmrcAuth::default_creds`] with a
/// non-matching field. Exercises both no-match arms.
#[test]
fn unknown_per_registry_suffix_is_silently_dropped() {
    let ini = "//reg.example/:registry=https://other.example/\n";
    let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert!(auth.creds_by_uri.is_empty());
    assert_eq!(auth.default_creds, RawCreds::default());
    assert_eq!(auth.warnings, Vec::<String>::new());
}

/// [`NpmrcAuth::apply_registry_and_warn`] should drain the warning
/// queue. Pnpm's `substituteEnv` writes the same string to stderr
/// via `globalWarn` once per resolution failure.
#[test]
fn apply_registry_and_warn_drains_warnings() {
    let ini = "//reg.com/:_authToken=${MISSING}\n";
    let mut auth = NpmrcAuth::from_ini::<NoEnv>(ini);
    assert_eq!(auth.warnings.len(), 1);
    let mut npmrc = Npmrc::new();
    auth.apply_registry_and_warn(&mut npmrc);
    assert!(auth.warnings.is_empty(), "warnings should be drained after flush");
}

/// When `_password` is *not* valid base64, [`creds_to_header`]
/// falls back to using the raw string verbatim. Mirrors the
/// `unwrap_or_else` branch inside that function. Pnpm's
/// `parseBasicAuth` doesn't have this exact fallback (it always
/// `atob`s), but pacquet's tolerance avoids losing the credential
/// for `.npmrc` files where `_password` was already a raw value.
#[test]
fn invalid_base64_password_falls_back_to_raw_value() {
    // `*` is outside the base64 alphabet, so `base64_decode`
    // returns `None` and the raw string is used as the password.
    let ini = "//reg.com/:username=alice\n//reg.com/:_password=raw*pw\n";
    let mut npmrc = Npmrc::new();
    NpmrcAuth::from_ini::<NoEnv>(ini).apply_to(&mut npmrc);
    assert_eq!(
        npmrc.auth_headers.for_url("https://reg.com/").as_deref(),
        Some(format!("Basic {}", base64_encode("alice:raw*pw")).as_str()),
    );
}

/// Exercises every branch of [`base64_decode`]: the alphanumeric
/// arms, the `+` arm, the `/` arm, the `=` padding break, and the
/// "invalid character" return. Without these the password-decode
/// fallback (`unwrap_or_else(... pass_b64.clone())`) path stays
/// unreachable from the parser tests.
#[test]
fn base64_decode_covers_every_alphabet_branch() {
    // Standard alphanumeric round-trip.
    assert_eq!(
        base64_decode(&base64_encode("alice:hunter2")).as_deref(),
        Some("alice:hunter2")
    );
    // `/` arm: `"???"` (three 0x3f bytes) encodes to `"Pz8/"`.
    assert_eq!(base64_decode("Pz8/").as_deref(), Some("???"));
    // `+` arm: `"~~~"` (three 0x7e bytes) encodes to `"fn5+"`.
    assert_eq!(base64_decode("fn5+").as_deref(), Some("~~~"));
    // `=` padding short-circuits the loop on a 2-byte input.
    assert_eq!(base64_decode("aGk=").as_deref(), Some("hi"));
    // Invalid byte returns None so the parser keeps the raw
    // value verbatim. `*` is not in the alphabet.
    assert_eq!(base64_decode("not*base64"), None);
}
