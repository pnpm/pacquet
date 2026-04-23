use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{
    DependencyPath, LockfileResolution, PackageSnapshot, PackageSnapshotDependency, PkgName,
    PkgNameVerPeer, PkgVerPeer, RegistryResolution,
};
use pacquet_registry::PackageVersion;
use std::collections::HashMap;

/// Flags that cannot be derived from a [`PackageVersion`] alone and must be
/// provided by the installer based on how the package was reached.
#[derive(Debug, Clone, Copy, Default)]
pub struct SnapshotFlags {
    /// `true` if every path from the root to this package goes through a
    /// `devDependencies` entry.
    pub dev: bool,
    /// `true` if the package is an optional dependency.
    pub optional: bool,
}

/// Error type of [`build_package_snapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum BuildSnapshotError {
    #[display(
        "Package `{name}@{version}` was returned from the registry without an `integrity` field; cannot build a lockfile entry for it."
    )]
    #[diagnostic(code(pacquet_package_manager::build_snapshot::missing_integrity))]
    MissingIntegrity { name: String, version: String },

    #[display("Failed to parse package name `{name}`: {source}")]
    #[diagnostic(code(pacquet_package_manager::build_snapshot::parse_name))]
    ParseName {
        name: String,
        #[error(source)]
        source: pacquet_lockfile::ParsePkgNameError,
    },
}

/// Build the lockfile `DependencyPath` for a package installed from the default
/// registry (no custom registry, no peer suffix).
pub fn registry_dependency_path(
    package: &PackageVersion,
) -> Result<DependencyPath, BuildSnapshotError> {
    let name = PkgName::parse(package.name.as_str())
        .map_err(|source| BuildSnapshotError::ParseName { name: package.name.clone(), source })?;
    let peer = format!("{}", package.version)
        .parse::<PkgVerPeer>()
        .expect("PackageVersion.version always serializes to a valid PkgVerPeer");
    Ok(DependencyPath { custom_registry: None, package_specifier: PkgNameVerPeer::new(name, peer) })
}

/// Convert a [`PackageVersion`] into a ([`DependencyPath`], [`PackageSnapshot`]) pair,
/// suitable for insertion into a `pnpm-lock.yaml`'s `packages` map.
///
/// `resolved_dependencies` maps each of this package's declared dependency
/// names to the version-with-peer-suffix that was actually picked by the
/// resolver. Callers that install without peer resolution may pass empty peer
/// suffixes.
pub fn build_package_snapshot(
    package: &PackageVersion,
    resolved_dependencies: &HashMap<String, PkgVerPeer>,
    flags: SnapshotFlags,
) -> Result<(DependencyPath, PackageSnapshot), BuildSnapshotError> {
    let dependency_path = registry_dependency_path(package)?;

    let integrity =
        package.dist.integrity.clone().ok_or_else(|| BuildSnapshotError::MissingIntegrity {
            name: package.name.clone(),
            version: package.version.to_string(),
        })?;

    let mut dependencies: HashMap<PkgName, PackageSnapshotDependency> = HashMap::new();
    for (dep_name, ver_peer) in resolved_dependencies {
        let parsed = PkgName::parse(dep_name.as_str())
            .map_err(|source| BuildSnapshotError::ParseName { name: dep_name.clone(), source })?;
        dependencies.insert(parsed, PackageSnapshotDependency::PkgVerPeer(ver_peer.clone()));
    }

    let snapshot = PackageSnapshot {
        resolution: LockfileResolution::Registry(RegistryResolution { integrity }),
        id: None,
        name: None,
        version: None,
        engines: None,
        cpu: None,
        os: None,
        libc: None,
        deprecated: None,
        has_bin: None,
        prepare: None,
        requires_build: None,
        bundled_dependencies: None,
        peer_dependencies: None,
        peer_dependencies_meta: None,
        dependencies: (!dependencies.is_empty()).then_some(dependencies),
        optional_dependencies: None,
        transitive_peer_dependencies: None,
        dev: Some(flags.dev),
        optional: Some(flags.optional),
    };

    Ok((dependency_path, snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_semver::Version;
    use pacquet_registry::{PackageDistribution, PackageVersion};
    use pretty_assertions::assert_eq;
    use ssri::Integrity;

    fn integrity(s: &str) -> Integrity {
        s.parse().expect("parse integrity string")
    }

    fn make_package(name: &str, version: &str) -> PackageVersion {
        PackageVersion {
            name: name.to_string(),
            version: version.parse::<Version>().expect("parse semver"),
            dist: PackageDistribution {
                integrity: Some(integrity(
                    "sha512-TIE61hcgbI/SlJh/0c1sT1SZbBlpg7WiZcs65WPJhoIZQPhH1SCpcGA7LgrVXT15lwN3HV4GQM/MJ9aKEn3Qfg==",
                )),
                shasum: None,
                tarball: format!("https://registry.npmjs.org/{name}/-/{name}-{version}.tgz"),
                file_count: None,
                unpacked_size: None,
            },
            dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
        }
    }

    #[test]
    fn builds_dependency_path_with_no_registry_and_no_peer_suffix() {
        let pkg = make_package("react", "17.0.2");
        let dep_path = registry_dependency_path(&pkg).unwrap();
        assert_eq!(dep_path.to_string(), "/react@17.0.2");
    }

    #[test]
    fn builds_dependency_path_for_scoped_name() {
        let pkg = make_package("@types/node", "18.7.19");
        let dep_path = registry_dependency_path(&pkg).unwrap();
        assert_eq!(dep_path.to_string(), "/@types/node@18.7.19");
    }

    #[test]
    fn builds_snapshot_with_registry_resolution_and_flags() {
        let pkg = make_package("lodash", "4.17.21");
        let (dep_path, snapshot) = build_package_snapshot(
            &pkg,
            &HashMap::new(),
            SnapshotFlags { dev: true, optional: false },
        )
        .unwrap();

        assert_eq!(dep_path.to_string(), "/lodash@4.17.21");
        assert!(matches!(snapshot.resolution, LockfileResolution::Registry(_)));
        assert_eq!(snapshot.dev, Some(true));
        assert_eq!(snapshot.optional, Some(false));
        assert!(snapshot.dependencies.is_none());
    }

    #[test]
    fn builds_snapshot_with_resolved_dependencies() {
        let pkg = make_package("react-dom", "17.0.2");
        let mut resolved = HashMap::new();
        resolved.insert("react".to_string(), "17.0.2".parse::<PkgVerPeer>().unwrap());

        let (_, snapshot) =
            build_package_snapshot(&pkg, &resolved, SnapshotFlags::default()).unwrap();

        let deps = snapshot.dependencies.expect("dependencies should be populated");
        assert_eq!(deps.len(), 1);
        let react_key = PkgName::parse("react").unwrap();
        match deps.get(&react_key).expect("react entry") {
            PackageSnapshotDependency::PkgVerPeer(v) => assert_eq!(v.to_string(), "17.0.2"),
            other => panic!("expected PkgVerPeer, got {other:?}"),
        }
    }

    #[test]
    fn returns_error_when_integrity_is_missing() {
        let mut pkg = make_package("broken", "1.0.0");
        pkg.dist.integrity = None;

        let err = build_package_snapshot(&pkg, &HashMap::new(), SnapshotFlags::default())
            .expect_err("should fail without integrity");
        assert!(matches!(err, BuildSnapshotError::MissingIntegrity { .. }));
    }
}
