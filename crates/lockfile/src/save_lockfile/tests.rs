use super::SaveLockfileError;
use crate::Lockfile;
use pretty_assertions::assert_eq;
use tempfile::tempdir;
use text_block_macros::text_block;

/// A compact v9 lockfile fixture exercising the `importers` root entry, the
/// `packages` metadata map (registry resolution + engines + hasBin), and
/// the `snapshots` map (including peer-qualified keys and inner
/// `dependencies`).
const LOCKFILE_YAML: &str = text_block! {
    "lockfileVersion: '9.0'"
    ""
    "settings:"
    "  autoInstallPeers: true"
    "  excludeLinksFromLockfile: false"
    ""
    "importers:"
    ""
    "  .:"
    "    dependencies:"
    "      react:"
    "        specifier: ^17.0.2"
    "        version: 17.0.2"
    "      react-dom:"
    "        specifier: ^17.0.2"
    "        version: 17.0.2(react@17.0.2)"
    "    devDependencies:"
    "      typescript:"
    "        specifier: ^5.1.6"
    "        version: 5.1.6"
    ""
    "packages:"
    ""
    "  react@17.0.2:"
    "    resolution: {integrity: sha512-TIE61hcgbI/SlJh/0c1sT1SZbBlpg7WiZcs65WPJhoIZQPhH1SCpcGA7LgrVXT15lwN3HV4GQM/MJ9aKEn3Qfg==}"
    "    engines: {node: '>=0.10.0'}"
    ""
    "  react-dom@17.0.2:"
    "    resolution: {integrity: sha512-s4h96KtLDUQlsENhMn1ar8t2bEa+q/YAtj8pPPdIjPDGBDIVNsrD9aXNWqspUe6AzKCIG0C1HZZLqLV7qpOBGA==}"
    "    peerDependencies:"
    "      react: 17.0.2"
    ""
    "  typescript@5.1.6:"
    "    resolution: {integrity: sha512-zaWCozRZ6DLEWAWFrVDz1H6FVXzUSfTy5FUMWsQlU8Ym5JP9eO4xkTIROFCQvhQf61z6O/G6ugw3SgAnvvm+HA==}"
    "    engines: {node: '>=14.17'}"
    "    hasBin: true"
    ""
    "snapshots:"
    ""
    "  react@17.0.2: {}"
    ""
    "  react-dom@17.0.2(react@17.0.2):"
    "    dependencies:"
    "      react: 17.0.2"
    ""
    "  typescript@5.1.6: {}"
};

#[test]
fn round_trip_parse_save_parse_preserves_lockfile() {
    let original: Lockfile = serde_saphyr::from_str(LOCKFILE_YAML).expect("parse fixture lockfile");

    let tmp = tempdir().expect("create tempdir");
    let path = tmp.path().join("pnpm-lock.yaml");
    original.save_to_path(&path).expect("save lockfile");

    let saved_bytes = std::fs::read_to_string(&path).expect("read saved lockfile");

    // Long single-line scalars (notably `integrity: sha512-…`) must stay plain;
    // pnpm-lock.yaml never uses folded block scalars (`>-`) for them. Guard the
    // formatting invariant that `serialize_yaml` exists to enforce.
    assert!(
        !saved_bytes.contains(">-"),
        "saved lockfile must not contain folded block scalars (`>-`):\n{saved_bytes}",
    );
    assert!(
        saved_bytes.contains("integrity: sha512-"),
        "saved lockfile must keep `integrity: sha512-` as a plain scalar:\n{saved_bytes}",
    );

    let reparsed: Lockfile = serde_saphyr::from_str(&saved_bytes).expect("reparse lockfile");

    assert_eq!(original, reparsed);
}

#[test]
fn save_fails_with_wrapped_io_error_when_path_is_invalid() {
    let empty_lockfile: Lockfile =
        serde_saphyr::from_str("lockfileVersion: '9.0'\n").expect("parse minimal lockfile");

    // Attempt to write under a non-existent directory; fs::write returns NotFound.
    let tmp = tempdir().expect("create tempdir");
    let bad_path = tmp.path().join("missing-dir").join("pnpm-lock.yaml");
    let err = empty_lockfile.save_to_path(&bad_path).expect_err("should fail");
    assert!(
        matches!(err, SaveLockfileError::WriteFile(_)),
        "expected SaveLockfileError::WriteFile(_), got: {err:?}",
    );
}
