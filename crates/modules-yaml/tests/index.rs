use indexmap::IndexSet;
use pacquet_modules_yaml::{
    DepPath, FsCreateDirAll, FsReadToString, FsWrite, HoistKind, Modules, RealApi,
    read_modules_manifest, write_modules_manifest,
};
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

fn manifest_from_json(value: Value) -> Modules {
    serde_json::from_value(value).expect("deserialize Modules fixture")
}

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L10-L40
#[test]
fn write_modules_manifest_and_read_modules_manifest() {
    let temp_dir = tempfile::tempdir().expect("create temporary directory");
    let modules_dir = temp_dir.path();
    let modules_yaml = manifest_from_json(json!({
        "hoistedDependencies": {},
        "included": {
            "dependencies": true,
            "devDependencies": true,
            "optionalDependencies": true,
        },
        "ignoredBuilds": [],
        "layoutVersion": 5,
        "packageManager": "pnpm@2",
        "pendingBuilds": [],
        "publicHoistPattern": [],
        "prunedAt": "Thu, 01 Jan 1970 00:00:00 GMT",
        "registries": {
            "default": "https://registry.npmjs.org/",
        },
        "shamefullyHoist": false,
        "skipped": [],
        "storeDir": "/.pnpm-store",
        "virtualStoreDir": modules_dir.join(".pnpm"),
        "virtualStoreDirMaxLength": 120,
    }));

    write_modules_manifest::<RealApi>(modules_dir, modules_yaml.clone()).expect("write manifest");
    let actual = read_modules_manifest::<RealApi>(modules_dir).expect("read manifest");
    assert_eq!(actual, Some(modules_yaml));

    let raw =
        fs::read_to_string(modules_dir.join(".modules.yaml")).expect("read raw .modules.yaml");
    let raw: Value = serde_json::from_str(&raw).expect("parse raw .modules.yaml");
    let virtual_store_dir = raw
        .get("virtualStoreDir")
        .expect("virtualStoreDir is present")
        .as_str()
        .expect("virtualStoreDir is a string");
    assert_eq!(Path::new(virtual_store_dir).is_absolute(), cfg!(windows));
}

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L42-L53
#[test]
fn read_legacy_shamefully_hoist_true_manifest() {
    let modules_dir =
        env!("CARGO_MANIFEST_DIR").pipe(Path::new).join("tests/fixtures/old-shamefully-hoist");
    let manifest = read_modules_manifest::<RealApi>(&modules_dir)
        .expect("read manifest")
        .expect("modules manifest exists");

    assert_eq!(manifest.public_hoist_pattern.as_deref(), Some(&["*".to_string()][..]));

    let mut expected: BTreeMap<String, BTreeMap<String, HoistKind>> = BTreeMap::new();
    for dep_path in ["/accepts/1.3.7", "/array-flatten/1.1.1", "/body-parser/1.19.0"] {
        let alias = dep_path.split('/').nth(1).expect("dep path has alias segment").to_string();
        let mut entry = BTreeMap::new();
        entry.insert(alias, HoistKind::Public);
        expected.insert(dep_path.to_string(), entry);
    }
    assert_eq!(manifest.hoisted_dependencies, expected);
}

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L55-L66
#[test]
fn read_legacy_shamefully_hoist_false_manifest() {
    let modules_dir =
        env!("CARGO_MANIFEST_DIR").pipe(Path::new).join("tests/fixtures/old-no-shamefully-hoist");
    let manifest = read_modules_manifest::<RealApi>(&modules_dir)
        .expect("read manifest")
        .expect("modules manifest exists");

    assert_eq!(manifest.public_hoist_pattern.as_deref(), Some(&[][..]));

    let mut expected: BTreeMap<String, BTreeMap<String, HoistKind>> = BTreeMap::new();
    for dep_path in ["/accepts/1.3.7", "/array-flatten/1.1.1", "/body-parser/1.19.0"] {
        let alias = dep_path.split('/').nth(1).expect("dep path has alias segment").to_string();
        let mut entry = BTreeMap::new();
        entry.insert(alias, HoistKind::Private);
        expected.insert(dep_path.to_string(), entry);
    }
    assert_eq!(manifest.hoisted_dependencies, expected);
}

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L68-L94
#[test]
fn write_modules_manifest_creates_node_modules_directory() {
    let temp_dir = tempfile::tempdir().expect("create temporary directory");
    let modules_dir = temp_dir.path().join("node_modules");
    let modules_yaml = manifest_from_json(json!({
        "hoistedDependencies": {},
        "included": {
            "dependencies": true,
            "devDependencies": true,
            "optionalDependencies": true,
        },
        "ignoredBuilds": [],
        "layoutVersion": 5,
        "packageManager": "pnpm@2",
        "pendingBuilds": [],
        "publicHoistPattern": [],
        "prunedAt": "Thu, 01 Jan 1970 00:00:00 GMT",
        "registries": {
            "default": "https://registry.npmjs.org/",
        },
        "shamefullyHoist": false,
        "skipped": [],
        "storeDir": "/.pnpm-store",
        "virtualStoreDir": modules_dir.join(".pnpm"),
        "virtualStoreDirMaxLength": 120,
    }));

    write_modules_manifest::<RealApi>(&modules_dir, modules_yaml.clone()).expect("write manifest");
    let actual = read_modules_manifest::<RealApi>(&modules_dir).expect("read manifest");
    assert_eq!(actual, Some(modules_yaml));
}

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L96-L99
#[test]
fn read_empty_modules_manifest_returns_none() {
    let modules_dir =
        env!("CARGO_MANIFEST_DIR").pipe(Path::new).join("tests/fixtures/empty-modules-yaml");
    let modules_yaml = read_modules_manifest::<RealApi>(&modules_dir).expect("read manifest");
    assert_eq!(modules_yaml, None);
}

