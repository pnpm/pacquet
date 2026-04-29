use super::*;
use serde_json::json;

#[test]
fn bin_as_string_uses_package_name() {
    let manifest = json!({"name": "foo", "bin": "cli.js"});
    let commands = get_bins_from_package_manifest(&manifest, Path::new("/pkg/foo"));
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "foo");
    assert_eq!(commands[0].path, Path::new("/pkg/foo/cli.js"));
}

#[test]
fn bin_as_string_strips_scope() {
    let manifest = json!({"name": "@scope/foo", "bin": "cli.js"});
    let commands = get_bins_from_package_manifest(&manifest, Path::new("/pkg/foo"));
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "foo");
}

#[test]
fn bin_as_object_keeps_keys_and_strips_scope() {
    let manifest = json!({
        "name": "tool",
        "bin": {
            "tool": "bin/tool.js",
            "@scope/extra": "bin/extra.js",
        },
    });
    let mut commands = get_bins_from_package_manifest(&manifest, Path::new("/p"));
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].name, "extra");
    assert_eq!(commands[1].name, "tool");
}

#[test]
fn rejects_unsafe_bin_names() {
    let manifest = json!({
        "name": "x",
        "bin": {
            "good-name": "ok.js",
            "../bad": "evil.js",
            "with space": "no.js",
            "$": "dollar.js",
        },
    });
    let mut names: Vec<_> = get_bins_from_package_manifest(&manifest, Path::new("/p"))
        .into_iter()
        .map(|c| c.name)
        .collect();
    names.sort();
    assert_eq!(names, vec!["$".to_string(), "good-name".to_string()]);
}

#[test]
fn rejects_path_traversal_outside_package_root() {
    let manifest = json!({
        "name": "x",
        "bin": {"x": "../../../etc/passwd"},
    });
    let commands = get_bins_from_package_manifest(&manifest, Path::new("/pkg/x"));
    assert!(commands.is_empty(), "must reject `..`-escapes from pkg root");
}

#[test]
fn no_bin_field_returns_empty() {
    let manifest = json!({"name": "x"});
    assert!(get_bins_from_package_manifest(&manifest, Path::new("/p")).is_empty());
}

#[test]
fn pkg_owns_bin_default_rule() {
    assert!(pkg_owns_bin("foo", "foo"));
    assert!(!pkg_owns_bin("foo", "bar"));
}

#[test]
fn pkg_owns_bin_overrides() {
    assert!(pkg_owns_bin("npx", "npm"));
    assert!(pkg_owns_bin("pnpx", "pnpm"));
    assert!(pkg_owns_bin("pnpx", "@pnpm/exe"));
    assert!(!pkg_owns_bin("npx", "anything-else"));
}
