use super::default_store_dir;
use crate::test_env_guard::EnvGuard;
use pacquet_store_dir::StoreDir;
use pretty_assertions::assert_eq;
use std::env;

#[cfg(windows)]
use super::{default_store_dir_windows, get_drive_letter};
#[cfg(windows)]
use std::path::Path;

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
