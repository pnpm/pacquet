use pacquet_store_dir::StoreDir;
use std::{env, path::PathBuf};

#[cfg(windows)]
use std::{path::Component, path::Path};

pub fn default_hoist_pattern() -> Vec<String> {
    vec!["*".to_string()]
}

pub fn default_public_hoist_pattern() -> Vec<String> {
    vec!["*eslint*".to_string(), "*prettier*".to_string()]
}

// Get the drive letter from a path on Windows. If it's not a Windows path, return None.
#[cfg(windows)]
fn get_drive_letter(current_dir: &Path) -> Option<char> {
    if let Some(Component::Prefix(prefix_component)) = current_dir.components().next()
        && let std::path::Prefix::Disk(disk_byte) | std::path::Prefix::VerbatimDisk(disk_byte) =
            prefix_component.kind()
    {
        return Some(disk_byte as char);
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

/// Default `childConcurrency` matching upstream's
/// [`getDefaultWorkspaceConcurrency`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.ts#L21-L23):
/// `min(4, availableParallelism())`. Read at runtime so `cargo test`
/// and overrides via yaml still resolve to a usable value on
/// 1-core sandboxes.
pub fn default_child_concurrency() -> u32 {
    default_child_concurrency_with_parallelism(available_parallelism())
}

/// Internal helper exposed for tests so they can pin the
/// `parallelism` input. Upstream's test suite mocks
/// `os.availableParallelism` via Jest; pacquet injects the value
/// directly. Mirrors upstream's [`getDefaultWorkspaceConcurrency`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.ts#L21-L23).
pub fn default_child_concurrency_with_parallelism(parallelism: u32) -> u32 {
    parallelism.min(4)
}

/// Available CPU parallelism, mirroring upstream's
/// [`getAvailableParallelism`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.ts#L5-L13).
/// Floors at 1.
pub fn available_parallelism() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1).max(1)
}

/// Resolve `childConcurrency` from a possibly-negative yaml value
/// to a concrete `u32`. Mirrors upstream's
/// [`getWorkspaceConcurrency`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.ts#L25-L34):
///
/// - `None` → default (`min(4, parallelism)`).
/// - Positive `n` → `n`.
/// - Zero or negative `n` → `max(1, parallelism - |n|)`.
///
/// The negative-offset semantics let users say "use all cores minus
/// N" without hardcoding the core count.
pub fn resolve_child_concurrency(option: Option<i32>) -> u32 {
    resolve_child_concurrency_with_parallelism(option, available_parallelism())
}

/// Internal helper exposed for tests so they can pin the
/// `parallelism` input. Mirrors upstream's
/// [`getWorkspaceConcurrency`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.ts#L25-L34)
/// — the resolver logic itself, with the parallelism input
/// injected rather than read from the OS.
pub fn resolve_child_concurrency_with_parallelism(option: Option<i32>, parallelism: u32) -> u32 {
    match option {
        None => default_child_concurrency_with_parallelism(parallelism),
        Some(n) if n > 0 => n as u32,
        // `unsigned_abs` instead of `(-n) as u32` — the latter
        // panics in debug builds on `n == i32::MIN` (negation
        // overflow); the former returns `i32::MAX as u32 + 1`
        // safely.
        Some(n) => parallelism.saturating_sub(n.unsigned_abs()).max(1),
    }
}

#[cfg(test)]
mod tests;
