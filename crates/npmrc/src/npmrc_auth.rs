use std::collections::HashMap;
use std::sync::Arc;

use pacquet_network::{AuthHeaders, base64_encode, nerf_dart};

use crate::{Npmrc, api::EnvVar, env_replace::env_replace};

/// Subset of `.npmrc` keys pacquet honours for registry / auth setup.
///
/// The parser pulls out:
/// * the top-level `registry=` URL (already supported pre-#336),
/// * default-registry credentials (`_auth`, `_authToken`,
///   `username` + `_password`),
/// * per-registry credentials keyed on a nerf-darted URI prefix
///   (e.g. `//npm.pkg.github.com/pnpm/:_authToken=…`).
///
/// Values pass through `${VAR}` substitution before being stored,
/// matching pnpm's `loadNpmrcFiles.ts` flow. Substitution failures are
/// recorded as warnings and the offending value is left verbatim, again
/// matching pnpm.
///
/// Other `.npmrc` knobs (TLS, proxy, scoped `@scope:registry`, etc.)
/// remain unparsed for now — see the upstream
/// [`isIniConfigKey`](https://github.com/pnpm/pnpm/blob/601317e7a3/config/reader/src/localConfig.ts#L160-L161)
/// list. They will land here as the matching feature work picks them
/// up.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NpmrcAuth {
    pub registry: Option<String>,
    /// Default-registry creds (i.e. `_auth=…`, `_authToken=…`,
    /// `username=…` / `_password=…` without a leading `//host/`).
    /// Applied to whichever URI the resolved `registry` points at.
    pub default_creds: RawCreds,
    /// Per-URI creds, keyed by the literal `.npmrc` key prefix
    /// (`//host[:port]/path/`). The map is preserved verbatim through
    /// to [`AuthHeaders`] construction so the lookup keys stay
    /// byte-equivalent to upstream.
    pub creds_by_uri: HashMap<String, RawCreds>,
    /// `${VAR}` placeholders that could not be resolved while parsing.
    /// Surfaced as warnings; `pnpm` does the same in
    /// [`substituteEnv`](https://github.com/pnpm/pnpm/blob/601317e7a3/config/reader/src/loadNpmrcFiles.ts#L156-L162).
    pub warnings: Vec<String>,
}

/// Raw (unparsed) credential fields for a given registry URI, mirroring
/// pnpm's
/// [`RawCreds`](https://github.com/pnpm/pnpm/blob/601317e7a3/config/reader/src/parseCreds.ts#L7-L18).
/// Each `Option` stores the post-`${VAR}`-substitution value when set.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct RawCreds {
    /// `_authToken=` value.
    pub auth_token: Option<String>,
    /// `_auth=` value (base64 of `username:password`).
    pub auth_pair_base64: Option<String>,
    /// `username=` value.
    pub username: Option<String>,
    /// `_password=` value (base64-encoded password, per npm convention).
    pub password: Option<String>,
}

impl RawCreds {
    fn is_empty(&self) -> bool {
        self.auth_token.is_none()
            && self.auth_pair_base64.is_none()
            && self.username.is_none()
            && self.password.is_none()
    }
}

