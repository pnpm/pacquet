use super::{SymlinkDirectDependencies, SymlinkDirectDependenciesError};
use pacquet_lockfile::{Lockfile, ProjectSnapshot, ResolvedDependencyMap, ResolvedDependencySpec};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use pacquet_reporter::{
    AddedRoot, DependencyType, LogEvent, Reporter, RootLog, RootMessage, SilentReporter,
};
use pacquet_testing_utils::fs::is_symlink_or_junction;
use std::{collections::HashMap, fs, sync::Mutex};
use tempfile::tempdir;

/// `pnpm:root added` fires once per direct dependency, after the
/// symlink under `node_modules/` has been created. The captured
/// payload must mirror pnpm's wire shape: `name` and `realName`
/// from the lockfile key, `version` from the resolved snapshot
/// spec, and `dependencyType` keyed off the originating
/// [`DependencyGroup`]. `prefix` is the install root, mirroring
/// pnpm's emit at
/// <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/linking/direct-dep-linker/src/linkDirectDeps.ts#L131>.
#[test]
fn emits_pnpm_root_added_per_direct_dependency() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().unwrap().clear();

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().unwrap().push(event.clone());
        }
    }

    let dir = tempdir().unwrap();
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    let mut config = Npmrc::new();
    config.store_dir = dir.path().join("pacquet-store").into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    let config = config.leak();

    // The symlink targets must exist for the test to work on
    // Windows: pacquet's `symlink_dir` falls back to junctions
    // there, and `junction::create` requires the target directory
    // to exist. On Unix `symlink` doesn't care, but creating the
    // dirs here keeps the test platform-uniform.
    for (store_name, real_name) in
        [("fastify@4.0.0", "fastify"), ("@pnpm.e2e+dev-dep@1.2.3", "@pnpm.e2e/dev-dep")]
    {
        let target = virtual_store_dir.join(store_name).join("node_modules").join(real_name);
        fs::create_dir_all(&target).expect("create symlink target");
    }

    // One prod and one dev dep so we can assert that `dependencyType`
    // tracks the originating group across the iteration order.
    let mut prod = ResolvedDependencyMap::new();
    prod.insert(
        "fastify".parse().expect("parse fastify pkg name"),
        ResolvedDependencySpec {
            specifier: "^4.0.0".to_string(),
            version: "4.0.0".parse().expect("parse fastify version"),
        },
    );
    let mut dev = ResolvedDependencyMap::new();
    dev.insert(
        "@pnpm.e2e/dev-dep".parse().expect("parse dev pkg name"),
        ResolvedDependencySpec {
            specifier: "^1.2.3".to_string(),
            version: "1.2.3".parse().expect("parse dev version"),
        },
    );

    let project_snapshot = ProjectSnapshot {
        dependencies: Some(prod),
        dev_dependencies: Some(dev),
        ..ProjectSnapshot::default()
    };
    let mut importers = HashMap::new();
    importers.insert(Lockfile::ROOT_IMPORTER_KEY.to_string(), project_snapshot);

    let requester = "/proj";
    SymlinkDirectDependencies {
        config,
        importers: &importers,
        dependency_groups: [DependencyGroup::Prod, DependencyGroup::Dev],
        requester,
    }
    .run::<RecordingReporter>()
    .expect("symlink should succeed");

    // Both symlinks must land under `node_modules/` for the wire-
    // shape assertion below to be meaningful — an emit without the
    // matching FS effect would mask a real regression.
    let fastify_link = modules_dir.join("fastify");
    let dev_dep_link = modules_dir.join("@pnpm.e2e/dev-dep");
    eprintln!("fastify_link = {fastify_link:?}");
    assert!(is_symlink_or_junction(&fastify_link).unwrap());
    eprintln!("dev_dep_link = {dev_dep_link:?}");
    assert!(is_symlink_or_junction(&dev_dep_link).unwrap());

    let captured = EVENTS.lock().unwrap();
    let added: Vec<&AddedRoot> = captured
        .iter()
        .filter_map(|e| match e {
            LogEvent::Root(RootLog { message: RootMessage::Added { added, prefix }, .. }) => {
                assert_eq!(prefix, requester);
                Some(added)
            }
            _ => None,
        })
        .collect();
    assert_eq!(added.len(), 2, "one pnpm:root added per direct dep");

    // par_iter doesn't pin order, so look up by name. Both entries
    // must carry their version, the matching dependency type, and
    // `realName == name` (pacquet's lockfile snapshots don't
    // preserve npm-alias keys at this layer).
    let fastify = added.iter().find(|a| a.name == "fastify").expect("fastify added event missing");
    assert_eq!(fastify.real_name, "fastify");
    assert_eq!(fastify.version.as_deref(), Some("4.0.0"));
    assert_eq!(fastify.dependency_type, Some(DependencyType::Prod));

    let dev =
        added.iter().find(|a| a.name == "@pnpm.e2e/dev-dep").expect("dev-dep added event missing");
    assert_eq!(dev.real_name, "@pnpm.e2e/dev-dep");
    assert_eq!(dev.version.as_deref(), Some("1.2.3"));
    assert_eq!(dev.dependency_type, Some(DependencyType::Dev));

    drop(dir);
}

