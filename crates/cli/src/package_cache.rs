// TODO: find a better name

use dashmap::DashMap;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::mpsc::UnboundedReceiver;

/// Value of the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageState {
    /// The package is being processed.
    InProgress,
    /// The package is saved.
    Available(Arc<HashMap<String, PathBuf>>),
}

/// Internal cache of [`crate::PackageManager`].
///
/// The key of this hashmap is saved path of each package.
pub type PackageCache = DashMap<PathBuf, UnboundedReceiver<PackageState>>; // TODO: use bounded channels to avoid out-of-memory errors