// The next three tests cover behavior branches that pnpm only exercises
// transitively via install-level integration tests in `pnpm/test/`
// (e.g., custom virtualStoreDir at
// https://github.com/pnpm/pnpm/blob/1819226b51/pnpm/test/monorepo/index.ts#L1467-L1545).
// Those install tests are gated on the install pipeline being ported, so
// these direct unit tests guard the behavior in the meantime.

/// Reading a manifest whose `virtualStoreDir` is already absolute must
/// preserve it verbatim, matching upstream
/// https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L66-L70.
#[test]
fn read_preserves_absolute_virtual_store_dir() {
    let temp_dir = tempfile::tempdir().expect("create temporary directory");
    let modules_dir = temp_dir.path().join("node_modules");
    fs::create_dir_all(&modules_dir).expect("create modules dir");
    let custom_store = temp_dir.path().join("custom-store");
    let raw = json!({ "virtualStoreDir": &custom_store, "layoutVersion": 5 }).to_string();
    fs::write(modules_dir.join(".modules.yaml"), raw).expect("write fixture");

    let manifest = read_modules_manifest::<RealApi>(&modules_dir)
        .expect("read manifest")
        .expect("manifest exists");
    assert_eq!(Path::new(&manifest.virtual_store_dir), custom_store);
}

/// `writeModules` sorts `skipped` in place before serializing,
/// matching upstream
/// https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L117.
#[test]
fn write_sorts_skipped_array() {
    let temp_dir = tempfile::tempdir().expect("create temporary directory");
    let modules_dir = temp_dir.path();
    let manifest = manifest_from_json(json!({
        "layoutVersion": 5,
        "skipped": ["zeta", "alpha", "mu"],
    }));

    write_modules_manifest::<RealApi>(modules_dir, manifest).expect("write manifest");
    let raw =
        fs::read_to_string(modules_dir.join(".modules.yaml")).expect("read raw .modules.yaml");
    let parsed: Value = serde_json::from_str(&raw).expect("parse raw .modules.yaml");
    assert_eq!(parsed["skipped"], json!(["alpha", "mu", "zeta"]));
}

// The next five tests use dependency injection to drive I/O outcomes that
// are awkward or impossible to provoke with the real filesystem. Each fake
// implements only the capability trait the function under test consumes,
// so a read fake never has to declare `write`. This is the
// interface-segregation refinement of the lumped `FsApi` pattern at
// https://github.com/KSXGitHub/parallel-disk-usage/blob/2aa39917f9/src/app/hdd.rs#L25-L35.

