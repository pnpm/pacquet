// TODO: find a better name

use dashmap::DashMap;
use std::path::PathBuf;
use tokio::sync::watch::Receiver;

/// Value of the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageState {
    /// The package is being processed.
    InProcess,
    /// The package is saved.
    Available,
}

/// Internal cache of [`crate::PackageManager`].
///
/// The key of this hashmap is saved path of each package.
pub type PackageCache = DashMap<PathBuf, Receiver<PackageState>>;
