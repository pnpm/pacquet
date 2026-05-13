//! Fetcher for `LockfileResolution::Git` snapshots.
//!
//! Ports pnpm's
//! [`fetching/git-fetcher`](https://github.com/pnpm/pnpm/blob/94240bc046/fetching/git-fetcher/src/index.ts)
//! and the inner
//! [`exec/prepare-package`](https://github.com/pnpm/pnpm/blob/94240bc046/exec/prepare-package/src/index.ts)
//! that the fetcher delegates to when a git-hosted package needs
//! building. The two are co-located here because their only consumer
//! today is this crate; when Section C lands (the git-hosted *tarball*
//! fetcher), `prepare_package` is the one piece that lifts out into a
//! shared crate.

mod error;
mod fetcher;
mod packlist;
mod preferred_pm;
mod prepare_package;

pub use error::{GitFetcherError, PacklistError, PreparePackageError};
pub use fetcher::{GitFetchOutput, GitFetcher};
pub use packlist::packlist;
pub use preferred_pm::{PreferredPm, detect_preferred_pm};
pub use prepare_package::{PreparePackageOptions, PreparedPackage, prepare_package};
