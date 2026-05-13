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
    diff.added.insert("ramda".to_string(), "^0.30.0".to_string());
    diff.removed.insert("underscore".to_string(), "^1.0.0".to_string());
    diff.modified.insert("react".to_string(), ("^17.0.2".to_string(), "^18.0.0".to_string()));
    let rendered = diff.to_string();
    // Plural noun + plural verb for n>1.
    assert!(rendered.contains("2 dependencies were added: "));
    // Singular noun + singular verb for n==1 — the Copilot review
    // catch ("1 dependencies were added" was grammatically wrong).
    assert!(rendered.contains("1 dependency was removed: underscore@^1.0.0"));
    assert!(rendered.contains("1 dependency is mismatched:"));
    assert!(rendered.contains("react (lockfile: ^17.0.2, manifest: ^18.0.0)"));
}

/// Two deps swapped between fields with same cardinality on each
/// side: lockfile has `react` under `dependencies` + `typescript`
/// under `devDependencies`, manifest swaps them. The flat-union diff
/// over `(deps ∪ devDeps ∪ optDeps)` matches because the union is
/// identical, so the per-field check is the only thing that can
/// catch this. Pre-fix the per-field loop only ran when field
/// cardinalities differed; this test guards against that regression.
#[test]
fn cross_field_swap_with_same_cardinalities_caught_by_per_field_check() {
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
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    // Same names + specifiers as the lockfile, but `react` and
    // `typescript` swap fields.
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": { "typescript": "^5.0.0" },
        "devDependencies": { "react": "^17.0.2" }
    }"#,
    );
    let err = satisfies_package_manifest(importer, &manifest, ".").expect_err("should be stale");
    assert!(
        matches!(err, StalenessReason::DepSpecifierMismatch { .. }),
        "expected DepSpecifierMismatch for cross-field swap, got {err:?}",
    );
}

/// `publishDirectory` on the lockfile differing from
/// `publishConfig.directory` on the manifest fails the check.
/// Mirrors upstream's `publishDirectory` mismatch.
#[test]
fn publish_directory_mismatch_returns_publish_directory_mismatch() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    publishDirectory: ./dist"
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
        "publishConfig": { "directory": "./build" },
        "dependencies": { "react": "^17.0.2" }
    }"#,
    );
    let err = satisfies_package_manifest(importer, &manifest, ".").expect_err("should be stale");
    assert!(
        matches!(err, StalenessReason::PublishDirectoryMismatch { .. }),
        "expected PublishDirectoryMismatch, got {err:?}",
    );
}

/// `dependenciesMeta` mismatch (different `injected` flag) fails
/// the check. Two `None`s and `None`-vs-empty-object are both
/// considered equal — that's a separate happy-path case.
#[test]
fn dependencies_meta_mismatch_returns_dependencies_meta_mismatch() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      foo:"
        "        specifier: ^1.0.0"
        "        version: 1.0.0"
        "    dependenciesMeta:"
        "      foo:"
        "        injected: true"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": { "foo": "^1.0.0" }
    }"#,
    );
    let err = satisfies_package_manifest(importer, &manifest, ".").expect_err("should be stale");
    assert!(
        matches!(err, StalenessReason::DependenciesMetaMismatch { .. }),
        "expected DependenciesMetaMismatch, got {err:?}",
    );
}

/// `NoImporter` message renders with `importers["."]`-style
/// formatting, not `importers."."` (the previous `{:?}` debug-
/// format output). Caught in Copilot review on #450 — debug-format
/// quoting reads poorly for short keys like `.`.
#[test]
fn no_importer_message_uses_bracket_quoted_id() {
    let reason = StalenessReason::NoImporter { importer_id: ".".to_string() };
    let rendered = reason.to_string();
    assert!(rendered.contains(r#"importers["."]"#), "expected bracket-quoted id, got {rendered:?}");
    assert!(
        !rendered.contains(r#"importers.".""#),
        "must not use Rust debug-format quoting, got {rendered:?}",
    );
}

/// Pinpoint singular-vs-plural wording per bucket so the n==1 case
/// doesn't silently regress.
#[test]
fn spec_diff_display_uses_singular_for_count_of_one() {
    let mut diff = super::SpecDiff::default();
    diff.added.insert("foo".to_string(), "^1.0.0".to_string());
    let rendered = diff.to_string();
    assert!(
        rendered.contains("1 dependency was added: "),
        "expected singular wording for count of 1, got: {rendered:?}",
    );
    assert!(!rendered.contains("dependencies were added"));
}

/// `dependenciesMeta: {}` on the manifest with no `dependenciesMeta`
/// on the importer should match — empty object and absent are
/// equivalent. Mirrors upstream's `?? {}` coercion at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts#L56-L58>.
/// Ports the upstream test at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/test/satisfiesPackageManifest.ts#L232-L252>.
#[test]
fn dependencies_meta_empty_object_equivalent_to_absent() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      foo:"
        "        specifier: 1.0.0"
        "        version: 1.0.0"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    // Manifest has `dependenciesMeta: {}`; lockfile has none.
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": { "foo": "1.0.0" },
        "dependenciesMeta": {}
    }"#,
    );
    assert!(satisfies_package_manifest(importer, &manifest, ".").is_ok());
}

