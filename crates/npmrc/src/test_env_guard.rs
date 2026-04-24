//! Test-only helper: serialize env-var mutations across parallel
//! tests, and snapshot/restore selected variables around each block.
//!
//! `env::set_var` / `env::remove_var` are process-global and outlive
//! any `#[test]` they were set inside. Rust / nextest run unit tests in
//! parallel threads inside the same process by default, so two
//! concurrent tests mutating `PNPM_HOME` can easily observe each
//! other's half-set state. This module guards both concerns:
//!
//! * a process-wide `Mutex` is acquired for the lifetime of each
//!   guard, so only one env-mutating test holds the "env-is-mine" lock
//!   at a time;
//! * the snapshot/restore pass on `Drop` puts the named variables back
//!   to whatever the caller inherited, so the guard can't leak state
//!   into unrelated tests or stomp a developer's shell.
//!
//! Proper fix is to thread env lookups through dependency injection
//! (the same TODO already noted inline on each test), at which point
//! this module goes away. Until then, holding the returned guard is
//! enough to keep the env-var tests correct under `cargo test` and
//! `cargo nextest run` alike.
use std::{
    env,
    ffi::OsString,
    sync::{Mutex, MutexGuard, OnceLock},
};

/// Serialization mutex for env-mutating tests. A single `Mutex<()>` —
/// uncontended outside the handful of tests that need it, cheap when
/// held.
fn env_mutex() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Restore a named set of env vars on drop and hold the env-mutation
/// lock for that lifetime.
#[must_use = "the guard must be held until the test ends"]
pub struct EnvGuard {
    // `OsString` so non-UTF-8 values round-trip. `env::var` returns
    // `Err(NotUnicode)` for those and would silently coerce to "absent"
    // here — then `Drop` would `remove_var` a variable the user
    // actually had set, clobbering CI / shell state. `env::var_os` +
    // `OsString` preserves the raw bytes.
    saved: Vec<(&'static str, Option<OsString>)>,
    // Released on drop, last. Ignore the poison case: if another
    // env-mutating test panicked while holding the lock, the env vars
    // it touched were restored by *its* guard's `Drop` before the
    // unwind propagated, so the environment is in a known state and
    // the next test can safely proceed.
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// Acquire the process-wide env-mutation lock and snapshot the
    /// current values of `vars`. When the returned guard drops, each
    /// variable is put back to exactly what it was (set to the recorded
    /// value, or removed if it was absent), and the lock is released.
    pub fn snapshot<I>(vars: I) -> Self
    where
        I: IntoIterator<Item = &'static str>,
    {
        let lock = env_mutex().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = vars.into_iter().map(|name| (name, env::var_os(name))).collect();
        EnvGuard { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, prior) in &self.saved {
            match prior {
                Some(value) => env::set_var(name, value),
                None => env::remove_var(name),
            }
        }
    }
}
