use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicU8, time::SystemTime};

use crate::{
    InstallFrozenLockfile, InstallFrozenLockfileError, InstallWithoutLockfile,
    InstallWithoutLockfileError, ResolvedPackages,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::Lockfile;
use pacquet_modules_yaml::{
    DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH, IncludedDependencies, LayoutVersion, Modules,
    NodeLinker as ModulesNodeLinker, RealApi, WriteModulesError, write_modules_manifest,
};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::{NodeLinker, Npmrc};
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_reporter::{
    ContextLog, LogEvent, LogLevel, PackageManifestLog, PackageManifestMessage, Reporter, Stage,
    StageLog, SummaryLog,
};
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

    #[diagnostic(transparent)]
    WriteModules(#[error(source)] WriteModulesError),
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

        // Collect once so the same set drives both the install dispatch
        // and the `included` field of `.modules.yaml` written below.
        // Mirrors upstream `ctx.include` at
        // <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1612>,
        // which is the same set the dependency-graph walker observes.
        let dependency_groups: Vec<DependencyGroup> = dependency_groups.into_iter().collect();
        let included = IncludedDependencies {
            dependencies: dependency_groups.contains(&DependencyGroup::Prod),
            dev_dependencies: dependency_groups.contains(&DependencyGroup::Dev),
            optional_dependencies: dependency_groups.contains(&DependencyGroup::Optional),
        };

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

        // `pnpm:package-manifest initial` carries the on-disk
        // `package.json` body. Mirrors pnpm's per-project emit at
        // <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/context/src/index.ts#L133>:
        // fires before `pnpm:context` so consumers that key off
        // manifest contents have it ready when the install header
        // renders.
        R::emit(&LogEvent::PackageManifest(PackageManifestLog {
            level: LogLevel::Debug,
            message: PackageManifestMessage::Initial {
                prefix: prefix.clone(),
                initial: manifest.value().clone(),
            },
        }));

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

        // `Stage::ImportingDone` is emitted inside the install paths
        // (`InstallFrozenLockfile` between symlink and build, and
        // `InstallWithoutLockfile` after the writer task) so that any
        // subsequent `pnpm:lifecycle` events render after the import
        // progress display has closed. Mirrors upstream's emit point in
        // <https://github.com/pnpm/pnpm/blob/80037699fb/installing/deps-installer/src/install/link.ts#L167>.

        // Write `node_modules/.modules.yaml`. Mirrors upstream's
        // `writeModulesManifest` call at
        // <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1608-L1630>,
        // which fires after `importing_done` and before the closing
        // `pnpm:summary` emit. The manifest records the resolved
        // directory layout, hoist patterns, included dependency groups,
        // store dir, and registries so a later install (or another
        // tool) can detect a layout change and prune accordingly.
        write_modules_manifest::<RealApi>(
            &config.modules_dir,
            build_modules_manifest(config, included),
        )
        .map_err(InstallError::WriteModules)?;

        // `pnpm:summary` closes the install and lets the reporter render
        // the accumulated `pnpm:root` events as a "+N -M" block. Must
        // come after `importing_done`, matching pnpm's ordering at
        // <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1663>.
        R::emit(&LogEvent::Summary(SummaryLog { level: LogLevel::Debug, prefix }));

        Ok(())
    }
}

/// Translate pacquet's [`Npmrc::node_linker`] into the
/// [`pacquet_modules_yaml::NodeLinker`] enum used on disk. The two
/// enums share the same variant set (`isolated`, `hoisted`, `pnp`),
/// matching upstream's `nodeLinker` string.
fn map_node_linker(linker: &NodeLinker) -> ModulesNodeLinker {
    match linker {
        NodeLinker::Isolated => ModulesNodeLinker::Isolated,
        NodeLinker::Hoisted => ModulesNodeLinker::Hoisted,
        NodeLinker::Pnp => ModulesNodeLinker::Pnp,
    }
}

/// Assemble the [`Modules`] payload for [`write_modules_manifest`].
///
/// Mirrors upstream's literal at
/// <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1608-L1630>.
/// Fields pacquet does not populate yet (`hoistedDependencies`,
/// `pendingBuilds`, `skipped`, `injectedDeps`, `ignoredBuilds`,
/// `allowBuilds`) default to empty / unset, which is exactly what
/// upstream produces for a single-importer install with no skipped
/// optional deps and no build allowlist.
fn build_modules_manifest(config: &Npmrc, included: IncludedDependencies) -> Modules {
    Modules {
        hoist_pattern: Some(config.hoist_pattern.clone()),
        included,
        layout_version: Some(LayoutVersion),
        node_linker: Some(map_node_linker(&config.node_linker)),
        // `${name}@${version}` per upstream. `CARGO_PKG_VERSION`
        // resolves at compile time to this crate's package version.
        package_manager: concat!("pacquet@", env!("CARGO_PKG_VERSION")).to_string(),
        public_hoist_pattern: Some(config.public_hoist_pattern.clone()),
        // RFC 1123 / `toUTCString()` format, matching upstream's
        // `new Date().toUTCString()` at line 1622.
        pruned_at: httpdate::fmt_http_date(SystemTime::now()),
        registries: Some(BTreeMap::from([("default".to_string(), config.registry.clone())])),
        store_dir: config.store_dir.display().to_string(),
        virtual_store_dir: config.virtual_store_dir.to_string_lossy().into_owned(),
        virtual_store_dir_max_length: DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
