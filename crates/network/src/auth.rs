//! URL-keyed lookup of `Authorization` headers, ported from pnpm's
//! [`@pnpm/network.auth-header`](https://github.com/pnpm/pnpm/blob/601317e7a3/network/auth-header/src/index.ts).
//!
//! The lookup walks "nerf-darted" forms of a URL (the protocol-stripped
//! `//host[:port]/path/` representation npm has used for `.npmrc` keys
//! since the npm 5 era) from longest path prefix down to the host. If
//! the URL carries inline `user:password@`, that takes precedence and
//! is encoded as a `Basic` header even when no per-host token matches.
//!
//! The map is built once per install from the merged `.npmrc` and is
//! consulted on every metadata fetch and tarball download. Tarballs
//! served from a CDN host that differs from the registry host (the
//! common case for private registries) still pick up the header keyed
//! at the registry's nerf-dart, because the per-CDN nerf-dart never
//! exists in the config and the lookup falls through to the host-only
//! key for that registry.

use std::collections::HashMap;

/// Bag of `Authorization` header values keyed by the nerf-darted form
/// of each registry URL. Pacquet builds one of these from the parsed
/// `.npmrc` and shares it across every HTTP call made during install.
///
/// Construct via [`AuthHeaders::from_creds_map`], [`AuthHeaders::from_map`],
/// or [`AuthHeaders::default`] (empty). Look up via [`AuthHeaders::for_url`].
#[derive(Debug, Default, Clone)]
pub struct AuthHeaders {
    /// Keys are the nerf-darted form (`//host[:port]/path/`). Values
    /// are ready-to-send header values like `Bearer abc123` or
    /// `Basic Zm9vOmJhcg==`.
    by_uri: HashMap<String, String>,
    /// The longest key in `by_uri` measured in `/`-separated parts. The
    /// lookup walks from this depth down to 3 (the `//host/` floor),
    /// matching pnpm's `getMaxParts` precomputation.
    max_parts: usize,
}

impl AuthHeaders {
    /// Build an [`AuthHeaders`] from `(nerf_darted_uri, header_value)`
    /// pairs. Caller is responsible for nerf-darting and for choosing
    /// the right scheme (`Bearer ...` or `Basic ...`).
    ///
    /// The `default_registry_uri` argument is the URI to register
    /// against the `default` (empty-string) credentials slot, matching
    /// `createGetAuthHeaderByURI`'s `defaultRegistry` argument and
    /// applying the same fallback to `//registry.npmjs.org/` when
    /// nothing is supplied.
    pub fn from_creds_map<Iter>(headers: Iter, default_registry_uri: Option<&str>) -> Self
    where
        Iter: IntoIterator<Item = (String, String)>,
    {
        let registry_default_key =
            default_registry_uri.map(nerf_dart).unwrap_or_else(|| "//registry.npmjs.org/".into());
        let mut by_uri = HashMap::new();
        for (raw_uri, header_value) in headers {
            let key = if raw_uri.is_empty() { registry_default_key.clone() } else { raw_uri };
            by_uri.insert(key, header_value);
        }
        Self::from_map(by_uri)
    }

    /// Build an [`AuthHeaders`] directly from an already-keyed map.
    /// Each key must already be in nerf-darted form
    /// (`//host[:port]/path/`).
    pub fn from_map(by_uri: HashMap<String, String>) -> Self {
        let max_parts = by_uri.keys().map(|key| key.split('/').count()).max().unwrap_or(0);
        AuthHeaders { by_uri, max_parts }
    }

    /// Resolve an `Authorization` header for `url`, mirroring pnpm's
    /// `getAuthHeaderByURI`:
    ///
    /// 1. If `url` has a `user:password@` prefix, return `Basic` of it,
    ///    regardless of whether anything matched in the map.
    /// 2. Otherwise nerf-dart the URL and walk parent path prefixes
    ///    down to the host-only key.
    /// 3. If the URL carried the protocol's default port (`80` for
    ///    `http`, `443` for `https`), retry the lookup with the port
    ///    stripped — pnpm strips default ports during the second
    ///    pass via `removePort` on the parsed URL.
    pub fn for_url(&self, url: &str) -> Option<String> {
        // Append a trailing `/` first, matching pnpm's lookup which
        // does the same before parsing. Without this, a URL like
        // `https://npm.pkg.github.com/pnpm` (registry without
        // trailing slash) would nerf-dart to `//npm.pkg.github.com/`
        // and miss a `//npm.pkg.github.com/pnpm/` token.
        let mut owned: String;
        let url_with_slash = if url.ends_with('/') {
            url
        } else {
            owned = String::with_capacity(url.len() + 1);
            owned.push_str(url);
            owned.push('/');
            owned.as_str()
        };
        let parsed = ParsedUrl::parse(url_with_slash)?;
        if let Some(basic) = parsed.basic_auth_header() {
            return Some(basic);
        }
        if let Some(value) = self.lookup_by_nerf(&parsed) {
            return Some(value.to_owned());
        }
        if parsed.has_default_port() {
            let stripped = parsed.with_default_port_stripped();
            return self.lookup_by_nerf(&stripped).map(str::to_owned);
        }
        None
    }

