use super::{ImporterDepVersion, ResolvedDependencySpec};
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
