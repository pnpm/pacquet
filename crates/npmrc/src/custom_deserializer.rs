use std::{env, path::PathBuf, str::FromStr};

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

/// Get absolute path with home_dir
pub fn get_absolute_path_with_home_dir(relate_path: Option<String>) -> PathBuf {
    let home_dir = dirs::home_dir().expect("Home directory is not available");
    match relate_path {
        Some(p) => {
            // prefix should start with ~/
            if let Some(path_without_tilde) = p.strip_prefix("~/") {
                return home_dir.join(path_without_tilde);
            }
            PathBuf::from(p)
        }
        None => home_dir,
    }
}

/// If the $PACQUET_HOME env variable is set, then $PACQUET_HOME/store
/// If the $XDG_DATA_HOME env variable is set, then $XDG_DATA_HOME/pacquet/store
/// On Windows: ~/AppData/Local/pacquet/store
/// On macOS: ~/Library/pacquet/store
/// On Linux: ~/.local/share/pacquet/store
pub fn default_store_dir() -> PathBuf {
    if let Ok(pacquet_home) = env::var("PACQUET_HOME") {
        return get_absolute_path_with_home_dir(Some(pacquet_home)).join("store");
    }

    if let Ok(xdg_data_home) = env::var("XDG_DATA_HOME") {
        return get_absolute_path_with_home_dir(Some(xdg_data_home)).join("pacquet/store");
    }

    // https://doc.rust-lang.org/std/env/consts/constant.OS.html
    match env::consts::OS {
        "linux" => get_absolute_path_with_home_dir(None).join(".local/share/pacquet/store"),
        "macos" => get_absolute_path_with_home_dir(None).join("Library/pacquet/store"),
        "windows" => get_absolute_path_with_home_dir(None).join("AppData/Local/pacquet/store"),
        _ => panic!("unsupported operating system: {}", env::consts::OS),
    }
}

pub fn default_modules_dir() -> PathBuf {
    env::current_dir().expect("current directory is unavailable").join("node_modules")
}

pub fn default_virtual_store_dir() -> PathBuf {
    env::current_dir().expect("current directory is unavailable").join("node_modules/.pacquet")
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

pub fn deserialize_pathbuf<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    let path = PathBuf::from_str(&s).map_err(de::Error::custom)?;

    if path.is_absolute() {
        return Ok(path);
    }

    Ok(env::current_dir().map_err(de::Error::custom)?.join(path))
}

/// This deserializer adds a trailing "/" if not exist to make our life easier.
pub fn deserialize_registry<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    if s.ends_with('/') {
        return Ok(s);
    }

    Ok(format!("{s}/"))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use std::{env, path::Path};

    use super::*;
    #[test]
    fn test_default_store_dir_with_pac_env() {
        env::set_var("PACQUET_HOME", "/tmp/pacquet_home");
        let store_dir = default_store_dir();
        assert_eq!(store_dir, Path::new("/tmp/pacquet_home/store"));
        env::remove_var("PACQUET_HOME");
    }

    #[test]
    fn test_default_store_dir_with_xdg_env() {
        env::set_var("XDG_DATA_HOME", "/tmp/xdg_data_home");
        let store_dir = default_store_dir();
        assert_eq!(store_dir, Path::new("/tmp/xdg_data_home/pacquet/store"));
        env::remove_var("XDG_DATA_HOME");
    }

    #[test]
    fn test_get_absolute_path_with_home_dir_none() {
        let home_dir = dirs::home_dir().unwrap();
        let p = get_absolute_path_with_home_dir(None);
        assert_eq!(home_dir, p)
    }

    #[test]
    fn test_get_absolute_path_with_home_dir_prefix() {
        let home_dir = dirs::home_dir().unwrap();
        let p = get_absolute_path_with_home_dir(Some("~/deps/pacquet_home".to_string()));
        assert_eq!(home_dir.join("deps/pacquet_home"), p);
    }

    #[test]
    fn test_get_absolute_path_with_home_dir_absolute() {
        let p = get_absolute_path_with_home_dir(Some("/tmp/pacquet_home".to_string()));
        assert_eq!(PathBuf::from("/tmp/pacquet_home"), p);
    }
}
