use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;

/// Error type of [`StoreDir::prune`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum PruneError {}

impl StoreDir {
    /// Remove all files in the store that don't have reference elsewhere.
    pub fn prune(&self) -> Result<(), PruneError> {
        // Ref: https://pnpm.io/cli/store#prune
        todo!("remove orphaned files")
    }
}