/// `publishDirectory` happy-path: lockfile and manifest agree on the
/// directory. Ports the upstream test at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/test/satisfiesPackageManifest.ts#L314-L334>.
#[test]
fn publish_directory_match_satisfies() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    publishDirectory: ./dist"
        "    dependencies:"
        "      foo:"
        "        specifier: 1.0.0"
        "        version: 1.0.0"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "publishConfig": { "directory": "./dist" },
        "dependencies": { "foo": "1.0.0" }
    }"#,
    );
    assert!(satisfies_package_manifest(importer, &manifest, ".").is_ok());
}

/// Same dep listed in both `dependencies` and `devDependencies` on
/// the manifest, only in `dependencies` on the lockfile — should
/// pass because upstream's per-field check filters out a dep from
/// `devDependencies` when it also exists in `dependencies` /
/// `optionalDependencies` (precedence: optional > prod > dev).
/// Mirrors upstream's `pkgDepNames` filter at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/src/satisfiesPackageManifest.ts#L69-L84>.
/// Ports the upstream test at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/test/satisfiesPackageManifest.ts#L211-L230>.
#[test]
fn same_dep_in_prod_and_dev_counts_under_prod() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      foo:"
        "        specifier: 1.0.0"
        "        version: 1.0.0"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    // Manifest lists foo under both prod and dev; lockfile records
    // it only under prod (the higher-precedence field).
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": { "foo": "1.0.0" },
        "devDependencies": { "foo": "1.0.0" }
    }"#,
    );
    assert!(
        satisfies_package_manifest(importer, &manifest, ".").is_ok(),
        "manifest listing foo in prod+dev must satisfy a lockfile that records it under prod only",
    );
}

/// Same dep in both `dependencies` and `optionalDependencies`:
/// optional wins precedence, lockfile records it only under
/// `optionalDependencies`. Verifies the precedence rule in the
/// other direction.
#[test]
fn same_dep_in_prod_and_optional_counts_under_optional() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    optionalDependencies:"
        "      foo:"
        "        specifier: 1.0.0"
        "        version: 1.0.0"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": { "foo": "1.0.0" },
        "optionalDependencies": { "foo": "1.0.0" }
    }"#,
    );
    assert!(
        satisfies_package_manifest(importer, &manifest, ".").is_ok(),
        "manifest listing foo in prod+optional must satisfy a lockfile that records it under optional only",
    );
}

/// Manifest has prod-only deps; lockfile has prod deps plus an
/// empty `devDependencies` map. Should satisfy — absent and empty
/// must be treated alike on the importer side too. Ports the
/// upstream test at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/verification/test/satisfiesPackageManifest.ts#L20-L31>.
#[test]
fn importer_empty_dev_dependencies_equivalent_to_absent() {
    let lockfile: Lockfile = serde_saphyr::from_str(text_block! {
        "lockfileVersion: '9.0'"
        "importers:"
        "  .:"
        "    dependencies:"
        "      foo:"
        "        specifier: ^1.0.0"
        "        version: 1.0.0"
        "    devDependencies: {}"
    })
    .expect("parse fixture lockfile");
    let importer = lockfile.root_project().expect("root importer present");
    let (_dir, manifest) = manifest_from_json(
        r#"{
        "name": "x",
        "version": "1.0.0",
        "dependencies": { "foo": "^1.0.0" }
    }"#,
    );
    assert!(satisfies_package_manifest(importer, &manifest, ".").is_ok());
}
