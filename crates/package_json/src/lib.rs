use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use strum::IntoStaticStr;

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum PackageJsonError {
    #[error(transparent)]
    #[diagnostic(code(pacquet_package_json::serialization_error))]
    Serialization(#[from] serde_json::Error),

    #[error(transparent)]
    #[diagnostic(code(pacquet_package_json::io_error))]
    Io(#[from] std::io::Error),

    #[error("package.json file already exists")]
    #[diagnostic(
        code(pacquet_package_json::already_exist_error),
        help("Your current working directory already has a package.json file.")
    )]
    AlreadyExist,

    #[error("invalid attribute: {0}")]
    #[diagnostic(code(pacquet_package_json::invalid_attribute))]
    InvalidAttribute(String),

    #[error("No package.json was found in {0}")]
    #[diagnostic(code(pacquet_package_json::no_import_manifest_found))]
    NoImporterManifestFound(String),

    #[error("Missing script: \"{0}\"")]
    #[diagnostic(code(pacquet_package_json::no_script_error))]
    NoScript(String),
}

#[derive(Debug, Clone, Copy, PartialEq, IntoStaticStr)]
pub enum DependencyGroup {
    #[strum(serialize = "dependencies")]
    Default,
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

pub struct PackageJson {
    path: PathBuf,
    value: Value,
}

impl PackageJson {
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

    fn write_to_file(path: &Path) -> Result<(Value, String), PackageJsonError> {
        let name = path
            .parent()
            .and_then(|folder| folder.file_name())
            .and_then(|file_name| file_name.to_str())
            .unwrap_or("");
        let package_json = PackageJson::create_init_package_json(name);
        let contents = serde_json::to_string_pretty(&package_json)?;
        fs::write(path, &contents)?; // TODO: forbid overwriting existing files
        Ok((package_json, contents))
    }

    fn read_from_file(path: &Path) -> Result<Value, PackageJsonError> {
        let contents = fs::read_to_string(path)?;
        serde_json::from_str(&contents).map_err(PackageJsonError::from)
    }

    pub fn init(path: &Path) -> Result<(), PackageJsonError> {
        if path.exists() {
            return Err(PackageJsonError::AlreadyExist);
        }
        let (_, contents) = PackageJson::write_to_file(path)?;
        println!("Wrote to {path}\n\n{contents}", path = path.display());
        Ok(())
    }

    pub fn from_path(path: PathBuf) -> Result<PackageJson, PackageJsonError> {
        if !path.exists() {
            return Err(PackageJsonError::NoImporterManifestFound(path.display().to_string()));
        }

        let value = PackageJson::read_from_file(&path)?;
        Ok(PackageJson { path, value })
    }

    pub fn create_if_needed(path: PathBuf) -> Result<PackageJson, PackageJsonError> {
        let value = if path.exists() {
            PackageJson::read_from_file(&path)?
        } else {
            PackageJson::write_to_file(&path).map(|(value, _)| value)?
        };

        Ok(PackageJson { path, value })
    }

    pub fn save(&mut self) -> Result<(), PackageJsonError> {
        let mut file = fs::File::create(&self.path)?;
        let contents = serde_json::to_string_pretty(&self.value)?;
        file.write_all(contents.as_bytes())?;
        Ok(())
    }

    pub fn dependencies<'a>(
        &'a self,
        groups: impl IntoIterator<Item = DependencyGroup> + 'a,
    ) -> impl Iterator<Item = (&'a str, &'a str, DependencyGroup)> + 'a {
        // TODO: add error when `dependencies` is found to not be an object
        // TODO: add error when `version` is found to not be a string
        groups.into_iter().flat_map(move |group| {
            self.value
                .get::<&str>(group.into())
                .and_then(|dependencies| dependencies.as_object())
                .into_iter()
                .flatten()
                .flat_map(move |(name, version)| {
                    version.as_str().map(|value| (name.as_str(), value, group))
                })
        })
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
    ) -> Result<(), PackageJsonError> {
        let dependency_type: &str = dependency_group.into();
        if let Some(field) = self.value.get_mut(dependency_type) {
            if let Some(dependencies) = field.as_object_mut() {
                dependencies.insert(name.to_string(), Value::String(version.to_string()));
            } else {
                return Err(PackageJsonError::InvalidAttribute(
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
        if_present: bool,
    ) -> Result<Option<&str>, PackageJsonError> {
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
            Err(PackageJsonError::NoScript(command.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs::read_to_string};

    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use tempfile::{tempdir, NamedTempFile};

    use super::*;
    use crate::DependencyGroup;

    #[test]
    fn test_init_package_json_content() {
        let package_json = PackageJson::create_init_package_json("test");
        assert_snapshot!(serde_json::to_string_pretty(&package_json).unwrap());
    }

    #[test]
    fn init_should_throw_if_exists() {
        let tmp = NamedTempFile::new().unwrap();
        write!(tmp.as_file(), "hello world").unwrap();
        PackageJson::init(tmp.path()).expect_err("package.json already exist");
    }

    #[test]
    fn init_should_create_package_json_if_not_exist() {
        let dir = tempdir().unwrap();
        let tmp = dir.path().join("package.json");
        PackageJson::init(&tmp).unwrap();
        assert!(tmp.exists());
        assert!(tmp.is_file());
        assert_eq!(PackageJson::from_path(tmp.clone()).unwrap().path, tmp);
    }

    #[test]
    fn should_add_dependency() {
        let dir = tempdir().unwrap();
        let tmp = dir.path().join("package.json");
        let mut package_json = PackageJson::create_if_needed(tmp.clone()).unwrap();
        package_json.add_dependency("fastify", "1.0.0", DependencyGroup::Default).unwrap();

        let dependencies: HashMap<_, (_, _)> = package_json
            .dependencies([DependencyGroup::Default])
            .map(|(name, version, group)| (name, (version, group)))
            .collect();
        assert!(dependencies.contains_key("fastify"));
        assert_eq!(dependencies.get("fastify").unwrap(), &("1.0.0", DependencyGroup::Default));
        package_json.save().unwrap();
        assert!(read_to_string(tmp).unwrap().contains("fastify"));
    }

    #[test]
    fn should_throw_on_missing_command() {
        let dir = tempdir().unwrap();
        let tmp = dir.path().join("package.json");
        let package_json = PackageJson::create_if_needed(tmp).unwrap();
        package_json.script("dev", false).expect_err("dev command should not exist");
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
        let package_json = PackageJson::create_if_needed(tmp.path().to_path_buf()).unwrap();
        package_json.script("test", false).unwrap();
        package_json.script("invalid", false).expect_err("invalid command should not exist");
        package_json.script("invalid", true).unwrap();
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
        let package_json = PackageJson::create_if_needed(tmp.path().to_path_buf()).unwrap();
        let dependencies = |groups| {
            package_json
                .dependencies(groups)
                .map(|(name, version, group)| (name, (version, group)))
                .collect::<HashMap<_, (_, _)>>()
        };
        assert!(dependencies([DependencyGroup::Peer]).contains_key("fast-querystring"));
        assert!(dependencies([DependencyGroup::Default]).contains_key("fastify"));
    }
}
