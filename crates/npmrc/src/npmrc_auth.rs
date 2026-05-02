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
mod tests;
