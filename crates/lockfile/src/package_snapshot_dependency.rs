use crate::{DependencyPath, PkgVerPeer};
use derive_more::{Display, From, TryInto};
use serde::{Deserialize, Serialize};

/// Value of [`PackageSnapshot::dependencies`](crate::PackageSnapshot::dependencies).
#[derive(Debug, Display, Clone, PartialEq, Eq, From, TryInto, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PackageSnapshotDependency {
    PkgVerPeer(PkgVerPeer),
    DependencyPath(DependencyPath),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;

    #[test]
    fn deserialize_to_correct_variants() {
        macro_rules! case {
            ($input:expr => $output:ident) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                let snapshot_dependency: PackageSnapshotDependency =
                    serde_yaml::from_str(input).unwrap();
                dbg!(&snapshot_dependency);
                assert!(matches!(&snapshot_dependency, PackageSnapshotDependency::$output(_)));
            }};
        }

        case!("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => PkgVerPeer);
        case!("1.21.3(react@17.0.2)" => PkgVerPeer);
        case!("1.21.3-rc.0(react@17.0.2)" => PkgVerPeer);
        case!("1.21.3" => PkgVerPeer);
        case!("1.21.3-rc.0" => PkgVerPeer);
        case!("/react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => DependencyPath);
        case!("/react-json-view@1.21.3(react@17.0.2)" => DependencyPath);
        case!("/react-json-view@1.21.3-rc.0(react@17.0.2)" => DependencyPath);
        case!("/react-json-view@1.21.3" => DependencyPath);
        case!("/react-json-view@1.21.3-rc.0" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3(react@17.0.2)" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3-rc.0(react@17.0.2)" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3" => DependencyPath);
        case!("registry.npmjs.com/react-json-view@1.21.3-rc.0" => DependencyPath);
        case!("/@docusaurus/react-loadable@5.5.2(react@17.0.2)" => DependencyPath);
        case!("/@docusaurus/react-loadable@5.5.2" => DependencyPath);
        case!("registry.npmjs.com/@docusaurus/react-loadable@5.5.2(react@17.0.2)" => DependencyPath);
        case!("registry.npmjs.com/@docusaurus/react-loadable@5.5.2" => DependencyPath);
    }

    #[test]
    fn string_matches_yaml() {
        fn case(input: &'static str) {
            eprintln!("CASE: {input:?}");
            let snapshot_dependency: PackageSnapshotDependency =
                serde_yaml::from_str(input).unwrap();
            dbg!(&snapshot_dependency);
            let received = snapshot_dependency.to_string().pipe(serde_yaml::Value::String);
            let expected: serde_yaml::Value = serde_yaml::from_str(input).unwrap();
            assert_eq!(&received, &expected);
        }

        case("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case("1.21.3(react@17.0.2)");
        case("1.21.3-rc.0(react@17.0.2)");
        case("1.21.3");
        case("1.21.3-rc.0");
        case("/react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case("/react-json-view@1.21.3(react@17.0.2)");
        case("/react-json-view@1.21.3-rc.0(react@17.0.2)");
        case("/react-json-view@1.21.3");
        case("/react-json-view@1.21.3-rc.0");
        case("registry.npmjs.com/react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case("registry.npmjs.com/react-json-view@1.21.3(react@17.0.2)");
        case("registry.npmjs.com/react-json-view@1.21.3-rc.0(react@17.0.2)");
        case("registry.npmjs.com/react-json-view@1.21.3");
        case("registry.npmjs.com/react-json-view@1.21.3-rc.0");
        case("/@docusaurus/react-loadable@5.5.2(react@17.0.2)");
        case("/@docusaurus/react-loadable@5.5.2");
        case("registry.npmjs.com/@docusaurus/react-loadable@5.5.2(react@17.0.2)");
        case("registry.npmjs.com/@docusaurus/react-loadable@5.5.2");
    }
}
