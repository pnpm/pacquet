use std::{collections::HashMap, fs::read_to_string};

use insta::assert_snapshot;
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use tempfile::{NamedTempFile, tempdir};

use super::*;
use crate::DependencyGroup;

#[test]
fn test_init_package_json_content() {
    let manifest = PackageManifest::create_init_package_json("test");
    assert_snapshot!(serde_json::to_string_pretty(&manifest).unwrap());
}

#[test]
fn init_should_throw_if_exists() {
    let tmp = NamedTempFile::new().unwrap();
    write!(tmp.as_file(), "hello world").unwrap();
    PackageManifest::init(tmp.path()).expect_err("package.json already exist");
}

#[test]
fn init_should_create_package_json_if_not_exist() {
    let dir = tempdir().unwrap();
    let tmp = dir.path().join("package.json");
    PackageManifest::init(&tmp).unwrap();
    assert!(tmp.exists());
    assert!(tmp.is_file());
    assert_eq!(PackageManifest::from_path(tmp.clone()).unwrap().path, tmp);
}

#[test]
fn should_add_dependency() {
    let dir = tempdir().unwrap();
    let tmp = dir.path().join("package.json");
    let mut manifest = PackageManifest::create_if_needed(tmp.clone()).unwrap();
    manifest.add_dependency("fastify", "1.0.0", DependencyGroup::Prod).unwrap();

    let dependencies: HashMap<_, _> = manifest.dependencies([DependencyGroup::Prod]).collect();
    assert!(dependencies.contains_key("fastify"));
    assert_eq!(dependencies.get("fastify").unwrap(), &"1.0.0");
    manifest.save().unwrap();
    assert!(read_to_string(tmp).unwrap().contains("fastify"));
}

#[test]
fn should_throw_on_missing_command() {
    let dir = tempdir().unwrap();
    let tmp = dir.path().join("package.json");
    let manifest = PackageManifest::create_if_needed(tmp).unwrap();
    manifest.script("dev", false).expect_err("dev command should not exist");
}

#[test]
fn should_execute_a_command() {
    let data = r#"
    {
        "scripts": {
            "test": "echo"
        }
    }
    "#;
    let tmp = NamedTempFile::new().unwrap();
    write!(tmp.as_file(), "{}", data).unwrap();
    let manifest = PackageManifest::create_if_needed(tmp.path().to_path_buf()).unwrap();
    manifest.script("test", false).unwrap();
    manifest.script("invalid", false).expect_err("invalid command should not exist");
    manifest.script("invalid", true).unwrap();
}

#[test]
fn get_dependencies_should_return_peers() {
    let data = r#"
    {
        "dependencies": {
            "fastify": "1.0.0"
        },
        "peerDependencies": {
            "fast-querystring": "1.0.0"
        }
    }
    "#;
    let tmp = NamedTempFile::new().unwrap();
    write!(tmp.as_file(), "{}", data).unwrap();
    let manifest = PackageManifest::create_if_needed(tmp.path().to_path_buf()).unwrap();
    let dependencies = |groups| manifest.dependencies(groups).collect::<HashMap<_, _>>();
    assert!(dependencies([DependencyGroup::Peer]).contains_key("fast-querystring"));
    assert!(dependencies([DependencyGroup::Prod]).contains_key("fastify"));
}

#[test]
fn bundle_dependencies() {
    fn bundle_list<List>(list: List) -> BundleDependencies
    where
        List: IntoIterator,
        List::Item: Into<String>,
    {
        list.into_iter().map(Into::into).collect::<Vec<_>>().pipe(BundleDependencies::List)
    }

    macro_rules! case {
        ($input:expr => $output:expr) => {{
            let data = $input;
            eprintln!("CASE: {data}");
            let tmp = NamedTempFile::new().unwrap();
            write!(tmp.as_file(), "{}", data).unwrap();
            let manifest = PackageManifest::create_if_needed(tmp.path().to_path_buf()).unwrap();
            let bundle = manifest.bundle_dependencies().unwrap();
            assert_eq!(bundle, $output);
        }};
    }

    case!(r#"{ "bundleDependencies": ["foo", "bar"] }"# => Some(bundle_list(["foo", "bar"])));
    case!(r#"{ "bundledDependencies": ["foo", "bar"] }"# => Some(bundle_list(["foo", "bar"])));
    case!(r#"{ "bundleDependencies": false }"# => false.pipe(BundleDependencies::Boolean).pipe(Some));
    case!(r#"{ "bundledDependencies": false }"# => false.pipe(BundleDependencies::Boolean).pipe(Some));
    case!(r#"{ "bundleDependencies": true }"# => true.pipe(BundleDependencies::Boolean).pipe(Some));
    case!(r#"{ "bundledDependencies": true }"# => true.pipe(BundleDependencies::Boolean).pipe(Some));
    case!(r#"{}"# => None);
}

#[test]
fn resolve_registry_dependency_passes_through_plain_specs() {
    for (key, spec) in [
        ("foo", "^1.0.0"),
        ("foo", "1.2.3"),
        ("foo", "latest"),
        ("@scope/foo", "^1.0.0"),
        ("foo", "*"),
        ("foo", ">=1 <2"),
    ] {
        assert_eq!(
            PackageManifest::resolve_registry_dependency(key, spec),
            (key, spec),
            "plain spec ({key:?}, {spec:?}) should pass through unchanged",
        );
    }
}

#[test]
fn resolve_registry_dependency_strips_npm_alias_prefix() {
    assert_eq!(
        PackageManifest::resolve_registry_dependency("ansi-strip", "npm:strip-ansi@^6.0.1"),
        ("strip-ansi", "^6.0.1"),
    );
}

#[test]
fn resolve_registry_dependency_handles_scoped_target() {
    assert_eq!(
        PackageManifest::resolve_registry_dependency("react17", "npm:@types/react@^17.0.49"),
        ("@types/react", "^17.0.49"),
    );
}

#[test]
fn resolve_registry_dependency_handles_pinned_version() {
    assert_eq!(
        PackageManifest::resolve_registry_dependency("foo-cjs", "npm:foo@1.2.3"),
        ("foo", "1.2.3"),
    );
}

#[test]
fn resolve_registry_dependency_unversioned_npm_alias_defaults_to_latest() {
    // `npm:foo` and `npm:@scope/foo` mean "latest" in pnpm.
    assert_eq!(
        PackageManifest::resolve_registry_dependency("foo-cjs", "npm:foo"),
        ("foo", "latest"),
    );
    assert_eq!(
        PackageManifest::resolve_registry_dependency("react17", "npm:@types/react"),
        ("@types/react", "latest"),
    );
}

#[test]
fn resolve_registry_dependency_picks_last_at_for_alias() {
    // Mirrors pnpm's `lastIndexOf('@')` so prerelease/build metadata
    // containing `@` would still be split at the *final* `@`.
    assert_eq!(
        PackageManifest::resolve_registry_dependency("foo-rc", "npm:@scope/foo@1.0.0-rc.1",),
        ("@scope/foo", "1.0.0-rc.1"),
    );
}