/// A malformed importer snapshot can list the same package name
/// across multiple sections (e.g. both `dependencies` and
/// `optionalDependencies`). The dedup pass must collapse that to
/// one entry — first-wins per the caller-supplied
/// `dependency_groups` order — so we don't race two
/// `symlink_package` calls to the same `node_modules/<name>` and
/// emit two `pnpm:root added` events for the same dep.
#[test]
fn duplicate_dep_across_groups_collapses_to_one_entry() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().unwrap().clear();

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().unwrap().push(event.clone());
        }
    }

    let dir = tempdir().unwrap();
    let project_root = dir.path().join("project");
    let modules_dir = project_root.join("node_modules");
    let virtual_store_dir = modules_dir.join(".pacquet");

    let mut config = Npmrc::new();
    config.store_dir = dir.path().join("pacquet-store").into();
    config.modules_dir = modules_dir.clone();
    config.virtual_store_dir = virtual_store_dir.clone();
    let config = config.leak();

    // Same name in `dependencies` and `optionalDependencies`. The
    // versions match here so the symlink target path is the same;
    // a real malformed lockfile that mismatched versions would also
    // collapse here, with first-wins picking the prod entry.
    let target = virtual_store_dir.join("fastify@4.0.0").join("node_modules").join("fastify");
    fs::create_dir_all(&target).expect("create symlink target");

    let mut prod = ResolvedDependencyMap::new();
    prod.insert(
        "fastify".parse().expect("parse fastify pkg name"),
        ResolvedDependencySpec {
            specifier: "^4.0.0".to_string(),
            version: "4.0.0".parse().expect("parse fastify version"),
        },
    );
    let mut optional = ResolvedDependencyMap::new();
    optional.insert(
        "fastify".parse().expect("parse fastify pkg name"),
        ResolvedDependencySpec {
            specifier: "^4.0.0".to_string(),
            version: "4.0.0".parse().expect("parse fastify version"),
        },
    );

    let project_snapshot = ProjectSnapshot {
        dependencies: Some(prod),
        optional_dependencies: Some(optional),
        ..ProjectSnapshot::default()
    };
    let mut importers = HashMap::new();
    importers.insert(Lockfile::ROOT_IMPORTER_KEY.to_string(), project_snapshot);

    SymlinkDirectDependencies {
        config,
        importers: &importers,
        // Prod first → first-wins gives `dependencyType: prod`.
        dependency_groups: [DependencyGroup::Prod, DependencyGroup::Optional],
        requester: "/proj",
    }
    .run::<RecordingReporter>()
    .expect("symlink should succeed");

    let captured = EVENTS.lock().unwrap();
    let added: Vec<&AddedRoot> = captured
        .iter()
        .filter_map(|e| match e {
            LogEvent::Root(RootLog { message: RootMessage::Added { added, .. }, .. }) => {
                Some(added)
            }
            _ => None,
        })
        .collect();
    assert_eq!(added.len(), 1, "duplicate dep across groups must collapse to one emit");
    assert_eq!(added[0].name, "fastify");
    assert_eq!(added[0].dependency_type, Some(DependencyType::Prod));

    drop(dir);
}

/// Missing root importer in the lockfile is the only error the
/// subroutine produces. Pin it so a future refactor that elides
/// the lookup doesn't silently turn into a no-op install.
#[test]
fn missing_root_importer_surfaces_as_error() {
    let dir = tempdir().unwrap();
    let mut config = Npmrc::new();
    config.store_dir = dir.path().join("pacquet-store").into();
    config.modules_dir = dir.path().join("project/node_modules");
    config.virtual_store_dir = dir.path().join("project/node_modules/.pacquet");
    let config = config.leak();

    let importers = HashMap::new();
    let result = SymlinkDirectDependencies {
        config,
        importers: &importers,
        dependency_groups: [DependencyGroup::Prod],
        requester: "/proj",
    }
    .run::<SilentReporter>();

    dbg!(&result);
    assert!(matches!(result, Err(SymlinkDirectDependenciesError::MissingRootImporter { .. })));
    drop(dir);
}
