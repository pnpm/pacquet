use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum StoreCommand {
    /// Checks for modified packages in the store.
    Store,
    /// Functionally equivalent to pnpm add, except this adds new packages to the store directly
    /// without modifying any projects or files outside of the store.
    Add,
    /// Removes unreferenced packages from the store.
    /// Unreferenced packages are packages that are not used by any projects on the system.
    /// Packages can become unreferenced after most installation operations, for instance when
    /// dependencies are made redundant.
    Prune,
    /// Returns the path to the active store directory.
    Path,
}
