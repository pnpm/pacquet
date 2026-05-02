use std::{path::Path, sync::atomic::AtomicU8};

use crate::{
    InstallFrozenLockfile, InstallFrozenLockfileError, InstallWithoutLockfile,
    InstallWithoutLockfileError, ResolvedPackages,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::Lockfile;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_reporter::{ContextLog, LogEvent, LogLevel, Reporter, Stage, StageLog, SummaryLog};
use pacquet_tarball::MemCache;

/// This subroutine does everything `pacquet install` is supposed to do.
#[must_use]
pub struct Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub lockfile: Option<&'a Lockfile>,
    pub dependency_groups: DependencyGroupList,
    pub frozen_lockfile: bool,
}

/// Error type of [`Install`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallError {
    #[display(
        "Headless installation requires a pnpm-lock.yaml file, but none was found. Run `pacquet install` without --frozen-lockfile to create one."
    )]
    #[diagnostic(code(pacquet_package_manager::no_lockfile))]
    NoLockfile,

    #[display(
        "Installing with a writable lockfile is not yet supported. Disable lockfile in .npmrc (lockfile=false) or pass --frozen-lockfile with an existing pnpm-lock.yaml."
    )]
    #[diagnostic(code(pacquet_package_manager::unsupported_lockfile_mode))]
    UnsupportedLockfileMode,

    #[diagnostic(transparent)]
    WithoutLockfile(#[error(source)] InstallWithoutLockfileError),

    #[diagnostic(transparent)]
    FrozenLockfile(#[error(source)] InstallFrozenLockfileError),
}

impl<'a, DependencyGroupList> Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run<R: Reporter>(self) -> Result<(), InstallError> {
        let Install {
            tarball_mem_cache,
            resolved_packages,
            http_client,
            config,
            manifest,
            lockfile,
            dependency_groups,
            frozen_lockfile,
        } = self;

        // Project root for the [bunyan]-envelope `prefix`. Upstream pnpm
        // emits this as `lockfileDir`, the directory containing
        // `pnpm-lock.yaml`. With workspace support that equals the
        // workspace root. Pacquet has no workspace support yet, so the
        // manifest's parent directory is the correct value today.
        // pnpm/pacquet#357 tracks resolving this via a
        // `findWorkspaceDir`-equivalent once workspaces land.
        //
        // [bunyan]: https://github.com/trentm/node-bunyan
        let prefix = manifest
            .path()
            .parent()
            .map(Path::to_str)
            .map(Option::<&str>::unwrap)
            .unwrap()
            .to_owned();

        // `pnpm:context` carries the directories pnpm's reporter prints
        // in the install header. `currentLockfileExists` reflects
        // `node_modules/.pnpm/lock.yaml` upstream; pacquet doesn't yet
        // read or write that file, so it's always `false` today.
        // TODO: flip when the current-lockfile path lands.
        // Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/context/src/index.ts#L196>.
        R::emit(&LogEvent::Context(ContextLog {
            level: LogLevel::Debug,
            current_lockfile_exists: false,
            store_dir: config.store_dir.display().to_string(),
            virtual_store_dir: config.virtual_store_dir.to_string_lossy().into_owned(),
        }));

        R::emit(&LogEvent::Stage(StageLog {
            level: LogLevel::Debug,
            prefix: prefix.clone(),
            stage: Stage::ImportingStarted,
        }));

        // Install-scoped dedupe state for `pnpm:package-import-method`.
        // Threaded down to `link_file::log_method_once` so each install
        // emits the channel afresh — mirroring upstream pnpm's per-
        // importer closure capture rather than a process-static.
        let logged_methods = AtomicU8::new(0);

        tracing::info!(target: "pacquet::install", "Start all");

        // Dispatch priority, matching pnpm's CLI semantics:
        //
        // 1. `--frozen-lockfile` is the strongest signal. If the user
        //    passed the flag, use the frozen-lockfile path regardless of
        //    `config.lockfile`. The prior `match` treated
        //    `config.lockfile=false` as "skip the lockfile entirely" and
        //    silently dropped the CLI flag — so pacquet's new-config
        //    default (lockfile unset → `false`) turned every
        //    `--frozen-lockfile` install into a registry-resolving
        //    no-lockfile install, which is also what the integrated
        //    benchmark has been measuring.
        //
        // 2. Otherwise follow `config.lockfile`. `true` means we'd
        //    normally generate / update a lockfile, which pacquet
        //    doesn't support yet → `UnsupportedLockfileMode`. `false`
        //    means "lockfile disabled, resolve from registry".
        if frozen_lockfile {
            let Some(lockfile) = lockfile else {
                return Err(InstallError::NoLockfile);
            };
            let Lockfile { lockfile_version, importers, packages, snapshots, .. } = lockfile;
            assert_eq!(lockfile_version.major, 9); // compatibility check already happens at serde, but this still helps preventing programmer mistakes.

            InstallFrozenLockfile {
                http_client,
                config,
                importers,
                packages: packages.as_ref(),
                snapshots: snapshots.as_ref(),
                dependency_groups,
                logged_methods: &logged_methods,
                requester: &prefix,
            }
            .run::<R>()
            .await
            .map_err(InstallError::FrozenLockfile)?;
        } else if config.lockfile {
            return Err(InstallError::UnsupportedLockfileMode);
        } else {
            InstallWithoutLockfile {
                tarball_mem_cache,
                resolved_packages,
                http_client,
                config,
                manifest,
                dependency_groups,
                logged_methods: &logged_methods,
                requester: &prefix,
            }
            .run::<R>()
            .await
            .map_err(InstallError::WithoutLockfile)?;
        }

        tracing::info!(target: "pacquet::install", "Complete all");

        R::emit(&LogEvent::Stage(StageLog {
            level: LogLevel::Debug,
            prefix: prefix.clone(),
            stage: Stage::ImportingDone,
        }));

        // `pnpm:summary` closes the install and lets the reporter render
        // the accumulated `pnpm:root` events as a "+N -M" block. Must
        // come after `importing_done`, matching pnpm's ordering at
        // <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1663>.
        R::emit(&LogEvent::Summary(SummaryLog { level: LogLevel::Debug, prefix }));

        Ok(())
    }
}

#[cfg(test)]
mod tests;
