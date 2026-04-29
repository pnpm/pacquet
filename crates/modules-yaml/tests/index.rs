use pacquet_modules_yaml::{read_modules_manifest, write_modules_manifest};
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use std::{fs, path::Path};

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L10-L40
#[test]
fn write_modules_manifest_and_read_modules_manifest() {
    let temp_dir = tempfile::tempdir().expect("create temporary directory");
    let modules_dir = temp_dir.path();
    let modules_yaml = json!({
        "hoistedDependencies": {},
        "included": {
            "dependencies": true,
            "devDependencies": true,
            "optionalDependencies": true,
        },
        "ignoredBuilds": [],
        "layoutVersion": 1,
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
    });

    write_modules_manifest(modules_dir, &modules_yaml).expect("write manifest");
    let actual = read_modules_manifest(modules_dir).expect("read manifest");
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
    let modules_yaml = read_modules_manifest(&modules_dir)
        .expect("read manifest")
        .expect("modules manifest exists");

    assert_eq!(modules_yaml["publicHoistPattern"], json!(["*"]));
    assert_eq!(
        modules_yaml["hoistedDependencies"],
        json!({
            "/accepts/1.3.7": { "accepts": "public" },
            "/array-flatten/1.1.1": { "array-flatten": "public" },
            "/body-parser/1.19.0": { "body-parser": "public" },
        }),
    );
}

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L55-L66
#[test]
fn read_legacy_shamefully_hoist_false_manifest() {
    let modules_dir =
        env!("CARGO_MANIFEST_DIR").pipe(Path::new).join("tests/fixtures/old-no-shamefully-hoist");
    let modules_yaml = read_modules_manifest(&modules_dir)
        .expect("read manifest")
        .expect("modules manifest exists");

    assert_eq!(modules_yaml["publicHoistPattern"], json!([]));
    assert_eq!(
        modules_yaml["hoistedDependencies"],
        json!({
            "/accepts/1.3.7": { "accepts": "private" },
            "/array-flatten/1.1.1": { "array-flatten": "private" },
            "/body-parser/1.19.0": { "body-parser": "private" },
        }),
    );
}

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L68-L94
#[test]
fn write_modules_manifest_creates_node_modules_directory() {
    let temp_dir = tempfile::tempdir().expect("create temporary directory");
    let modules_dir = temp_dir.path().join("node_modules");
    let modules_yaml = json!({
        "hoistedDependencies": {},
        "included": {
            "dependencies": true,
            "devDependencies": true,
            "optionalDependencies": true,
        },
        "ignoredBuilds": [],
        "layoutVersion": 1,
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
    });

    write_modules_manifest(&modules_dir, &modules_yaml).expect("write manifest");
    let actual = read_modules_manifest(&modules_dir).expect("read manifest");
    assert_eq!(actual, Some(modules_yaml));
}

// Ported from https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/test/index.ts#L96-L99
#[test]
fn read_empty_modules_manifest_returns_none() {
    let modules_dir =
        env!("CARGO_MANIFEST_DIR").pipe(Path::new).join("tests/fixtures/empty-modules-yaml");
    let modules_yaml = read_modules_manifest(&modules_dir).expect("read manifest");
    assert_eq!(modules_yaml, None);
}