impl NpmrcAuth {
    /// Parse an `.npmrc` file's contents and pick out the auth/network keys.
    /// Unknown keys are silently dropped. `${VAR}` placeholders inside
    /// values are resolved via the [`EnvVar`] capability; placeholders
    /// that cannot be resolved leave the value verbatim and emit a
    /// warning.
    pub fn from_ini<Api: EnvVar>(text: &str) -> Self {
        let mut auth = NpmrcAuth::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            // `[section]` headers aren't meaningful in .npmrc; skip them.
            if line.starts_with('[') && line.ends_with(']') {
                continue;
            }
            let Some((raw_key, raw_value)) = line.split_once('=') else {
                continue;
            };
            let raw_key = raw_key.trim();
            let raw_value = raw_value.trim();

            // Apply ${VAR} substitution to both the key and the value,
            // matching `readAndFilterNpmrc` in pnpm's `loadNpmrcFiles.ts`.
            let key = match env_replace::<Api>(raw_key) {
                Ok(value) => value,
                Err(error) => {
                    auth.warnings.push(error.to_string());
                    raw_key.to_owned()
                }
            };
            let value = match env_replace::<Api>(raw_value) {
                Ok(value) => value,
                Err(error) => {
                    auth.warnings.push(error.to_string());
                    raw_value.to_owned()
                }
            };

            if key == "registry" {
                auth.registry = Some(value);
                continue;
            }

            if let Some((uri, suffix)) = split_creds_key(&key) {
                let entry = auth.creds_by_uri.entry(uri.to_owned()).or_default();
                apply_creds_field(entry, suffix, value);
                continue;
            }

            apply_creds_field(&mut auth.default_creds, key.as_str(), value);
        }
        auth
    }

    /// Phase 1: write the resolved `registry` onto `npmrc` and emit
    /// any `${VAR}`-substitution warnings. Does *not* build
    /// `auth_headers` yet — call [`NpmrcAuth::build_auth_headers`]
    /// after every other config layer (notably `pnpm-workspace.yaml`)
    /// has had a chance to override `registry`, so default-registry
    /// creds end up keyed at the final URL.
    pub fn apply_registry_and_warn(&mut self, npmrc: &mut Npmrc) {
        if let Some(registry) = self.registry.take() {
            npmrc.registry =
                if registry.ends_with('/') { registry } else { format!("{registry}/") };
        }
        for message in std::mem::take(&mut self.warnings) {
            tracing::warn!(target: "pacquet::npmrc", "{message}");
        }
    }

    /// Phase 2: compute and store the final [`AuthHeaders`] map,
    /// keying default-registry creds at `npmrc.registry`'s nerf-darted
    /// URI. Mirrors pnpm's
    /// [`getAuthHeadersFromCreds`](https://github.com/pnpm/pnpm/blob/601317e7a3/network/auth-header/src/getAuthHeadersFromConfig.ts).
    pub fn build_auth_headers(self, npmrc: &mut Npmrc) {
        let mut auth_header_by_uri: HashMap<String, String> = HashMap::new();
        for (uri, raw) in self.creds_by_uri {
            if let Some(header) = creds_to_header(&raw) {
                auth_header_by_uri.insert(uri, header);
            }
        }
        if !self.default_creds.is_empty()
            && let Some(header) = creds_to_header(&self.default_creds)
        {
            auth_header_by_uri.insert(nerf_dart(&npmrc.registry), header);
        }

        npmrc.auth_headers =
            Arc::new(AuthHeaders::from_creds_map(auth_header_by_uri, Some(&npmrc.registry)));
    }

    /// Convenience wrapper that runs [`apply_registry_and_warn`]
    /// followed by [`build_auth_headers`] in one call. Use this in
    /// tests and other callers that don't layer additional config
    /// sources on top of `.npmrc`.
    ///
    /// [`apply_registry_and_warn`]: NpmrcAuth::apply_registry_and_warn
    /// [`build_auth_headers`]: NpmrcAuth::build_auth_headers
    pub fn apply_to(mut self, npmrc: &mut Npmrc) {
        self.apply_registry_and_warn(npmrc);
        self.build_auth_headers(npmrc);
    }
}

/// Convert raw .npmrc credentials into the `Authorization` header
/// value pnpm would send. Returns `None` if no usable credential
/// shape is present.
fn creds_to_header(creds: &RawCreds) -> Option<String> {
    if let Some(token) = &creds.auth_token {
        return Some(format!("Bearer {token}"));
    }
    if let Some(pair) = &creds.auth_pair_base64 {
        return Some(format!("Basic {pair}"));
    }
    if let (Some(user), Some(pass_b64)) = (&creds.username, &creds.password) {
        // npm encodes `_password` as base64 of the raw password. The
        // header itself is `Basic base64(user:password)`, so we decode
        // the password back and re-encode the pair, matching pnpm's
        // [`parseBasicAuth`](https://github.com/pnpm/pnpm/blob/601317e7a3/config/reader/src/parseCreds.ts#L56-L77).
        let password = base64_decode(pass_b64).unwrap_or_else(|| pass_b64.clone());
        return Some(format!("Basic {}", base64_encode(&format!("{user}:{password}"))));
    }
    None
}

