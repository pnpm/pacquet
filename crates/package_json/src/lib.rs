mod error;

use std::{
    collections::HashMap,
    env, fs,
    io::{Read, Write},
};

use serde::{Deserialize, Serialize};

use crate::error::PackageJsonError;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageJson {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    main: Option<String>,
    author: Option<String>,
    license: Option<String>,
    dependencies: Option<HashMap<String, String>>,
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

    pub fn create_if_needed() -> Result<PackageJson, PackageJsonError> {
        let path = env::current_dir()?.join("package.json");
        if path.exists() {
            let mut file = fs::File::create(&path)?;
            let package = PackageJson::new();
            let contents = serde_json::to_string_pretty(&package)?;
            file.write_all(&contents.as_bytes())?;
            return Ok(package);
        }

        let mut file = fs::File::open(&path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        return Ok(serde_json::from_str(&contents)?);
    }
}
