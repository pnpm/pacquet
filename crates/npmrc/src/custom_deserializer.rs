use pacquet_store_dir::StoreDir;
use serde::{Deserialize, Deserializer, de};
use std::{env, path::PathBuf, str::FromStr};

#[cfg(windows)]
use std::{path::Component, path::Path};

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

// Get the drive letter from a path on Windows. If it's not a Windows path, return None.
#[cfg(windows)]
fn get_drive_letter(current_dir: &Path) -> Option<char> {
    if let Some(Component::Prefix(prefix_component)) = current_dir.components().next() {
        if let std::path::Prefix::Disk(disk_byte) | std::path::Prefix::VerbatimDisk(disk_byte) =
            prefix_component.kind()
        {
            return Some(disk_byte as char);
        }
    }
    None
}

#[cfg(windows)]
fn default_store_dir_windows(home_dir: &Path, current_dir: &Path) -> PathBuf {
    let current_drive =
        get_drive_letter(current_dir).expect("current dir is an absolute path with drive letter");
    let home_drive =
        get_drive_letter(home_dir).expect("home dir is an absolute path with drive letter");

    if current_drive == home_drive {
        return home_dir.join("AppData/Local/pnpm/store");
    }

    PathBuf::from(format!("{current_drive}:\\.pnpm-store"))
}

/// If the $PNPM_HOME env variable is set, then $PNPM_HOME/store
/// If the $XDG_DATA_HOME env variable is set, then $XDG_DATA_HOME/pnpm/store
/// On Windows: ~/AppData/Local/pnpm/store
/// On macOS: ~/Library/pnpm/store
/// On Linux: ~/.local/share/pnpm/store
pub fn default_store_dir() -> StoreDir {
    // TODO: If env variables start with ~, make sure to resolve it into home_dir.
    if let Ok(pnpm_home) = env::var("PNPM_HOME") {
        return PathBuf::from(pnpm_home).join("store").into();
    }

    if let Ok(xdg_data_home) = env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg_data_home).join("pnpm").join("store").into();
    }

    // Using ~ (tilde) for defining home path is not supported in Rust and
    // needs to be resolved into an absolute path.
    let home_dir = home::home_dir().expect("Home directory is not available");

    #[cfg(windows)]
    if cfg!(windows) {
        let current_dir = env::current_dir().expect("current directory is unavailable");
        return default_store_dir_windows(&home_dir, &current_dir).into();
    }

    // https://doc.rust-lang.org/std/env/consts/constant.OS.html
    match env::consts::OS {
        "linux" => home_dir.join(".local/share/pnpm/store").into(),
        "macos" => home_dir.join("Library/pnpm/store").into(),
        _ => panic!("unsupported operating system: {}", env::consts::OS),
    }
}

pub fn default_modules_dir() -> PathBuf {
    // TODO: find directory with package.json
    env::current_dir().expect("current directory is unavailable").join("node_modules")
}

pub fn default_virtual_store_dir() -> PathBuf {
    // TODO: find directory with package.json
    env::current_dir().expect("current directory is unavailable").join("node_modules/.pnpm")
}

pub fn default_registry() -> String {
    "https://registry.npmjs.org/".to_string()
}

pub fn default_modules_cache_max_age() -> u64 {
    10080
}

pub fn default_fetch_retries() -> u32 {
    2
}

pub fn default_fetch_retry_factor() -> u32 {
    10
}

pub fn default_fetch_retry_mintimeout() -> u64 {
    10_000
}

pub fn default_fetch_retry_maxtimeout() -> u64 {
    60_000
}

pub fn deserialize_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    u32::from_str(&s).map_err(de::Error::custom)
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

pub fn deserialize_store_dir<'de, D>(deserializer: D) -> Result<StoreDir, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_pathbuf(deserializer).map(StoreDir::from)
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
    use super::*;
    use crate::test_env_guard::EnvGuard;
    use pretty_assertions::assert_eq;
    use std::env;

    fn display_store_dir(store_dir: &StoreDir) -> String {
        store_dir.display().to_string().replace('\\', "/")
    }

    #[test]
    fn test_default_store_dir_with_pnpm_home_env() {
        let _g = EnvGuard::snapshot(["PNPM_HOME"]);
        // SAFETY: EnvGuard above serializes the test against other env-mutating
        // tests in this process; no other thread reads these vars concurrently.
        unsafe {
            env::set_var("PNPM_HOME", "/tmp/pnpm-home"); // TODO: change this to dependency injection
        }
        let store_dir = default_store_dir();
        assert_eq!(display_store_dir(&store_dir), "/tmp/pnpm-home/store");
    }

    #[test]
    fn test_default_store_dir_with_xdg_env() {
        // `default_store_dir` checks `PNPM_HOME` before `XDG_DATA_HOME`,
        // so a developer running the test suite with pnpm in their
        // environment (very common) otherwise sees the `PNPM_HOME`
        // branch win and the assertion fail. Snapshot-and-restore both
        // env vars so the test is self-contained even under nextest's
        // in-process parallelism. Proper fix is dependency injection —
        // see the TODO — but this is enough for the failure mode this
        // PR is fixing.
        let _g = EnvGuard::snapshot(["PNPM_HOME", "XDG_DATA_HOME"]);
        // SAFETY: EnvGuard above serializes the test against other env-mutating
        // tests in this process; no other thread reads these vars concurrently.
        unsafe {
            env::remove_var("PNPM_HOME");
            env::set_var("XDG_DATA_HOME", "/tmp/xdg_data_home");
        }
        let store_dir = default_store_dir();
        assert_eq!(display_store_dir(&store_dir), "/tmp/xdg_data_home/pnpm/store");
    }

    #[cfg(windows)]
    #[test]
    fn test_should_get_the_correct_drive_letter() {
        let current_dir = Path::new("C:\\Users\\user\\project");
        let drive_letter = get_drive_letter(current_dir);
        assert_eq!(drive_letter, Some('C'));
    }

    #[cfg(windows)]
    #[test]
    fn test_default_store_dir_with_windows_diff_drive() {
        let current_dir = Path::new("D:\\Users\\user\\project");
        let home_dir = Path::new("C:\\Users\\user");

        let store_dir = default_store_dir_windows(&home_dir, &current_dir);
        assert_eq!(store_dir, Path::new("D:\\.pnpm-store"));
    }

    #[cfg(windows)]
    #[test]
    fn test_dynamic_default_store_dir_with_windows_same_drive() {
        let current_dir = Path::new("C:\\Users\\user\\project");
        let home_dir = Path::new("C:\\Users\\user");

        let store_dir = default_store_dir_windows(&home_dir, &current_dir);
        assert_eq!(store_dir, Path::new("C:\\Users\\user\\AppData\\Local\\pnpm\\store"));
    }
}