    fn lookup_by_nerf(&self, parsed: &ParsedUrl<'_>) -> Option<&str> {
        if self.by_uri.is_empty() {
            return None;
        }
        let nerfed = parsed.nerf_dart();
        let parts: Vec<&str> = nerfed.split('/').collect();
        let upper = parts.len().min(self.max_parts);
        // Walk from the longest meaningful prefix down to `//host/`,
        // matching the index range `[maxParts-1, 3]` from
        // `getAuthHeaderByURI`. `parts[0..3]` is `["", "", host]`, so
        // joined with `/` it is `//host`; the loop slices through
        // `parts[..i]` and re-joins, then appends a trailing slash.
        for i in (3..=upper).rev() {
            let key = format!("{}/", parts[..i].join("/"));
            if let Some(value) = self.by_uri.get(&key) {
                return Some(value.as_str());
            }
        }
        None
    }
}

/// Strip protocol, query string, fragment, basic-auth, and any
/// trailing characters past the path's final `/`, returning the
/// canonical "nerf-darted" form npm uses as `.npmrc` keys.
///
/// Examples:
/// * `https://reg.com/` → `//reg.com/`
/// * `https://reg.com:8080/` → `//reg.com:8080/`
/// * `https://reg.com/foo/-/foo-1.tgz` → `//reg.com/foo/-/`
/// * `https://user:pw@reg.com/scoped/pkg` → `//reg.com/scoped/`
/// * `https://npm.pkg.github.com/pnpm` (no trailing slash) → `//npm.pkg.github.com/`
pub fn nerf_dart(url: &str) -> String {
    let parsed = match ParsedUrl::parse(url) {
        Some(parsed) => parsed,
        None => return String::new(),
    };
    parsed.nerf_dart()
}

/// Lightweight URL parsing tuned for the subset of URLs `.npmrc` and
/// registries actually carry: `http`/`https` only, optional `user:pw@`,
/// optional `:port`, optional path. Standard library has no URL type
/// and pulling in the full `url` crate just for this is heavier than
/// needed.
#[derive(Clone, Copy)]
struct ParsedUrl<'a> {
    scheme: &'a str,
    user_info: Option<&'a str>,
    host: &'a str,
    port: Option<&'a str>,
    path: &'a str,
}

impl<'a> ParsedUrl<'a> {
    fn parse(url: &'a str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        // Strip query string and fragment — they never participate in
        // nerf-darting per `removeFragment` / `removeSearch` in npm's
        // own implementation.
        let rest = rest.split(['?', '#']).next().unwrap_or(rest);
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path_tail)) => (authority, path_tail),
            None => (rest, ""),
        };
        let (user_info, host_port) = match authority.rsplit_once('@') {
            Some((user_info, host_port)) => (Some(user_info), host_port),
            None => (None, authority),
        };
        let (host, port) = match host_port.rsplit_once(':') {
            // Skip IPv6 brackets — pnpm doesn't, but neither does any
            // npm registry we care about. Documenting the limit here
            // rather than silently misparsing.
            Some((host, port)) if !host.contains('[') => (host, Some(port)),
            _ => (host_port, None),
        };
        Some(ParsedUrl { scheme, user_info, host, port, path })
    }

    fn nerf_dart(&self) -> String {
        let mut out = String::with_capacity(2 + self.host.len() + self.path.len());
        out.push_str("//");
        out.push_str(self.host);
        if let Some(port) = self.port {
            out.push(':');
            out.push_str(port);
        }
        out.push('/');
        // Drop everything after the last `/` in the path — that final
        // segment is a filename or package selector, not a key.
        let trimmed = match self.path.rfind('/') {
            Some(index) => &self.path[..index],
            None => "",
        };
        if !trimmed.is_empty() {
            out.push_str(trimmed);
            out.push('/');
        }
        out
    }

    fn basic_auth_header(&self) -> Option<String> {
        let user_info = self.user_info?;
        let (user, pass) = match user_info.split_once(':') {
            Some((user, pass)) => (user, pass),
            None => (user_info, ""),
        };
        if user.is_empty() && pass.is_empty() {
            return None;
        }
        Some(format!("Basic {}", base64_encode(&format!("{user}:{pass}"))))
    }

    fn has_default_port(&self) -> bool {
        matches!((self.scheme, self.port), ("https", Some("443")) | ("http", Some("80")))
    }

    fn with_default_port_stripped(&self) -> ParsedUrl<'a> {
        ParsedUrl { port: None, ..*self }
    }
}

