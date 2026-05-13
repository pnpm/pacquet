//! TLS + local-address configuration consumed by
//! [`crate::ThrottledClient::for_installs`].
//!
//! `TlsConfig` holds the resolved `(ca, client_identity_pem, strict_ssl,
//! local_address)` quadruple. Built by `pacquet-config` from the
//! `.npmrc` keys `ca`, `cafile`, `cert`, `key`, `strict-ssl`, and
//! `local-address`. Lives in `pacquet-network` for the same reason
//! [`crate::ProxyConfig`] does — `pacquet-config` depends on
//! `pacquet-network` for `AuthHeaders`, so the inverse direction
//! would form a cycle.
//!
//! Ports the TLS wiring of pnpm v11's
//! [`network/fetch/src/dispatcher.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/network/fetch/src/dispatcher.ts).
//! Parity policy: pnpm performs no PEM parsing in user-space (PEM
//! strings are handed directly to Node `tls` / undici, which parse
//! internally), emits no `ERR_PNPM_*` codes for malformed TLS
//! material, silently ignores a missing `cafile`, and consults no
//! environment variables. Pacquet mirrors each of those choices.

use std::net::IpAddr;

/// Resolved TLS + local-address configuration.
///
/// All fields are optional. `strict_ssl` is `None` here because pnpm
/// applies the `true` default at every read site
/// ([`dispatcher.ts:191,197,241,295`](https://github.com/pnpm/pnpm/blob/94240bc046/network/fetch/src/dispatcher.ts#L191))
/// rather than baking it into the config layer — pacquet does the
/// same so a user that explicitly sets `strict-ssl=false` stays
/// distinguishable from "unset". The default value is applied at
/// client-build time by [`crate::ThrottledClient::for_installs`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TlsConfig {
    /// CA certificate chain to trust for TLS verification. Each
    /// element is a PEM-encoded certificate. Populated by `.npmrc`'s
    /// `ca` key (inline PEM, possibly multiple via array shape) or by
    /// reading `cafile` (which gets split on
    /// `-----END CERTIFICATE-----` to mirror pnpm's
    /// [loader behavior](https://github.com/pnpm/pnpm/blob/94240bc046/config/reader/src/loadNpmrcFiles.ts#L249-L255)).
    /// `cafile`-not-found is silently treated as unset, matching
    /// upstream.
    pub ca: Vec<String>,

    /// PEM-encoded client certificate, when client-cert auth is
    /// required by the registry. Set from `.npmrc`'s `cert` key.
    pub cert: Option<String>,

    /// PEM-encoded client private key. Paired with [`Self::cert`] when
    /// both are set — reqwest's `Identity::from_pkcs8_pem` consumes
    /// them as two separate buffers. Set from `.npmrc`'s `key` key.
    pub key: Option<String>,

    /// `strict-ssl` toggle. `None` = unset (defaults to `true` at
    /// apply time); `Some(true)` = explicit strict (same as default);
    /// `Some(false)` = disable both cert-chain and hostname
    /// verification (matches Node's `rejectUnauthorized=false` which
    /// short-circuits SNI / hostname checks too). Maps to reqwest's
    /// `ClientBuilder::danger_accept_invalid_certs`.
    pub strict_ssl: Option<bool>,

    /// Outbound interface IP. Maps to reqwest's
    /// `ClientBuilder::local_address`. pnpm passes the value as a
    /// bare string with no validation. Pacquet parses it as
    /// [`IpAddr`] in the config layer and silently drops anything
    /// that doesn't parse — mirroring pnpm's parity policy of letting
    /// the network layer surface the failure when (and if) the value
    /// actually gets used at connect time. A future enhancement could
    /// emit a warning at parse time; tracked alongside the rest of
    /// the TLS error-surface work.
    pub local_address: Option<IpAddr>,
}

/// Build-time error returned by [`crate::ThrottledClient::for_installs`]
/// when configured TLS material is invalid.
///
/// pnpm does not define `ERR_PNPM_INVALID_CA` / `ERR_PNPM_INVALID_CERT`
/// / `ERR_PNPM_INVALID_KEY` error codes — invalid PEM surfaces as raw
/// `tls.connect` errors at request time
/// ([`dispatcher.ts:184-200`](https://github.com/pnpm/pnpm/blob/94240bc046/network/fetch/src/dispatcher.ts#L184-L200)).
/// Pacquet validates eagerly because reqwest's `Certificate::from_pem`
/// / `Identity::from_pkcs8_pem` return errors up-front and pushing
/// that to per-request time would silently degrade every install
/// behind a broken `ca`. Diagnostic messages are plain prose; no code
/// attribute is emitted so reviewers can see at a glance that this is
/// a pacquet-only diagnostic, not a pnpm error code.
#[derive(Debug, derive_more::Display, derive_more::Error, miette::Diagnostic)]
#[non_exhaustive]
pub enum TlsError {
    /// `Certificate::from_pem` rejected one of the `ca` entries.
    /// `index` is the 0-based position within the resolved CA list.
    #[display("Invalid CA certificate (entry {index}): {reason}")]
    InvalidCa { index: usize, reason: String },

    /// `Identity::from_pkcs8_pem` rejected the `cert` + `key`
    /// PEM pair. The native-tls backend only accepts PKCS#8-encoded
    /// keys (`-----BEGIN PRIVATE KEY-----`); legacy PKCS#1
    /// (`-----BEGIN RSA PRIVATE KEY-----`) keys land here. See the
    /// comment on `apply_tls` in `crates/network/src/lib.rs` for the
    /// `openssl pkcs8` conversion path.
    #[display("Invalid client TLS cert/key: {reason}")]
    InvalidClientIdentity { reason: String },
}

#[cfg(test)]
mod tests;