/// Decode a standard base64 string. Used for the `_password` field
/// where npm stores the raw password base64-encoded; falls back to
/// returning `None` so the caller can keep the raw value verbatim
/// when the input is not valid base64.
fn base64_decode(input: &str) -> Option<String> {
    let cleaned: Vec<u8> = input.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
    let mut bytes = Vec::with_capacity(cleaned.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in cleaned {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    String::from_utf8(bytes).ok()
}

/// Auth-suffix keys recognised on a `//host[:port]/path/:` prefix,
/// mirroring `AUTH_SUFFIX_RE` from pnpm's `getNetworkConfigs.ts`.
const CREDS_SUFFIXES: &[&str] = &["_authToken", "_auth", "_password", "username"];

fn split_creds_key(key: &str) -> Option<(&str, &str)> {
    if !key.starts_with("//") {
        return None;
    }
    for suffix in CREDS_SUFFIXES {
        let needle = format!(":{suffix}");
        if let Some(stripped) = key.strip_suffix(needle.as_str()) {
            return Some((stripped, suffix));
        }
    }
    None
}

fn apply_creds_field(creds: &mut RawCreds, field: &str, value: String) {
    // The catch-all swallows arbitrary `.npmrc` keys that don't map to
    // a credential field — e.g. a top-level `store-dir=` line, or a
    // `//host/:registry=` per-registry override that we don't honour
    // yet. Matches pnpm's `getNetworkConfigs` shape: only the four
    // recognised fields contribute to `RawCreds`; everything else is
    // silently dropped.
    match field {
        "_authToken" => creds.auth_token = Some(value),
        "_auth" => creds.auth_pair_base64 = Some(value),
        "username" => creds.username = Some(value),
        "_password" => creds.password = Some(value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// `[section]` headers are not meaningful in `.npmrc`; the parser
    /// should skip them silently.
    #[test]
    fn ini_section_headers_are_skipped() {
        let ini = "[default]\nregistry=https://r.example\n[other]\n";
        let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
        assert_eq!(auth.registry.as_deref(), Some("https://r.example"));
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
    /// land on `default_creds` so the resolved registry's nerf-darted
    /// URI gets a `Basic` header.
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
    /// produces no `Basic` header — the credential shape needs both
    /// halves. Hits the `None` fallthrough in `creds_to_header`.
    #[test]
    fn lone_per_registry_password_produces_no_header() {
        let ini = format!("//reg.com/:_password={}\n", base64_encode("solo"));
        let mut npmrc = Npmrc::new();
        NpmrcAuth::from_ini::<NoEnv>(&ini).apply_to(&mut npmrc);
        assert_eq!(npmrc.auth_headers.for_url("https://reg.com/"), None);
    }

    /// Per-registry creds with a recognisable suffix should be carried
    /// through `build_auth_headers` and surface as a `Basic` header for
    /// matching URLs. Exercises the `auth_header_by_uri.insert(...)`
    /// branch in [`NpmrcAuth::build_auth_headers`].
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

    /// `//host/:somethingUnknown=value` lines are dropped silently:
    /// `split_creds_key` returns `None` for anything outside
    /// [`CREDS_SUFFIXES`], and the line then falls through to
    /// `apply_creds_field` on `default_creds` with a non-matching
    /// field. Exercises both no-match arms.
    #[test]
    fn unknown_per_registry_suffix_is_silently_dropped() {
        let ini = "//reg.example/:registry=https://other.example/\n";
        let auth = NpmrcAuth::from_ini::<NoEnv>(ini);
        assert!(auth.creds_by_uri.is_empty());
        assert_eq!(auth.default_creds, RawCreds::default());
        assert_eq!(auth.warnings, Vec::<String>::new());
    }

    /// `apply_registry_and_warn` should drain the warning queue —
    /// pnpm's `substituteEnv` writes the same string to stderr via
    /// `globalWarn` once per resolution failure.
    #[test]
    fn apply_registry_and_warn_drains_warnings() {
        let ini = "//reg.com/:_authToken=${MISSING}\n";
        let mut auth = NpmrcAuth::from_ini::<NoEnv>(ini);
        assert_eq!(auth.warnings.len(), 1);
        let mut npmrc = Npmrc::new();
        auth.apply_registry_and_warn(&mut npmrc);
        assert!(auth.warnings.is_empty(), "warnings should be drained after flush");
    }

    /// When `_password` is *not* valid base64, `creds_to_header`
    /// falls back to using the raw string verbatim. Mirrors the
    /// `unwrap_or_else` branch in `creds_to_header`. Pnpm's
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
}
