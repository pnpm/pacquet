//! Unit tests for the [`super`] TLS types.

use super::{TlsConfig, TlsError};
use std::net::Ipv4Addr;

#[test]
fn tls_config_default_is_empty() {
    let cfg = TlsConfig::default();
    assert!(cfg.ca.is_empty(), "default CA list is empty");
    assert!(cfg.cert.is_none());
    assert!(cfg.key.is_none());
    assert!(cfg.strict_ssl.is_none(), "default is None — true is applied at build site");
    assert!(cfg.local_address.is_none());
}

#[test]
fn tls_config_clone_round_trip() {
    let cfg = TlsConfig {
        ca: vec!["pem1".to_string(), "pem2".to_string()],
        cert: Some("cert".to_string()),
        key: Some("key".to_string()),
        strict_ssl: Some(false),
        local_address: Some(Ipv4Addr::new(192, 168, 1, 100).into()),
    };
    assert_eq!(cfg.clone(), cfg);
}

#[test]
fn tls_error_invalid_ca_includes_index_in_display() {
    let err = TlsError::InvalidCa { index: 3, reason: "bad pem".into() };
    let rendered = err.to_string();
    assert!(rendered.contains("entry 3"), "expected `entry 3` in {rendered}");
    assert!(rendered.contains("bad pem"), "expected reason in {rendered}");
}

#[test]
fn tls_error_invalid_client_identity_includes_reason_in_display() {
    let err = TlsError::InvalidClientIdentity { reason: "garbage key".into() };
    let rendered = err.to_string();
    assert!(rendered.contains("garbage key"), "expected reason in {rendered}");
}
