use std::{env, path::Path, str::FromStr};

use serde::{de, Deserialize, Deserializer};

// This needs to be implemented because serde doesn't support default = "true" as
// a valid option, and throws  "failed to parse" error.
pub fn bool_true() -> bool {
    true
}

pub fn default_hoist_pattern() -> Vec<String> {
    vec!["*".to_string()]
}

pub fn default_public_hoist_pattern() -> Vec<String> {
    vec!["*eslint*".to_string(), "*prettier*".to_string()]
}

/// If the $PACQUET_HOME env variable is set, then $PACQUET_HOME/store
/// If the $XDG_DATA_HOME env variable is set, then $XDG_DATA_HOME/pacquet/store
/// On Windows: ~/AppData/Local/pacquet/store
/// On macOS: ~/Library/pacquet/store
/// On Linux: ~/.local/share/pacquet/store
pub fn default_store_dir() -> String {
    if let Ok(pacquet_home) = env::var("$PACQUET_HOME") {
        return Path::new(&pacquet_home).join("store").as_path().display().to_string();
    }

    if let Ok(xdg_data_home) = env::var("$XDG_DATA_HOME") {
        return Path::new(&xdg_data_home).join("pacquet/store").as_path().display().to_string();
    }

    // https://doc.rust-lang.org/std/env/consts/constant.OS.html
    match env::consts::OS {
        "linux" => "~/.local/share/pacquet/store".to_string(),
        "macos" => "~/Library/pacquet/store".to_string(),
        "windows" => "~/AppData/Local/pacquet/store".to_string(),
        _ => panic!("unsupported operating system: {0}", env::consts::OS),
    }
}

pub fn default_modules_dir() -> String {
    "node_modules".to_string()
}

pub fn default_virtual_store_dir() -> String {
    "node_modules/.pacquet".to_string()
}

pub fn default_registry() -> String {
    "https://registry.npmjs.org/".to_string()
}

pub fn default_modules_cache_max_age() -> u64 {
    10080
}

pub fn deserialize_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    bool::from_str(&s).map_err(de::Error::custom)
}

pub fn deserialize_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    u64::from_str(&s).map_err(de::Error::custom)
}
