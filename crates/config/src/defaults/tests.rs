use super::{
    default_child_concurrency_with_parallelism, default_store_dir, default_unsafe_perm,
    is_unsafe_perm_posix, resolve_child_concurrency, resolve_child_concurrency_with_parallelism,
};
use pacquet_store_dir::StoreDir;
use pacquet_testing_utils::env_guard::EnvGuard;
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

/// Port of upstream
/// [`'getDefaultWorkspaceConcurrency: cpu num < 4'`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.test.ts#L25-L28).
/// On a 1-core host, the default caps at 1 (not 4).
#[test]
fn default_child_concurrency_with_parallelism_below_four() {
    assert_eq!(default_child_concurrency_with_parallelism(1), 1);
}

/// Port of upstream
/// [`'getDefaultWorkspaceConcurrency: cpu num > 4'`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.test.ts#L30-L33).
/// Caps at 4 on a 5-core host.
#[test]
fn default_child_concurrency_with_parallelism_above_four() {
    assert_eq!(default_child_concurrency_with_parallelism(5), 4);
}

/// Port of upstream
/// [`'getDefaultWorkspaceConcurrency: cpu num = 4'`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.test.ts#L35-L38).
/// At the boundary, 4 is the exact result (not floored or capped).
#[test]
fn default_child_concurrency_with_parallelism_at_four() {
    assert_eq!(default_child_concurrency_with_parallelism(4), 4);
}

/// Port of upstream
/// [`'default workspace concurrency'`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.test.ts#L48-L52).
/// `getWorkspaceConcurrency(undefined)` on a >=4-core host yields 4
/// (the upstream test runs on the default Jest host; on a host with
/// >=4 cores the default is 4). Pin a >=4 parallelism so the
/// expectation is deterministic.
#[test]
fn resolve_child_concurrency_default_with_four_or_more_cores() {
    assert_eq!(resolve_child_concurrency_with_parallelism(None, 4), 4);
    assert_eq!(resolve_child_concurrency_with_parallelism(None, 8), 4);
}

/// Port of upstream
/// [`'match host cores amount'`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.test.ts#L58-L62).
/// `getWorkspaceConcurrency(0)` returns the host's parallelism
/// verbatim — the saturated `parallelism - 0` path.
#[test]
fn resolve_child_concurrency_zero_returns_full_parallelism() {
    assert_eq!(resolve_child_concurrency_with_parallelism(Some(0), 8), 8);
    assert_eq!(resolve_child_concurrency_with_parallelism(Some(0), 1), 1);
}

/// Port of upstream
/// [`'host cores minus X'`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.test.ts#L64-L71).
/// `n = -1` → `max(1, cores - 1)`; `n = -9999` → `1` (saturating).
/// Replaces the earlier bound-check-only test with the precise
/// formula that the upstream suite pins.
#[test]
fn resolve_child_concurrency_negative_offset_matches_upstream_formula() {
    // n = -1 with 8 cores → 7.
    assert_eq!(resolve_child_concurrency_with_parallelism(Some(-1), 8), 7);
    // n = -1 with 1 core → max(1, 0) → 1.
    assert_eq!(resolve_child_concurrency_with_parallelism(Some(-1), 1), 1);
    // n = -9999 saturates → 1 regardless of parallelism.
    assert_eq!(resolve_child_concurrency_with_parallelism(Some(-9999), 8), 1);
    assert_eq!(resolve_child_concurrency_with_parallelism(Some(-9999), 1), 1);
}

/// Existing pacquet test (not from upstream): both the public
/// `resolve_child_concurrency` and the testable
/// `_with_parallelism` helper agree on positive inputs. The
/// upstream
/// [`'get back positive amount'`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/concurrency.test.ts#L54-L56)
/// case (`n = 5` → `5`) is checked here alongside the helper
/// equivalence.
#[test]
fn resolve_child_concurrency_positive_amount() {
    assert_eq!(resolve_child_concurrency(Some(5)), 5);
    assert_eq!(resolve_child_concurrency_with_parallelism(Some(5), 1), 5);
    assert_eq!(resolve_child_concurrency_with_parallelism(Some(5), 100), 5);
}

/// `resolve_child_concurrency(Some(i32::MIN))` must not panic.
/// A naive `(-n) as u32` overflows in debug builds when
/// `n == i32::MIN` because the negation itself overflows;
/// `unsigned_abs` is the safe path. `i32::MIN.unsigned_abs()`
/// is `2_147_483_648`, well above any plausible host
/// parallelism, so `saturating_sub` produces `0` and `.max(1)`
/// lifts to exactly `1` — assert that precise value so a wrong
/// result like `2` would still fail the test.
#[test]
fn resolve_child_concurrency_handles_i32_min() {
    let result = resolve_child_concurrency(Some(i32::MIN));
    assert_eq!(result, 1);
}

/// POSIX truth table for [`is_unsafe_perm_posix`] matching
/// upstream's
/// [`getuid?.() !== 0`](https://github.com/pnpm/pnpm/blob/94240bc046/building/after-install/src/extendBuildOptions.ts#L83-L86)
/// branch:
///
/// - root (uid 0) → `false` (drop privileges)
/// - non-root (any other uid) → `true` (no drop)
#[test]
fn is_unsafe_perm_posix_truth_table() {
    assert!(!is_unsafe_perm_posix(0), "running as root → drop perms");
    assert!(is_unsafe_perm_posix(1), "non-root uid 1 → no drop");
    assert!(is_unsafe_perm_posix(501), "non-root uid 501 → no drop");
    assert!(is_unsafe_perm_posix(65534), "non-root uid 65534 → no drop");
}

/// On Windows, [`default_unsafe_perm`] short-circuits to `true`
/// without ever calling `getuid()`. Mirrors upstream's
/// `process.platform === 'win32' || process.platform === 'cygwin'`
/// branch.
#[cfg(windows)]
#[test]
fn default_unsafe_perm_on_windows_is_always_true() {
    assert!(default_unsafe_perm(), "Windows default must always be true");
}

/// On POSIX (excluding Cygwin), [`default_unsafe_perm`] matches
/// the host's runtime uid via [`is_unsafe_perm_posix`]. Test
/// environments don't usually run as root, so this is `true` in
/// practice; the `is_unsafe_perm_posix_truth_table` test above
/// pins the per-uid logic without needing root privileges. Cygwin
/// is excluded because `default_unsafe_perm` short-circuits to
/// `true` on Cygwin regardless of uid (matching upstream's
/// `process.platform === 'cygwin'` branch).
#[cfg(all(unix, not(target_os = "cygwin")))]
#[test]
fn default_unsafe_perm_on_posix_matches_runtime_uid() {
    // SAFETY: `libc::getuid` is documented as always-safe.
    let uid = unsafe { libc::getuid() } as u32;
    assert_eq!(default_unsafe_perm(), is_unsafe_perm_posix(uid));
}

/// On Cygwin, [`default_unsafe_perm`] short-circuits to `true`
/// without consulting the uid — same branch as Windows. Mirrors
/// upstream's `process.platform === 'cygwin'` check.
#[cfg(target_os = "cygwin")]
#[test]
fn default_unsafe_perm_on_cygwin_is_always_true() {
    assert!(default_unsafe_perm(), "Cygwin default must always be true (matches upstream)");
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
