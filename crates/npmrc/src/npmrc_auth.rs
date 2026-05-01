use crate::Npmrc;

/// Narrow subset of `.npmrc` that pacquet currently reads.
///
/// At the moment this parser only extracts the top-level `registry` key.
/// The rest of pnpm's `.npmrc` allow-list (TLS via `ca` / `cafile` /
/// `cert` / `key`, npm auth via `_auth` / `_authToken` / `_password` /
/// `email` / `keyfile` / `username`, proxy via `https-proxy` / `proxy` /
/// `no-proxy` / `http-proxy` / `local-address` / `strict-ssl`, plus the
/// dynamic `@scope:registry` and `//host:_authToken` patterns) is not yet
/// represented in `NpmrcAuth` and is silently ignored.
///
/// Project-structural settings (`storeDir`, `lockfile`, hoist pattern,
/// `node-linker`, …) now live in `pnpm-workspace.yaml` and are also
/// ignored here.
#[derive(Debug, Default, PartialEq)]
pub struct NpmrcAuth {
    pub registry: Option<String>,
}

impl NpmrcAuth {
    /// Parse an `.npmrc` file's contents and pick out the auth/network keys.
    /// Unknown keys are silently dropped.
    ///
    /// The `.npmrc` format is a tiny ini dialect: one `key=value` per line,
    /// plus comments starting with `;` or `#`. We hand-parse instead of
    /// `serde_ini` so unknown / malformed keys don't blow up parsing the way
    /// they would with a strongly-typed deserializer.
    pub fn from_ini(text: &str) -> Self {
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
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if key == "registry" {
                auth.registry = Some(value.to_string());
            }
            // Other auth/network keys aren't consumed yet — they'll land
            // here as pacquet gains auth / proxy / TLS support.
        }
        auth
    }

    /// Apply the parsed auth settings onto `npmrc`, leaving unset fields
    /// alone and doing the same trailing-slash normalisation the ini
    /// deserializer used to perform via `deserialize_registry`.
    pub fn apply_to(self, npmrc: &mut Npmrc) {
        if let Some(registry) = self.registry {
            npmrc.registry =
                if registry.ends_with('/') { registry } else { format!("{registry}/") };
        }
    }
}

#[cfg(test)]
mod tests;
