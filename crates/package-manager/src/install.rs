use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicU8, time::SystemTime};

use crate::{
    InstallFrozenLockfile, InstallFrozenLockfileError, InstallWithoutLockfile,
    InstallWithoutLockfileError, ResolvedPackages,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_config::{Config, NodeLinker};
use pacquet_lockfile::{
    LoadLockfileError, Lockfile, SaveLockfileError, StalenessReason, satisfies_package_manifest,
};
use pacquet_modules_yaml::{
    DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH, IncludedDependencies, LayoutVersion, Modules,
    NodeLinker as ModulesNodeLinker, RealApi, WriteModulesError, write_modules_manifest,
};
use pacquet_network::ThrottledClient;
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
    pub config: &'static Config,
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

    /// Surfaces a corrupted `<virtual_store_dir>/lock.yaml` rather
    /// than silently skipping the optimization. Mirrors upstream's
    /// `ignoreIncompatible: false` posture at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/index.ts#L226-L227>.
    #[diagnostic(transparent)]
    LoadCurrentLockfile(#[error(source)] LoadLockfileError),

    /// Surfaces a failure to persist the current lockfile so the next
    /// install can diff against it. A best-effort warn would let
    /// silent disk-full or permission issues compound across installs;
    /// fail the install instead.
    #[diagnostic(transparent)]
    SaveCurrentLockfile(#[error(source)] SaveLockfileError),

    /// `pnpm-lock.yaml` doesn't match the on-disk `package.json` for
    /// the project being installed. Mirrors upstream's
    /// `ERR_PNPM_OUTDATED_LOCKFILE` thrown from
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/pkg-manager/core/src/install/index.ts#L823>:
    /// the user (or CI) edited the manifest without regenerating the
    /// lockfile, and a frozen install would silently produce the
    /// wrong shape of `node_modules`. Fail the install instead.
    #[display(
        "Cannot install with \"frozen-lockfile\" because pnpm-lock.yaml is not up to date with package.json.\n\n  Failure reason:\n  {reason}"
    )]
    #[diagnostic(
        code(pacquet_package_manager::outdated_lockfile),
        help(
            "Regenerate the lockfile with `pnpm install --lockfile-only` so that pnpm-lock.yaml reflects the current package.json, then re-run `pacquet install --frozen-lockfile`."
        )
    )]
    OutdatedLockfile { reason: StalenessReason },

    /// `--frozen-lockfile` was requested against a lockfile whose
    /// `importers` map has no entry for the root project. Distinct
    /// from `NoLockfile` (file missing) — here the file exists but
    /// doesn't describe the project being installed.
    #[display(
        r#"Cannot install with "frozen-lockfile" because pnpm-lock.yaml has no `importers["{importer_id}"]` entry. Regenerate the lockfile with `pnpm install --lockfile-only`."#
    )]
    #[diagnostic(code(pacquet_package_manager::no_importer))]
    NoImporter { importer_id: String },
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

        // Load the *current* lockfile that records what the previous
        // install actually materialized in `<virtual_store_dir>/lock.yaml`.
        // The frozen-lockfile path diffs each wanted snapshot against
        // this on a per-`PackageKey` basis to decide whether the
        // already-installed slot is still usable. `Ok(None)` on a
        // first install (the file doesn't exist yet). A corrupted /
        // version-incompatible file surfaces as `LoadCurrentLockfile`
        // and fails the install — matching upstream's
        // `ignoreIncompatible: false` posture at the deps-restorer
        // call site rather than silently dropping the optimization.
        //
        // Mirrors upstream's `readCurrentLockfile` call at
        // <https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/index.ts#L226-L227>.
        let current_lockfile =
            Lockfile::load_current_from_virtual_store_dir(&config.virtual_store_dir)
                .map_err(InstallError::LoadCurrentLockfile)?;

        // `pnpm:context` carries the directories pnpm's reporter prints
        // in the install header. `currentLockfileExists` mirrors
        // upstream's <https://github.com/pnpm/pnpm/blob/94240bc046/installing/context/src/index.ts#L196>:
        // `true` once a previous install has written
        // `<virtual_store_dir>/lock.yaml`.
        R::emit(&LogEvent::Context(ContextLog {
            level: LogLevel::Debug,
            current_lockfile_exists: current_lockfile.is_some(),
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

            // Freshness check: verify the on-disk `package.json`
            // still matches the lockfile's importer entry before we
            // commit to materializing `node_modules` from it. Mirrors
            // upstream's `satisfiesPackageManifest` gate at
            // <https://github.com/pnpm/pnpm/blob/94240bc046/pkg-manager/core/src/install/index.ts#L808-L832>.
            // Pacquet has only one importer today (#431 tracks
            // workspaces), so the root project is the only thing to
            // verify; once workspaces land this becomes a per-project
            // loop over `importers`.
            let importer = importers.get(Lockfile::ROOT_IMPORTER_KEY).ok_or_else(|| {
                InstallError::NoImporter { importer_id: Lockfile::ROOT_IMPORTER_KEY.to_string() }
            })?;
            satisfies_package_manifest(importer, manifest, Lockfile::ROOT_IMPORTER_KEY)
                .map_err(|reason| InstallError::OutdatedLockfile { reason })?;

            InstallFrozenLockfile {
                http_client,
                config,
                importers,
                packages: packages.as_ref(),
                snapshots: snapshots.as_ref(),
                current_snapshots: current_lockfile.as_ref().and_then(|l| l.snapshots.as_ref()),
                current_packages: current_lockfile.as_ref().and_then(|l| l.packages.as_ref()),
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

        // Write `<virtual_store_dir>/lock.yaml`. Mirrors upstream's
        // `writeCurrentLockfile` call at
        // <https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/index.ts#L1597>:
        // captures what was actually materialized so the next install
        // can diff each snapshot against it and skip the unchanged
        // slots. Persist *after* `write_modules_manifest` succeeds so
        // a manifest failure can't leave a fresh current-lockfile
        // pointing at incomplete install state — the next frozen
        // reinstall would otherwise diff against a graph that never
        // finished committing (review on #442). Today pacquet writes
        // the wanted lockfile unchanged because there's only one
        // importer to filter to; once workspace install (#431) lands
        // this needs to narrow to the *filtered* lockfile (selected
        // importers × engine filter).
        if frozen_lockfile && let Some(lockfile) = lockfile {
            lockfile
                .save_current_to_virtual_store_dir(&config.virtual_store_dir)
                .map_err(InstallError::SaveCurrentLockfile)?;
        }

        // `pnpm:summary` closes the install and lets the reporter render
        // the accumulated `pnpm:root` events as a "+N -M" block. Must
        // come after `importing_done`, matching pnpm's ordering at
        // <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1663>.
        R::emit(&LogEvent::Summary(SummaryLog { level: LogLevel::Debug, prefix }));

        Ok(())
    }
}

/// Translate pacquet's [`Config::node_linker`] into the
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
fn build_modules_manifest(config: &Config, included: IncludedDependencies) -> Modules {
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
