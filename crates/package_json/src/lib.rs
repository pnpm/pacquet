use std::{collections::HashMap, env, ffi::OsStr, io::Write, path::PathBuf};

use serde_json;

pub struct PackageJson {
    path: PathBuf,
}

impl PackageJson {
    pub fn from_current_directory() -> Self {
        PackageJson {
            path: env::current_dir()
                .expect("current directory should exist")
                .as_path()
                .join("package.json"),
        }
    }

    pub fn from_path(path: PathBuf) -> Self {
        PackageJson { path }
    }

    pub fn create_if_needed(&self) {
        if self.path.exists() {
            return;
        }

        let folder_name = self
            .path
            .parent()
            .expect("should have a parent folder")
            .file_name()
            .unwrap_or(OsStr::new(""));

        let mut file = std::fs::File::create(&self.path).unwrap();
        let empty_object: HashMap<String, String> = HashMap::new();
        let mut scripts = HashMap::new();
        scripts.insert("test", "echo \"Error: no test specified\" && exit 1");

        let mut contents = HashMap::new();
        contents.insert("name", serde_json::to_value(folder_name.to_str().unwrap()).unwrap());
        contents.insert("version", serde_json::to_value("1.0.0").unwrap());
        contents.insert("description", serde_json::to_value("").unwrap());
        contents.insert("main", serde_json::to_value("index.js").unwrap());
        contents.insert("author", serde_json::to_value("").unwrap());
        contents.insert("license", serde_json::to_value("MIT").unwrap());
        contents.insert("scripts", serde_json::to_value(&scripts).unwrap());
        contents.insert("dependencies", serde_json::to_value(&empty_object).unwrap());
        contents.insert("devDependencies", serde_json::to_value(&empty_object).unwrap());

        let serialized = serde_json::to_string_pretty(&contents).unwrap();
        file.write_all(serialized.as_bytes()).unwrap();
    }
}
