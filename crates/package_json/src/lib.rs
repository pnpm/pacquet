pub mod error;

use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

use crate::error::PackageJsonError;

#[derive(Debug, Serialize, Deserialize)]
pub struct PackageJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    main: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependencies: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "devDependencies")]
    dev_dependencies: Option<HashMap<String, String>>,
}

impl Default for PackageJson {
    fn default() -> Self {
        PackageJson::new()
    }
}

impl PackageJson {
    pub fn new() -> PackageJson {
        PackageJson {
            name: Some("".to_string()),
            version: Some("1.0.0".to_string()),
            description: Some("".to_string()),
            main: Some("index.js".to_string()),
            author: Some("".to_string()),
            license: Some("MIT".to_string()),
            dependencies: None,
            dev_dependencies: None,
        }
    }

    pub fn path() -> Result<PathBuf, PackageJsonError> {
        Ok(env::current_dir()?.join("package.json"))
    }

    fn write_to_file(path: &PathBuf) -> Result<PackageJson, PackageJsonError> {
        let mut file = fs::File::create(path)?;
        let mut package = PackageJson::new();
        if let Some(parent_folder) = path.parent() {
            package.name = Some(
                parent_folder
                    .file_name()
                    .unwrap_or(OsStr::new(""))
                    .to_str()
                    .unwrap_or("")
                    .to_string(),
            )
        }
        let contents = serde_json::to_string_pretty(&package)?;
        file.write_all(contents.as_bytes())?;
        Ok(package)
    }

    pub fn create() -> Result<PackageJson, PackageJsonError> {
        let path = PackageJson::path()?;
        if path.exists() {
            return Err(PackageJsonError::AlreadyExist);
        }

        PackageJson::write_to_file(&path)
    }

    pub fn create_if_needed() -> Result<PackageJson, PackageJsonError> {
        let path = PackageJson::path()?;
        if !path.exists() {
            return PackageJson::write_to_file(&path);
        }

        let mut file = fs::File::open(&path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        Ok(serde_json::from_str(&contents)?)
    }

    pub fn save(&mut self) -> Result<(), PackageJsonError> {
        let path = PackageJson::path()?;
        let mut file = fs::File::create(path)?;
        let contents = serde_json::to_string_pretty(&self)?;
        file.write_all(contents.as_bytes())?;
        Ok(())
    }

    pub fn add_dependency(&mut self, name: &str, version: &str) {
        if let Some(dependencies) = self.dependencies.as_mut() {
            dependencies.insert(name.to_string(), version.to_string());
        } else {
            let mut dependencies = HashMap::<String, String>::new();
            dependencies.insert(name.to_string(), version.to_string());
            self.dependencies = Some(dependencies);
        }
    }
}