/// `read_modules_manifest` should map a non-`NotFound` I/O error from
/// `read_to_string` to `ReadModulesError::ReadFile`.
#[test]
fn read_propagates_non_not_found_io_error() {
    use std::io;
    struct FailingRead;
    impl FsReadToString for FailingRead {
        fn read_to_string(_: &Path) -> io::Result<String> {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "mocked"))
        }
    }

    let modules_dir = Path::new("/dev/null/unused");
    let err = read_modules_manifest::<FailingRead>(modules_dir).expect_err("expected error");
    eprintln!("error: {err}");
    assert!(matches!(err, pacquet_modules_yaml::ReadModulesError::ReadFile { .. }));
}

/// `read_modules_manifest` should surface a YAML parse failure as
/// `ReadModulesError::ParseYaml`.
#[test]
fn read_propagates_parse_error() {
    use std::io;
    struct BadYamlContent;
    impl FsReadToString for BadYamlContent {
        fn read_to_string(_: &Path) -> io::Result<String> {
            Ok("{ this is not valid yaml or json".to_string())
        }
    }

    let modules_dir = Path::new("/dev/null/unused");
    let err = read_modules_manifest::<BadYamlContent>(modules_dir).expect_err("expected error");
    eprintln!("error: {err}");
    assert!(matches!(err, pacquet_modules_yaml::ReadModulesError::ParseYaml { .. }));
}

/// A YAML document that parses to `null` should yield `Ok(None)`, matching
/// upstream's `if (!modulesRaw) return modulesRaw;` at
/// https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L55.
#[test]
fn read_returns_none_for_null_document() {
    use std::io;
    struct NullDocContent;
    impl FsReadToString for NullDocContent {
        fn read_to_string(_: &Path) -> io::Result<String> {
            Ok("null\n".to_string())
        }
    }

    let modules_dir = Path::new("/dev/null/unused");
    let result = read_modules_manifest::<NullDocContent>(modules_dir).expect("read manifest");
    assert_eq!(result, None);
}

/// `write_modules_manifest` should map a `create_dir_all` failure to
/// `WriteModulesError::CreateDir`. The fake still has to implement
/// `FsWrite` because the function bound includes it, but the body asserts
/// that `write` is never reached on this code path.
#[test]
fn write_propagates_create_dir_error() {
    use std::io;
    struct FailingMkdir;
    impl FsCreateDirAll for FailingMkdir {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "mocked"))
        }
    }
    impl FsWrite for FailingMkdir {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            unreachable!("write must not be called when create_dir_all fails");
        }
    }

    let modules_dir = Path::new("/dev/null/unused");
    let err = write_modules_manifest::<FailingMkdir>(modules_dir, Modules::default())
        .expect_err("expected error");
    eprintln!("error: {err}");
    assert!(matches!(err, pacquet_modules_yaml::WriteModulesError::CreateDir { .. }));
}

/// `write_modules_manifest` should map a `write` failure to
/// `WriteModulesError::WriteFile` after `create_dir_all` succeeds.
#[test]
fn write_propagates_write_error() {
    use std::io;
    struct FailingWrite;
    impl FsCreateDirAll for FailingWrite {
        fn create_dir_all(_: &Path) -> io::Result<()> {
            Ok(())
        }
    }
    impl FsWrite for FailingWrite {
        fn write(_: &Path, _: &[u8]) -> io::Result<()> {
            Err(io::Error::other("mocked write failure"))
        }
    }

    let modules_dir = Path::new("/dev/null/unused");
    let err = write_modules_manifest::<FailingWrite>(modules_dir, Modules::default())
        .expect_err("expected error");
    eprintln!("error: {err}");
    assert!(matches!(err, pacquet_modules_yaml::WriteModulesError::WriteFile { .. }));
}

/// `LayoutVersion` is a unit type pinned to `5`. A manifest whose
/// `layoutVersion` is any other number must fail at parse time. This is
/// stricter than upstream's `readModules`, which accepts any number
/// and defers the decision to `checkCompatibility` at
/// https://github.com/pnpm/pnpm/blob/1819226b51/installing/deps-installer/src/install/checkCompatibility/index.ts#L18-L22;
/// the end-to-end behavior matches because both code paths reject
/// incompatible manifests.
#[test]
fn read_rejects_incompatible_layout_version() {
    use std::io;
    struct LegacyVersion;
    impl FsReadToString for LegacyVersion {
        fn read_to_string(_: &Path) -> io::Result<String> {
            Ok("layoutVersion: 4\n".to_string())
        }
    }

    let modules_dir = Path::new("/dev/null/unused");
    let err = read_modules_manifest::<LegacyVersion>(modules_dir).expect_err("expected error");
    eprintln!("error: {err}");
    assert!(matches!(err, pacquet_modules_yaml::ReadModulesError::ParseYaml { .. }));
}

