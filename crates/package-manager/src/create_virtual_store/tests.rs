use super::{emit_warm_snapshot_progress, integrity_equal, snapshot_deps_equal};
use pacquet_lockfile::{
    LockfileResolution, PackageMetadata, PkgName, RegistryResolution, SnapshotDepRef, SnapshotEntry,
};
use pacquet_reporter::{LogEvent, ProgressMessage, Reporter};
use std::{collections::HashMap, sync::Mutex};

fn name(s: &str) -> PkgName {
    PkgName::parse(s).expect("parse pkg name")
}

fn metadata_with_integrity(integrity: &str) -> PackageMetadata {
    PackageMetadata {
        resolution: LockfileResolution::Registry(RegistryResolution {
            integrity: integrity.parse().expect("parse integrity"),
        }),
        engines: None,
        cpu: None,
        os: None,
        libc: None,
        deprecated: None,
        has_bin: None,
        prepare: None,
        bundled_dependencies: None,
        peer_dependencies: None,
        peer_dependencies_meta: None,
    }
}

fn snapshot_with_dep(child: &str, ref_str: &str) -> SnapshotEntry {
    let dep_ref: SnapshotDepRef = ref_str.parse().expect("parse SnapshotDepRef");
    SnapshotEntry {
        dependencies: Some(HashMap::from([(name(child), dep_ref)])),
        ..Default::default()
    }
}

/// `emit_warm_snapshot_progress` fires `resolved` then
/// `found_in_store` in that order for one (package_id, requester)
/// pair. Both events carry the same identifiers — pnpm's per-package
/// counter relies on the pair to pin the tick to the right package
/// row.
#[test]
fn emits_resolved_then_found_in_store_with_matching_identifiers() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().unwrap().push(event.clone());
        }
    }

    EVENTS.lock().unwrap().clear();
    emit_warm_snapshot_progress::<RecordingReporter>("react@18.0.0", "/proj");

    let captured = EVENTS.lock().unwrap();
    assert!(
        matches!(
            captured.as_slice(),
            [
                LogEvent::Progress(r),
                LogEvent::Progress(f),
            ] if matches!(
                &r.message,
                ProgressMessage::Resolved { package_id, requester }
                    if package_id == "react@18.0.0" && requester == "/proj"
            ) && matches!(
                &f.message,
                ProgressMessage::FoundInStore { package_id, requester }
                    if package_id == "react@18.0.0" && requester == "/proj",
            ),
        ),
        "warm-snapshot pair must be (Resolved, FoundInStore) with matching identifiers; got {captured:?}",
    );
}

/// `snapshot_deps_equal` is `true` when both `dependencies` and
/// `optionalDependencies` agree — matching upstream's `equals(...)`
/// pair. An absent map matches an empty map: pnpm canonicalises both
/// to `{}` via Ramda's `isEmpty`, so pacquet must too or warm
/// reinstalls would loop pointlessly when the lockfile drops the
/// optional-deps key.
#[test]
fn snapshot_deps_equal_treats_absent_and_empty_alike() {
    let absent = SnapshotEntry::default();
    let empty = SnapshotEntry {
        dependencies: Some(HashMap::new()),
        optional_dependencies: Some(HashMap::new()),
        ..Default::default()
    };
    assert!(snapshot_deps_equal(&absent, &empty));
    assert!(snapshot_deps_equal(&empty, &absent));
}

/// A real diff on `dependencies` flips the result to `false`. Upstream
/// gates the skip on this comparison; if pacquet treated mismatched
/// child-version edges as "no change", a warm reinstall would silently
/// keep an outdated symlink layout when the lockfile bumped a
/// transitive.
#[test]
fn snapshot_deps_equal_distinguishes_different_dependency_values() {
    let a = snapshot_with_dep("react", "17.0.2");
    let b = snapshot_with_dep("react", "18.0.0");
    assert!(!snapshot_deps_equal(&a, &b));
}

/// `optionalDependencies` participate in the comparison the same way
/// `dependencies` do — both upstream `equals` calls have to agree
/// before the skip fires.
#[test]
fn snapshot_deps_equal_distinguishes_different_optional_dependency_values() {
    let dep_ref: SnapshotDepRef = "1.0.0".parse().expect("parse dep ref");
    let a = SnapshotEntry {
        optional_dependencies: Some(HashMap::from([(name("react"), dep_ref.clone())])),
        ..Default::default()
    };
    let b = SnapshotEntry {
        optional_dependencies: Some(HashMap::from([(name("react-dom"), dep_ref)])),
        ..Default::default()
    };
    assert!(!snapshot_deps_equal(&a, &b));
}

/// `integrity_equal` mirrors upstream's `isIntegrityEqual` —
/// identical `integrity` strings on both sides means the cached
/// tarball is still valid, mismatched (or one-sided) integrities
/// force a re-fetch.
#[test]
fn integrity_equal_matches_when_integrities_agree() {
    let a = metadata_with_integrity(
        "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    );
    let b = metadata_with_integrity(
        "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    );
    assert!(integrity_equal(Some(&a), Some(&b)));
}

#[test]
fn integrity_equal_distinguishes_changed_integrities() {
    let a = metadata_with_integrity(
        "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    );
    let b = metadata_with_integrity(
        "sha512-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
    );
    assert!(!integrity_equal(Some(&a), Some(&b)));
}

/// Missing metadata on either side (a malformed lockfile, or the
/// snapshot referring to a `packages:` entry that was dropped)
/// collapses to `None` on the integrity lookup. Both sides `None`
/// stays "equal" so a directory/git resolution pair (whose integrity
/// is `None`) doesn't trip a spurious re-fetch.
#[test]
fn integrity_equal_treats_none_pair_as_equal() {
    assert!(integrity_equal(None, None));
}

#[test]
fn integrity_equal_treats_one_sided_missing_as_unequal() {
    let with_integrity = metadata_with_integrity(
        "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    );
    assert!(!integrity_equal(None, Some(&with_integrity)));
    assert!(!integrity_equal(Some(&with_integrity), None));
}
