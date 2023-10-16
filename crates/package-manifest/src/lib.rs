use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use derive_more::{Display, Error, From};
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use strum::IntoStaticStr;

#[derive(Debug, Display, Error, From, Diagnostic)]
#[non_exhaustive]
pub enum PackageManifestError {
    #[diagnostic(code(pacquet_package_manifest::serialization_error))]
    Serialization(serde_json::Error), // TODO: remove derive(From), split this variant

    #[diagnostic(code(pacquet_package_manifest::io_error))]
    Io(std::io::Error), // TODO: remove derive(From), split this variant

    #[display("package.json file already exists")]
    #[diagnostic(
        code(pacquet_package_manifest::already_exist_error),
        help("Your current working directory already has a package.json file.")
    )]
    AlreadyExist,

    #[from(ignore)] // TODO: remove this after derive(From) has been removed
    #[display("invalid attribute: {_0}")]
    #[diagnostic(code(pacquet_package_manifest::invalid_attribute))]
    InvalidAttribute(#[error(not(source))] String),

    #[from(ignore)] // TODO: remove this after derive(From) has been removed
    #[display("No package.json was found in {_0}")]
    #[diagnostic(code(pacquet_package_manifest::no_import_manifest_found))]
    NoImporterManifestFound(#[error(not(source))] String),

    #[from(ignore)] // TODO: remove this after derive(From) has been removed
    #[display("Missing script: {_0:?}")]
    #[diagnostic(code(pacquet_package_manifest::no_script_error))]
    NoScript(#[error(not(source))] String),
}

#[derive(Debug, Clone, Copy, PartialEq, IntoStaticStr)]
pub enum DependencyGroup {
    #[strum(serialize = "dependencies")]
    Prod,
    #[strum(serialize = "devDependencies")]
    Dev,
    #[strum(serialize = "optionalDependencies")]
    Optional,
    #[strum(serialize = "peerDependencies")]
    Peer,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BundleDependencies {
    Boolean(bool),
    List(Vec<String>),
}

/// Content of the `package.json` files and its path.
pub struct PackageManifest {
    path: PathBuf,
    value: Value, // TODO: convert this into a proper struct + an array of keys order
}

impl PackageManifest {
    fn create_init_package_json(name: &str) -> Value {
        json!({
            "name": name,
            "version": "1.0.0",
            "description": "",
            "main": "index.js",
            "scripts": {
              "test": "echo \"Error: no test specified\" && exit 1"
            },
            "keywords": [],
            "author": "",
            "license": "ISC"
        })
    }

    fn write_to_file(path: &Path) -> Result<(Value, String), PackageManifestError> {
        let name = path
            .parent()
            .and_then(|folder| folder.file_name())
            .and_then(|file_name| file_name.to_str())
            .unwrap_or("");
        let manifest = PackageManifest::create_init_package_json(name);
        let contents = serde_json::to_string_pretty(&manifest)?;
        fs::write(path, &contents)?; // TODO: forbid overwriting existing files
        Ok((manifest, contents))
    }

    fn read_from_file(path: &Path) -> Result<Value, PackageManifestError> {
        let contents = fs::read_to_string(path)?;
        serde_json::from_str(&contents).map_err(PackageManifestError::from)
    }

    pub fn init(path: &Path) -> Result<(), PackageManifestError> {
        if path.exists() {
            return Err(PackageManifestError::AlreadyExist);
        }
        let (_, contents) = PackageManifest::write_to_file(path)?;
        println!("Wrote to {path}\n\n{contents}", path = path.display());
        Ok(())
    }

    pub fn from_path(path: PathBuf) -> Result<PackageManifest, PackageManifestError> {
        if !path.exists() {
            return Err(PackageManifestError::NoImporterManifestFound(path.display().to_string()));
        }

        let value = PackageManifest::read_from_file(&path)?;
        Ok(PackageManifest { path, value })
    }

    pub fn create_if_needed(path: PathBuf) -> Result<PackageManifest, PackageManifestError> {
        let value = if path.exists() {
            PackageManifest::read_from_file(&path)?
        } else {
            PackageManifest::write_to_file(&path).map(|(value, _)| value)?
        };

        Ok(PackageManifest { path, value })
    }

    pub fn path(&self) -> &'_ Path {
        &self.path
    }

    pub fn value(&self) -> &'_ Value {
        &self.value
    }

    pub fn save(&self) -> Result<(), PackageManifestError> {
        let mut file = fs::File::create(&self.path)?;
        let contents = serde_json::to_string_pretty(&self.value)?;
        file.write_all(contents.as_bytes())?;
        Ok(())
    }

    pub fn dependencies<'a>(
        &'a self,
        groups: impl IntoIterator<Item = DependencyGroup> + 'a,
    ) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        // TODO: add error when `dependencies` is found to not be an object
        // TODO: add error when `version` is found to not be a string
        groups
            .into_iter()
            .flat_map(|group| self.value.get::<&str>(group.into()))
            .flat_map(|dependencies| dependencies.as_object())
            .flatten()
            .flat_map(|(name, version)| version.as_str().map(|value| (name.as_str(), value)))
    }

    pub fn bundle_dependencies(&self) -> Result<Option<BundleDependencies>, serde_json::Error> {
        self.value
            .get("bundleDependencies")
            .or_else(|| self.value.get("bundledDependencies"))
            .map(serde_json::Value::clone)
            .map(serde_json::from_value)
            .transpose()
    }

    pub fn add_dependency(
        &mut self,
        name: &str,
        version: &str,
        dependency_group: DependencyGroup,
    ) -> Result<(), PackageManifestError> {
        let dependency_type: &str = dependency_group.into();
        if let Some(field) = self.value.get_mut(dependency_type) {
            if let Some(dependencies) = field.as_object_mut() {
                dependencies.insert(name.to_string(), Value::String(version.to_string()));
            } else {
                return Err(PackageManifestError::InvalidAttribute(
                    "dependencies attribute should be an object".to_string(),
                ));
            }
        } else {
            let mut dependencies = Map::<String, Value>::new();
            dependencies.insert(name.to_string(), Value::String(version.to_string()));
            self.value[dependency_type] = Value::Object(dependencies);
        }
        Ok(())
    }

    pub fn script(
        &self,
        command: &str,
        if_present: bool, // TODO: split this function into 2, one with --if-present, one without
    ) -> Result<Option<&str>, PackageManifestError> {
        if let Some(script_str) = self
            .value
            .get("scripts")
            .and_then(|scripts| scripts.get(command))
            .and_then(|script| script.as_str())
        {
            return Ok(Some(script_str));
        }

        if if_present {
            Ok(None)
        } else {
            Err(PackageManifestError::NoScript(command.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs::read_to_string};

    use insta::assert_snapshot;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use tempfile::{tempdir, NamedTempFile};

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
}