/// Local base64 encode so this crate doesn't pull in `base64` just for
/// 4 lines. Standard alphabet, with padding, matching `btoa` /
/// `Buffer.from(...).toString('base64')` from the JS port.
pub fn base64_encode(input: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(n & 0x3f) as usize] as char);
    }
    let remainder = chunks.remainder();
    match remainder.len() {
        1 => {
            let n = u32::from(remainder[0]) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (u32::from(remainder[0]) << 16) | (u32::from(remainder[1]) << 8);
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn build(entries: &[(&str, &str)]) -> AuthHeaders {
        AuthHeaders::from_creds_map(
            entries.iter().map(|(uri, value)| ((*uri).to_string(), (*value).to_string())),
            None,
        )
    }

    #[test]
    fn nerf_dart_strips_protocol_query_fragment_and_filename() {
        assert_eq!(nerf_dart("https://reg.com/"), "//reg.com/");
        assert_eq!(nerf_dart("https://reg.com:8080/"), "//reg.com:8080/");
        assert_eq!(nerf_dart("https://reg.com/foo/-/foo-1.tgz"), "//reg.com/foo/-/");
        assert_eq!(
            nerf_dart("https://npm.pkg.github.com/pnpm/foo?token=x"),
            "//npm.pkg.github.com/pnpm/",
        );
        assert_eq!(nerf_dart("https://user:pw@reg.com/scoped/pkg"), "//reg.com/scoped/");
    }

    #[test]
    fn base64_round_trip_matches_known_vectors() {
        // Sanity-check vectors from the pnpm test fixtures.
        assert_eq!(base64_encode("foobar:foobar"), "Zm9vYmFyOmZvb2Jhcg==");
        assert_eq!(base64_encode("user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn matches_host_only_token() {
        let headers = build(&[("//reg.com/", "Bearer abc123")]);
        assert_eq!(headers.for_url("https://reg.com/").as_deref(), Some("Bearer abc123"));
        assert_eq!(
            headers.for_url("https://reg.com/foo/-/foo-1.0.0.tgz").as_deref(),
            Some("Bearer abc123"),
        );
        assert_eq!(headers.for_url("https://reg.io/foo/-/foo-1.0.0.tgz"), None);
    }

    #[test]
    fn matches_path_scoped_token() {
        let headers =
            build(&[("//reg.com/", "Bearer abc123"), ("//reg.co/tarballs/", "Bearer xxx")]);
        assert_eq!(
            headers.for_url("https://reg.co/tarballs/foo/-/foo-1.0.0.tgz").as_deref(),
            Some("Bearer xxx"),
        );
    }

    #[test]
    fn matches_explicit_port_token() {
        let headers = build(&[("//reg.gg:8888/", "Bearer 0000")]);
        assert_eq!(
            headers.for_url("https://reg.gg:8888/foo/-/foo-1.0.0.tgz").as_deref(),
            Some("Bearer 0000"),
        );
    }

    #[test]
    fn default_https_port_strips_for_lookup() {
        let headers = build(&[("//reg.com/", "Bearer abc123")]);
        assert_eq!(headers.for_url("https://reg.com:443/").as_deref(), Some("Bearer abc123"));
        assert_eq!(headers.for_url("http://reg.com:80/").as_deref(), Some("Bearer abc123"));
    }

    #[test]
    fn basic_auth_in_url_wins_over_token() {
        let headers = build(&[("//reg.com/", "Bearer abc123")]);
        let header = headers.for_url("https://user:secret@reg.com/").unwrap();
        assert_eq!(header, format!("Basic {}", base64_encode("user:secret")));
    }

    #[test]
    fn basic_auth_works_without_settings() {
        let empty = AuthHeaders::default();
        assert_eq!(
            empty.for_url("https://user:secret@reg.io/"),
            Some(format!("Basic {}", base64_encode("user:secret"))),
        );
        assert_eq!(
            empty.for_url("https://user:@reg.io/"),
            Some(format!("Basic {}", base64_encode("user:"))),
        );
        assert_eq!(
            empty.for_url("https://user@reg.io/"),
            Some(format!("Basic {}", base64_encode("user:"))),
        );
    }

    #[test]
    fn registry_with_pathname_matches_metadata_and_tarballs() {
        // Mirrors the GitHub Packages scope-registry example from
        // pnpm's test suite.
        let headers = build(&[("//npm.pkg.github.com/pnpm/", "Bearer abc123")]);
        assert_eq!(
            headers.for_url("https://npm.pkg.github.com/pnpm").as_deref(),
            Some("Bearer abc123"),
        );
        assert_eq!(
            headers.for_url("https://npm.pkg.github.com/pnpm/").as_deref(),
            Some("Bearer abc123"),
        );
        assert_eq!(
            headers.for_url("https://npm.pkg.github.com/pnpm/foo/-/foo-1.0.0.tgz").as_deref(),
            Some("Bearer abc123"),
        );
    }

    #[test]
    fn default_registry_creds_apply_to_npmjs_when_unspecified() {
        let headers = AuthHeaders::from_creds_map(
            [(String::new(), "Bearer default-token".to_owned())],
            Some("https://registry.npmjs.org/"),
        );
        assert_eq!(
            headers.for_url("https://registry.npmjs.org/").as_deref(),
            Some("Bearer default-token"),
        );
        assert_eq!(
            headers.for_url("https://registry.npmjs.org/foo/-/foo-1.0.0.tgz").as_deref(),
            Some("Bearer default-token"),
        );
    }

    #[test]
    fn registry_with_pathname_matches_with_explicit_port() {
        let headers =
            build(&[("//custom.domain.com/artifactory/api/npm/npm-virtual/", "Bearer xyz")]);
        assert_eq!(
            headers
                .for_url("https://custom.domain.com:443/artifactory/api/npm/npm-virtual/")
                .as_deref(),
            Some("Bearer xyz"),
        );
        assert_eq!(
            headers
                .for_url(
                    "https://custom.domain.com:443/artifactory/api/npm/npm-virtual/@platform/device-utils/-/@platform/device-utils-1.0.0.tgz",
                )
                .as_deref(),
            Some("Bearer xyz"),
        );
        assert_eq!(
            headers.for_url("https://custom.domain.com:443/artifactory/api/npm/").as_deref(),
            None,
        );
    }

    #[test]
    fn returns_none_for_unmatched_url_in_empty_map() {
        assert_eq!(AuthHeaders::default().for_url("http://reg.com"), None);
    }

    /// Specifically exercises the trailing-slash-append branch in
    /// [`AuthHeaders::for_url`]: the URL ends without a `/` *and*
    /// names a path segment (`/scope`). Without the append,
    /// [`nerf_dart`] would drop the segment and miss the token; with
    /// it, the lookup walks `//reg.com/scope/`. Removing the append
    /// branch makes this test fail.
    /// [`registry_with_pathname_matches_metadata_and_tarballs`] alone
    /// is not enough because its host-only assertion would pass via
    /// [`ParsedUrl::parse`]'s no-path branch even without the append.
    #[test]
    fn slash_append_branch_lets_path_segment_match() {
        let headers = build(&[("//reg.com/scope/", "Bearer scoped")]);
        assert_eq!(headers.for_url("https://reg.com/scope").as_deref(), Some("Bearer scoped"),);
    }

    /// Hits the `None => return String::new()` branch of [`nerf_dart`]
    /// (and the `?` short-circuit in [`ParsedUrl::parse`]).
    #[test]
    fn nerf_dart_returns_empty_for_malformed_url() {
        assert_eq!(nerf_dart("not-a-url"), "");
        assert_eq!(nerf_dart(""), "");
        // No URL → no match in any non-empty map.
        let headers = build(&[("//reg.com/", "Bearer abc123")]);
        assert_eq!(headers.for_url("not-a-url"), None);
    }

    /// Hits the no-path-separator branch (`None => (rest, "")`) inside
    /// [`ParsedUrl::parse`]: the URL has no `/` after the authority.
    /// The parsed `path` is an empty string, so [`nerf_dart`] should
    /// produce `//host/`.
    #[test]
    fn nerf_dart_handles_url_with_no_path_separator() {
        assert_eq!(nerf_dart("https://reg.com"), "//reg.com/");
        assert_eq!(nerf_dart("https://reg.com:8080"), "//reg.com:8080/");
    }

    /// Hits the `user.is_empty() && pass.is_empty()` short-circuit in
    /// [`ParsedUrl::basic_auth_header`]: a URL whose authority parses
    /// as `@host` must not produce a `Basic ` header.
    #[test]
    fn empty_user_info_returns_no_basic_header() {
        let empty = AuthHeaders::default();
        assert_eq!(empty.for_url("https://@reg.com/"), None);
    }
}
