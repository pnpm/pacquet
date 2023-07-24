pub mod error;

use std::{
    collections::HashMap,
    convert::Into,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use serde_json::{json, Map, Value};

use crate::error::PackageJsonError;

#[derive(Debug, PartialEq)]
pub enum DependencyGroup {
    Default,
    Dev,
    Optional,
    Peer,
    Bundled,
}

impl From<DependencyGroup> for &str {
    fn from(value: DependencyGroup) -> Self {
        match value {
            DependencyGroup::Default => "dependencies",
            DependencyGroup::Dev => "devDependencies",
            DependencyGroup::Optional => "optionalDependencies",
            DependencyGroup::Peer => "peerDependencies",
            DependencyGroup::Bundled => "bundledDependencies",
        }
    }
}

pub struct PackageJson {
    path: PathBuf,
    value: Value,
}

impl PackageJson {
    pub fn new(path: PathBuf, value: Value) -> PackageJson {
        PackageJson { path, value }
    }

    fn write_to_file(path: &PathBuf) -> Result<Value, PackageJsonError> {
        let mut file = fs::File::create(path)?;
        let mut name = "";
        if let Some(folder) = path.parent() {
            // Set the default package name as the folder of the current directory
            name = folder.file_name().unwrap_or(OsStr::new("")).to_str().unwrap();
        }
        let package_json = json!({
            "name": name,
            "version": "1.0.0",
            "description": "",
            "main": "index.js",
            "author": "",
            "license": "MIT",
        });
        let contents = serde_json::to_string_pretty(&package_json)?;
        file.write_all(contents.as_bytes())?;
        Ok(package_json)
    }

    fn read_from_file(path: &PathBuf) -> Result<Value, PackageJsonError> {
        let mut file = fs::File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        Ok(serde_json::from_str(&contents)?)
    }

    pub fn init(path: &PathBuf) -> Result<(), PackageJsonError> {
        if path.exists() {
            return Err(PackageJsonError::AlreadyExist);
        }
        PackageJson::write_to_file(path)?;
        Ok(())
    }

    pub fn from_path(path: &PathBuf) -> Result<PackageJson, PackageJsonError> {
        if !path.exists() {
            return Err(PackageJsonError::NoImporterManifestFound(path.display().to_string()));
        }

        Ok(PackageJson { path: path.to_path_buf(), value: PackageJson::read_from_file(path)? })
    }

    pub fn create_if_needed(path: &PathBuf) -> Result<PackageJson, PackageJsonError> {
        let value = if path.exists() {
            PackageJson::read_from_file(path)?
        } else {
            PackageJson::write_to_file(path)?
        };

        Ok(PackageJson::new(path.to_path_buf(), value))
    }

    pub fn save(&mut self) -> Result<(), PackageJsonError> {
        let mut file = fs::File::create(&self.path)?;
        let contents = serde_json::to_string_pretty(&self.value)?;
        file.write_all(contents.as_bytes())?;
        Ok(())
    }

    pub fn get_dependencies(&self, group: DependencyGroup) -> HashMap<&str, &str> {
        let mut dependencies = HashMap::<&str, &str>::new();
        let group_key: &str = group.into();

        if let Some(value) = self.value.get(group_key) {
            if let Some(entries) = value.as_object() {
                for (key, value) in entries {
                    if let Some(value) = value.as_str() {
                        dependencies.insert(key.as_str(), value);
                    }
                }
            }
        }

        dependencies
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

    pub fn execute_command(&self, command: &str) -> Result<(), PackageJsonError> {
        match self
            .value
            .get("scripts")
            .unwrap_or(&Value::default())
            .get(command)
            .unwrap_or(&Value::default())
            .as_str()
        {
            Some(command) => {
                let mut cmd = Command::new(command)
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .stdin(Stdio::inherit())
                    .spawn()
                    .unwrap();

                cmd.wait().unwrap();

                Ok(())
            }
            None => Err(PackageJsonError::NoScript(command.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use tempfile::{tempdir, NamedTempFile};

    use super::*;
    use crate::DependencyGroup;

    #[test]
    fn test_dependency_group_into() {
        assert_eq!(<DependencyGroup as Into<&str>>::into(DependencyGroup::Default), "dependencies");
        assert_eq!(<DependencyGroup as Into<&str>>::into(DependencyGroup::Dev), "devDependencies");
        assert_eq!(
            <DependencyGroup as Into<&str>>::into(DependencyGroup::Optional),
            "optionalDependencies"
        );
        assert_eq!(
            <DependencyGroup as Into<&str>>::into(DependencyGroup::Peer),
            "peerDependencies"
        );
        assert_eq!(
            <DependencyGroup as Into<&str>>::into(DependencyGroup::Bundled),
            "bundledDependencies"
        );
    }

    #[test]
    fn init_should_throw_if_exists() {
        let tmp = NamedTempFile::new().unwrap();
        write!(tmp.as_file(), "hello world").unwrap();
        PackageJson::init(&tmp.path().to_path_buf()).expect_err("package.json already exist");
    }

    #[test]
    fn init_should_create_package_json_if_not_exist() {
        let dir = tempdir().unwrap();
        let tmp = dir.path().join("package.json");
        PackageJson::init(&tmp).unwrap();
        assert!(tmp.exists());
        assert!(tmp.is_file());
        assert_eq!(PackageJson::from_path(&tmp).unwrap().path, tmp);
    }

    #[test]
    fn should_add_dependency() {
        let dir = tempdir().unwrap();
        let tmp = dir.path().join("package.json");
        let mut package_json = PackageJson::create_if_needed(&tmp).unwrap();
        package_json.add_dependency("fastify", "1.0.0", DependencyGroup::Default).unwrap();

        let dependencies = package_json.get_dependencies(DependencyGroup::Default);
        assert!(dependencies.contains_key("fastify"));
        assert_eq!(dependencies.get("fastify").unwrap(), &"1.0.0");
        package_json.save().unwrap();
        assert!(read_to_string(tmp).unwrap().contains("fastify"));
    }

    #[test]
    fn should_throw_on_missing_command() {
        let dir = tempdir().unwrap();
        let tmp = dir.path().join("package.json");
        let package_json = PackageJson::create_if_needed(&tmp).unwrap();
        package_json.execute_command("test").expect_err("test command should not exist");
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
        let package_json = PackageJson::create_if_needed(&tmp.path().to_path_buf()).unwrap();
        package_json.execute_command("test").unwrap();
        package_json.execute_command("invalid").expect_err("invalid command should not exist");
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
        let package_json = PackageJson::create_if_needed(&tmp.path().to_path_buf()).unwrap();
        assert!(
            package_json.get_dependencies(DependencyGroup::Peer).contains_key("fast-querystring")
        );
        assert!(package_json.get_dependencies(DependencyGroup::Default).contains_key("fastify"));
    }
}
