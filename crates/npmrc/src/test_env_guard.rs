//! Test-only helper: snapshot and restore selected environment
//! variables around a block of code.
//!
//! `env::set_var` / `env::remove_var` are process-global and outlive
//! any `#[test]` they were set inside. The tests in this crate that
//! exercise [`default_store_dir`][crate::custom_deserializer::default_store_dir]
//! need to set `PNPM_HOME` / `XDG_DATA_HOME` to check specific branches;
//! without the guard each test leaks its modifications into everything
//! that runs afterwards in the same process and cross-stomps on a
//! developer's shell-exported values.
//!
//! Proper fix is to thread env lookups through dependency injection
//! (the same TODO already noted inline on each test), at which point
//! the guard goes away. For now this keeps the existing env-var tests
//! correct under nextest's in-process parallelism.
use std::env;

/// Restore a named set of env vars on drop.
#[must_use = "the guard must be held until the test ends"]
pub struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    /// Snapshot the current values of `vars`. When the returned guard
    /// drops, each variable is put back to exactly what it was (set to
    /// the recorded value, or removed if it was absent).
    pub fn snapshot<I>(vars: I) -> Self
    where
        I: IntoIterator<Item = &'static str>,
    {
        let saved = vars.into_iter().map(|name| (name, env::var(name).ok())).collect();
        EnvGuard { saved }
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
