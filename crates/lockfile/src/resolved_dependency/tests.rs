use super::{ImporterDepVersion, ResolvedDependencySpec};
use crate::PkgName;
use pretty_assertions::assert_eq;

/// Bare semver versions parse into `Regular` and round-trip through
/// the string form.
#[test]
fn parses_regular_version() {
    let parsed: ImporterDepVersion = "4.0.0".parse().unwrap();
    assert_eq!(parsed.as_regular().map(ToString::to_string), Some("4.0.0".to_string()));
    assert!(parsed.as_link_target().is_none());
    let serialized: String = parsed.into();
    assert_eq!(serialized, "4.0.0");
}

/// Peer-suffixed semver versions still parse into `Regular`.
#[test]
fn parses_regular_version_with_peer() {
    let parsed: ImporterDepVersion = "17.0.2(react@17.0.2)".parse().unwrap();
    assert!(matches!(parsed, ImporterDepVersion::Regular(_)));
}

/// `link:<path>` parses into `Link` and keeps the path verbatim
/// (without the `link:` prefix).
#[test]
fn parses_link_version() {
    let parsed: ImporterDepVersion = "link:../shared".parse().unwrap();
    assert_eq!(parsed.as_link_target(), Some("../shared"));
    assert!(parsed.as_regular().is_none());
    let serialized: String = parsed.into();
    assert_eq!(serialized, "link:../shared");
}

/// `link:` with an absolute path is preserved verbatim.
#[test]
fn parses_link_with_absolute_path() {
    let parsed: ImporterDepVersion = "link:/abs/sibling".parse().unwrap();
    assert_eq!(parsed.as_link_target(), Some("/abs/sibling"));
}

/// A `ResolvedDependencySpec` with a `link:` value round-trips
/// through serde, the load path the lockfile reader uses.
#[test]
fn resolved_spec_deserialize_link() {
    let yaml = "specifier: workspace:*\nversion: link:../shared\n";
    let spec: ResolvedDependencySpec = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(spec.specifier, "workspace:*");
    assert_eq!(spec.version.as_link_target(), Some("../shared"));
}

/// And the regular case round-trips too.
#[test]
fn resolved_spec_deserialize_regular() {
    let yaml = "specifier: ^4.0.0\nversion: 4.0.0\n";
    let spec: ResolvedDependencySpec = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(spec.specifier, "^4.0.0");
    assert!(spec.version.as_regular().is_some());
}

/// A scoped npm-alias (`@scope/name@version`) parses into `Alias`,
/// matching pnpm's `refToRelative` rule: a leading `@` always
/// indicates a full dep-path. Regression for the
/// `version: '@zkochan/js-yaml@0.0.11'` shape pnpm v11 writes for
/// `catalog:` deps that resolve to a scoped alias.
#[test]
fn parses_scoped_alias_version() {
    let parsed: ImporterDepVersion = "@zkochan/js-yaml@0.0.11".parse().unwrap();
    let alias = parsed.as_alias().expect("alias variant");
    assert_eq!(alias.name.to_string(), "@zkochan/js-yaml");
    assert_eq!(alias.suffix.to_string(), "0.0.11");
    assert!(parsed.as_regular().is_none());
    assert!(parsed.as_link_target().is_none());
    let serialized: String = parsed.into();
    assert_eq!(serialized, "@zkochan/js-yaml@0.0.11");
}

/// An unscoped npm-alias parses into `Alias` when the first `@`
/// appears before any `(` or `:` — the same discriminator pnpm uses.
#[test]
fn parses_unscoped_alias_version() {
    let parsed: ImporterDepVersion = "string-width@4.2.3".parse().unwrap();
    let alias = parsed.as_alias().expect("alias variant");
    assert_eq!(alias.name.to_string(), "string-width");
    assert_eq!(alias.suffix.to_string(), "4.2.3");
}

/// An alias with a peer suffix still parses into `Alias`; the peer
/// suffix is preserved as part of the alias's version-with-peer.
#[test]
fn parses_alias_version_with_peer() {
    let parsed: ImporterDepVersion = "react-dom@17.0.2(react@17.0.2)".parse().unwrap();
    let alias = parsed.as_alias().expect("alias variant");
    assert_eq!(alias.name.to_string(), "react-dom");
    assert_eq!(alias.suffix.to_string(), "17.0.2(react@17.0.2)");
    let serialized: String = parsed.into();
    assert_eq!(serialized, "react-dom@17.0.2(react@17.0.2)");
}

/// A `ResolvedDependencySpec` with an aliased version round-trips
/// through serde, the load path the lockfile reader uses. This is the
/// exact YAML shape that previously failed to parse.
#[test]
fn resolved_spec_deserialize_alias() {
    let yaml = "specifier: 'catalog:'\nversion: '@zkochan/js-yaml@0.0.11'\n";
    let spec: ResolvedDependencySpec = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(spec.specifier, "catalog:");
    let alias = spec.version.as_alias().expect("alias variant");
    assert_eq!(alias.name.to_string(), "@zkochan/js-yaml");
    assert_eq!(alias.suffix.to_string(), "0.0.11");
}

/// `resolved_key` returns `(importer_key, version)` for `Regular`,
/// the alias's own `(name, suffix)` for `Alias`, and `None` for
/// `Link`. The snapshot lookup, skipped check, and reachability BFS
/// all rely on this.
#[test]
fn resolved_key_returns_alias_name_for_alias_variant() {
    let importer_key: PkgName = "js-yaml".parse().unwrap();

    let regular: ImporterDepVersion = "4.0.0".parse().unwrap();
    let regular_key = regular.resolved_key(&importer_key).expect("regular key");
    assert_eq!(regular_key.name.to_string(), "js-yaml");
    assert_eq!(regular_key.suffix.to_string(), "4.0.0");

    let alias: ImporterDepVersion = "@zkochan/js-yaml@0.0.11".parse().unwrap();
    let alias_key = alias.resolved_key(&importer_key).expect("alias key");
    assert_eq!(alias_key.name.to_string(), "@zkochan/js-yaml");
    assert_eq!(alias_key.suffix.to_string(), "0.0.11");

    let link: ImporterDepVersion = "link:../shared".parse().unwrap();
    assert!(link.resolved_key(&importer_key).is_none());
}
