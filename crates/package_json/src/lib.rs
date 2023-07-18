pub mod error;

use std::{
    convert::Into,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use serde_json::{json, Map, Value};

use crate::error::PackageJsonError;

pub struct PackageJson {
    path: PathBuf,
    value: Value,
}

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
