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
mod tests {
    use super::*;
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
}