/// A null `publicHoistPattern` is removed before serializing because the
/// YAML writer fails on undefined fields upstream. The behavior matches
/// https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L123-L125.
#[test]
fn write_removes_null_public_hoist_pattern() {
    let temp_dir = tempfile::tempdir().expect("create temporary directory");
    let modules_dir = temp_dir.path();
    let manifest = manifest_from_json(json!({
        "layoutVersion": 5,
        "publicHoistPattern": null,
    }));

    write_modules_manifest::<RealApi>(modules_dir, manifest).expect("write manifest");
    let raw =
        fs::read_to_string(modules_dir.join(".modules.yaml")).expect("read raw .modules.yaml");
    let parsed: Value = serde_json::from_str(&raw).expect("parse raw .modules.yaml");
    assert!(
        parsed.get("publicHoistPattern").is_none(),
        "publicHoistPattern was kept after write: {parsed}",
    );
}

/// `DepPath` is a transparent newtype around `String`: on the wire it
/// is indistinguishable from a plain string, so `hoistedAliases` keys
/// and `ignoredBuilds` elements round-trip through JSON (and YAML) the
/// same way upstream's `as DepPath`-cast values do.
#[test]
fn dep_path_serializes_transparently() {
    let temp_dir = tempfile::tempdir().expect("create temporary directory");
    let modules_dir = temp_dir.path();
    let manifest = manifest_from_json(json!({
        "layoutVersion": 5,
        "hoistedAliases": {
            "/accepts/1.3.7": ["accepts"],
        },
        "ignoredBuilds": ["/sharp/0.32.0"],
        "publicHoistPattern": [],
    }));
    assert_eq!(
        manifest.hoisted_aliases.as_ref().and_then(|m| m.keys().next()),
        Some(&DepPath::from("/accepts/1.3.7".to_string())),
    );
    let expected_ignored: IndexSet<DepPath> =
        [DepPath::from("/sharp/0.32.0".to_string())].into_iter().collect();
    assert_eq!(manifest.ignored_builds.as_ref(), Some(&expected_ignored));

    write_modules_manifest::<RealApi>(modules_dir, manifest).expect("write manifest");
    let raw =
        fs::read_to_string(modules_dir.join(".modules.yaml")).expect("read raw .modules.yaml");
    let parsed: Value = serde_json::from_str(&raw).expect("parse raw .modules.yaml");
    assert_eq!(
        parsed["hoistedAliases"]["/accepts/1.3.7"],
        json!(["accepts"]),
        "DepPath key did not serialize as a plain string",
    );
    assert_eq!(
        parsed["ignoredBuilds"],
        json!(["/sharp/0.32.0"]),
        "DepPath element did not serialize as a plain string",
    );
}

/// `ignoredBuilds` deserializes into an [`IndexSet`], mirroring upstream's
/// `new Set<DepPath>(modulesRaw.ignoredBuilds)` normalization at
/// https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L64.
/// Duplicates are dropped, and insertion order is preserved so a
/// write-after-read round-trip leaves the on-disk array byte-stable
/// against an upstream-written manifest.
#[test]
fn ignored_builds_dedups_and_preserves_insertion_order() {
    use std::io;
    struct DupIgnored;
    impl FsReadToString for DupIgnored {
        fn read_to_string(_: &Path) -> io::Result<String> {
            Ok(concat!(
                "layoutVersion: 5\n",
                "ignoredBuilds:\n",
                "  - /b@1\n",
                "  - /a@1\n",
                "  - /b@1\n",
                "  - /c@1\n",
                "  - /a@1\n",
            )
            .to_string())
        }
    }

    let modules_dir = Path::new("/dev/null/unused");
    let manifest = read_modules_manifest::<DupIgnored>(modules_dir)
        .expect("read manifest")
        .expect("manifest exists");
    let ignored: Vec<&str> = manifest
        .ignored_builds
        .as_ref()
        .expect("ignored_builds present")
        .iter()
        .map(DepPath::as_str)
        .collect();
    assert_eq!(ignored, ["/b@1", "/a@1", "/c@1"]);
}
