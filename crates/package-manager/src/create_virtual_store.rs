use crate::{InstallPackageBySnapshot, InstallPackageBySnapshotError};
use derive_more::{Display, Error};
use futures_util::future;
use miette::Diagnostic;
use pacquet_lockfile::{PackageKey, PackageMetadata, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_store_dir::StoreIndex;
use pipe_trait::Pipe;
use std::collections::HashMap;

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
}

/// Error type of [`CreateVirtualStore`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualStoreError {
    #[diagnostic(transparent)]
    InstallPackageBySnapshot(#[error(source)] InstallPackageBySnapshotError),

    #[display("Lockfile has a snapshot entry `{snapshot_key}` with no matching metadata entry (`{metadata_key}`) in `packages:`.")]
    #[diagnostic(code(pacquet_package_manager::missing_package_metadata))]
    MissingPackageMetadata { snapshot_key: String, metadata_key: String },

    #[display(
        "Lockfile has a `snapshots:` section but no `packages:` section; every entry in `snapshots:` must have a matching metadata entry. The lockfile is malformed."
    )]
    #[diagnostic(code(pacquet_package_manager::missing_packages_section))]
    MissingPackagesSection,
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<(), CreateVirtualStoreError> {
        let CreateVirtualStore { http_client, config, packages, snapshots } = self;

        let Some(snapshots) = snapshots else {
            // No snapshots to install. If the lockfile also has no project deps
            // this is a valid no-op; if it does, pnpm would have populated
            // `snapshots`, so bailing out here is safe enough for v9.
            return Ok(());
        };
        let packages = packages.ok_or(CreateVirtualStoreError::MissingPackagesSection)?;

        // Open the read-only SQLite index once for the whole run instead of
        // per snapshot. Every `InstallPackageBySnapshot` performs a cache
        // lookup against this index before falling through to the network;
        // on a 1352-package lockfile the per-snapshot reopen accounted for
        // ~1.3 s of wall time even with a fully populated store (see #260).
        // A `None` here means the store has no `index.db` yet (first install
        // against an empty store), in which case every lookup would miss —
        // so we keep the handle `Option`al and short-circuit.
        //
        // The open itself is synchronous SQLite I/O (`Connection::open_with_flags`
        // + a `PRAGMA busy_timeout`), so park it on the blocking pool instead
        // of stalling the reactor thread, even for the sub-millisecond it
        // usually takes.
        //
        // A `JoinError` here (blocking-task panic, or cancellation during
        // runtime shutdown) is degraded into `None` so the install still
        // makes progress — cache lookups just miss. `shared_readonly_in`
        // already yields `None` for a first-time install against an empty
        // store, and downstream callers handle that shape correctly. We
        // surface the error at `warn!` so a silent task panic or
        // cancellation is still diagnosable in the log.
        let store_dir: &'static _ = &config.store_dir;
        let store_index =
            match tokio::task::spawn_blocking(move || StoreIndex::shared_readonly_in(store_dir))
                .await
            {
                Ok(store_index) => store_index,
                Err(error) => {
                    tracing::warn!(
                        target: "pacquet::install",
                        ?error,
                        "store-index open task failed; continuing without a shared cache index",
                    );
                    None
                }
            };
        let store_index_ref = store_index.as_ref();

        snapshots
            .iter()
            .map(|(snapshot_key, snapshot)| async move {
                let metadata_key = snapshot_key.without_peer();
                let metadata = packages.get(&metadata_key).ok_or_else(|| {
                    CreateVirtualStoreError::MissingPackageMetadata {
                        snapshot_key: snapshot_key.to_string(),
                        metadata_key: metadata_key.to_string(),
                    }
                })?;
                InstallPackageBySnapshot {
                    http_client,
                    config,
                    store_index: store_index_ref,
                    package_key: snapshot_key,
                    metadata,
                    snapshot,
                }
                .run()
                .await
                .map_err(CreateVirtualStoreError::InstallPackageBySnapshot)
            })
            .pipe(future::try_join_all)
            .await?;

        Ok(())
    }
}
