use std::{collections::HashMap, env, ffi::OsStr, io::Write, path::PathBuf};

struct PackageJson {
    path: PathBuf,
}

impl PackageJson {
    pub fn new() -> Self {
        PackageJson {
            path: env::current_dir()
                .expect("current directory should exist")
                .as_path()
                .join("package.json"),
        }
    }

    pub fn create_if_needed(&self) {
        if self.path.exists() {
            return;
        }

        let folder_name = self.path.file_name().unwrap_or(OsStr::new(""));

        let mut file = std::fs::File::open(&self.path).unwrap();

        let mut contents = HashMap::new();
        contents.insert("name", folder_name.to_str().unwrap());

        let serialized = serde_json::to_string(&contents).unwrap();
        file.write_all(serialized.as_bytes()).unwrap();
    }
}
