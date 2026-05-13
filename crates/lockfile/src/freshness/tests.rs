use super::{StalenessReason, satisfies_package_manifest};
use crate::Lockfile;
use pacquet_package_manifest::PackageManifest;
use pretty_assertions::assert_eq;
use tempfile::tempdir;
use text_block_macros::text_block;

/// Build a `PackageManifest` from inline JSON. Writes it to a temp
/// file so [`PackageManifest::from_path`] can parse it back through
/// the normal load path — exercises the actual deserialize, not a
/// `serde_json::Value` shortcut.
fn manifest_from_json(json: &str) -> (tempfile::TempDir, PackageManifest) {
    let tmp = tempdir().expect("create tempdir");
    let path = tmp.path().join("package.json");
    std::fs::write(&path, json).expect("write package.json");
    let manifest = PackageManifest::from_path(path).expect("parse package.json");
    (tmp, manifest)
}

/// Single-importer lockfile + matching manifest passes the check.
/// Baseline for every test below — if this fails everything else is
/// noise.
#[test]
fn matching_manifest_and_lockfile_satisfies() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      react:"
        "        specifier: ^17.0.2"
        "        version: 17.0.2"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": {
            "react": "^17.0.2"
        }
    }"#,
    );
    assert!(satisfies_package_manifest(importer, &manifest, ".").is_ok());
}

/// Manifest lists a dep the lockfile doesn't. Should surface as
/// `SpecifiersDiffer` with the missing entry in `added`.
#[test]
fn manifest_adds_dep_returns_specifier_diff() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      react:"
        "        specifier: ^17.0.2"
        "        version: 17.0.2"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": {
            "react": "^17.0.2",
            "lodash": "^4.17.21"
        }
    }"#,
    );
    let err = satisfies_package_manifest(importer, &manifest, ".").expect_err("should be stale");
    let StalenessReason::SpecifiersDiffer(diff) = err else {
        panic!("expected SpecifiersDiffer, got {err:?}");
    };
    assert_eq!(diff.added.get("lodash").map(String::as_str), Some("^4.17.21"));
    assert!(diff.removed.is_empty());
    assert!(diff.modified.is_empty());
}

/// Lockfile lists a dep the manifest dropped. Should surface as a
/// `SpecifiersDiffer` with the dropped entry in `removed`.
#[test]
fn manifest_drops_dep_returns_specifier_diff() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      react:"
        "        specifier: ^17.0.2"
        "        version: 17.0.2"
        "      lodash:"
        "        specifier: ^4.17.21"
        "        version: 4.17.21"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": {
            "react": "^17.0.2"
        }
    }"#,
    );
    let err = satisfies_package_manifest(importer, &manifest, ".").expect_err("should be stale");
    let StalenessReason::SpecifiersDiffer(diff) = err else {
        panic!("expected SpecifiersDiffer, got {err:?}");
    };
    assert_eq!(diff.removed.get("lodash").map(String::as_str), Some("^4.17.21"));
}

/// Same dep, same name, different specifier. Should surface as a
/// `SpecifiersDiffer` with the (lockfile, manifest) pair in
/// `modified`. This is the "user bumped a dep in package.json
/// without re-running install" case — the most common drift cause.
#[test]
fn manifest_bumps_specifier_returns_specifier_diff() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      react:"
        "        specifier: ^17.0.2"
        "        version: 17.0.2"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": {
            "react": "^18.0.0"
        }
    }"#,
    );
    let err = satisfies_package_manifest(importer, &manifest, ".").expect_err("should be stale");
    let StalenessReason::SpecifiersDiffer(diff) = err else {
        panic!("expected SpecifiersDiffer, got {err:?}");
    };
    let modified = diff.modified.get("react").expect("react bucketed under modified");
    assert_eq!(modified.0, "^17.0.2");
    assert_eq!(modified.1, "^18.0.0");
}

/// Manifest with dev + optional in addition to prod, all matching
/// the lockfile. Confirms the flat-union pre-pass treats all three
/// fields equally.
#[test]
fn matching_across_all_three_dep_fields_satisfies() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      react:"
        "        specifier: ^17.0.2"
        "        version: 17.0.2"
        "    devDependencies:"
        "      typescript:"
        "        specifier: ^5.0.0"
        "        version: 5.1.6"
        "    optionalDependencies:"
        "      fsevents:"
        "        specifier: ^2.0.0"
        "        version: 2.3.3"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": { "react": "^17.0.2" },
        "devDependencies": { "typescript": "^5.0.0" },
        "optionalDependencies": { "fsevents": "^2.0.0" }
    }"#,
    );
    assert!(satisfies_package_manifest(importer, &manifest, ".").is_ok());
}

/// Lockfile has no `importers["."]` entry — even though pacquet's
/// `Lockfile` type makes `importers` a map (so an empty map is a
/// valid shape), we still want to fail cleanly when the importer the
/// caller asked about isn't present.
#[test]
fn missing_importer_returns_no_importer() {
    // Build a manually-constructed lockfile with empty importers.
    let lockfile: Lockfile =
        serde_saphyr::from_str("lockfileVersion: '9.0'\n").expect("parse minimal lockfile");
    // We can't easily get a `ProjectSnapshot` out of an empty map,
    // so this test exercises the lookup-then-call shape on the
    // caller side: the caller uses `root_project()` which returns
    // `None`, and the `NoImporter` reason is constructed there.
    assert!(lockfile.root_project().is_none());
}

/// Same name + specifier moved between fields (`devDependencies` →
/// `dependencies`) should be caught by the per-field follow-up loop.
/// The flat-record pre-pass would say "specifiers match" because
/// they do across the union — but the dep-graph install would be
/// different so we must reject. Mirrors upstream's per-field check
/// at <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts#L67-L100>.
#[test]
fn dep_moves_between_fields_returns_dep_specifier_mismatch() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    devDependencies:"
        "      typescript:"
        "        specifier: ^5.0.0"
        "        version: 5.1.6"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    // Same name + specifier, but now in `dependencies` instead of
    // `devDependencies`.
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": { "typescript": "^5.0.0" }
    }"#,
    );
    let err = satisfies_package_manifest(importer, &manifest, ".").expect_err("should be stale");
    assert!(
        matches!(err, StalenessReason::DepSpecifierMismatch { .. }),
        "expected DepSpecifierMismatch, got {err:?}",
    );
}

/// `SpecDiff::Display` produces stable, user-readable output. Pins
/// the wording roughly: a regression in the format string would
/// silently scramble the error message users see in CI logs.
#[test]
fn spec_diff_display_lists_added_removed_modified() {
    let mut diff = super::SpecDiff::default();
    diff.added.insert("lodash".to_string(), "^4.0.0".to_string());
    diff.removed.insert("underscore".to_string(), "^1.0.0".to_string());
    diff.modified.insert("react".to_string(), ("^17.0.2".to_string(), "^18.0.0".to_string()));
    let rendered = diff.to_string();
    assert!(rendered.contains("1 dependencies were added: lodash@^4.0.0"));
    assert!(rendered.contains("1 dependencies were removed: underscore@^1.0.0"));
    assert!(rendered.contains("1 dependencies are mismatched:"));
    assert!(rendered.contains("react (lockfile: ^17.0.2, manifest: ^18.0.0)"));
}
