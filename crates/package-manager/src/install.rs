use std::{collections::BTreeMap, sync::atomic::AtomicU8, time::SystemTime};

use crate::{
    HoistedDependencies, InstallFrozenLockfile, InstallFrozenLockfileError, InstallWithoutLockfile,
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
    /// `supportedArchitectures` after merging
    /// `Config::supported_architectures` from `pnpm-workspace.yaml`
    /// with the CLI per-axis overrides (`--cpu` / `--os` / `--libc`).
    /// Threaded into `InstallabilityHost` in the frozen-lockfile
    /// path so optional platform-tagged dependencies for the listed
    /// triples are kept even when they don't match the host. `None`
    /// means "host triple is the sole accept set" — same as
    /// upstream's behavior when neither yaml nor CLI sets a value.
    ///
    /// Computed at the CLI layer (see
    /// `pacquet_cli::cli_args::supported_architectures::SupportedArchitecturesArgs`)
    /// instead of being read from `config` directly, because
    /// `State.config` is a shared `&'static Config` — the CLI
    /// override merge happens in the caller and lands here as a
    /// fully-resolved value.
    pub supported_architectures: Option<pacquet_package_is_installable::SupportedArchitectures>,
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

    #[diagnostic(transparent)]
    FindWorkspaceDir(#[error(source)] pacquet_workspace::FindWorkspaceDirError),
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
            supported_architectures,
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
        // workspace root — pacquet finds it via [`find_workspace_dir`]
        // (port of upstream's `findWorkspaceDir`). Falls back to the
        // manifest's parent dir when no `pnpm-workspace.yaml` exists in
        // any ancestor, matching upstream's single-project behavior.
        // Closes pnpm/pacquet#357.
        //
        // [bunyan]: https://github.com/trentm/node-bunyan
        let manifest_dir = manifest.path().parent().expect("manifest path always has a parent dir");
        let workspace_root = pacquet_workspace::find_workspace_dir(manifest_dir)
            .map_err(InstallError::FindWorkspaceDir)?
            .unwrap_or_else(|| manifest_dir.to_path_buf());
        // Use `to_string_lossy` rather than `to_str().expect(...)` so a
        // valid filesystem path with non-UTF-8 bytes (possible on Unix)
        // doesn't panic the installer. `prefix` is used only for
        // reporter envelopes, so a lossy conversion is acceptable —
        // the rest of the install path uses the same pattern for
        // paths threaded into log events.
        let prefix = workspace_root.to_string_lossy().into_owned();

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
        let hoisted_dependencies: HoistedDependencies = if frozen_lockfile {
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

            let frozen_result = InstallFrozenLockfile {
                http_client,
                config,
                importers,
                packages: packages.as_ref(),
                snapshots: snapshots.as_ref(),
                current_snapshots: current_lockfile.as_ref().and_then(|l| l.snapshots.as_ref()),
                current_packages: current_lockfile.as_ref().and_then(|l| l.packages.as_ref()),
                dependency_groups,
                logged_methods: &logged_methods,
                workspace_root: &workspace_root,
                requester: &prefix,
                supported_architectures: supported_architectures.as_ref(),
            }
            .run::<R>()
            .await
            .map_err(InstallError::FrozenLockfile)?;

            // Register every importer against the shared store now
            // that the install has materialized their `node_modules/`.
            // Mirrors upstream's call into `@pnpm/store.controller`'s
            // [`registerProject`](https://github.com/pnpm/pnpm/blob/94240bc046/store/controller/src/storeController/projectRegistry.ts),
            // which runs once per importer — a workspace ends up with
            // one symlink in `<store_dir>/projects/` per package, so
            // `pacquet store prune` (tracked separately) can find
            // every reachable consumer of `<store_dir>/links/...`.
            //
            // Gated on `frozen_lockfile && enable_global_virtual_store`:
            // `InstallWithoutLockfile` keeps the project-local virtual
            // store via `VirtualStoreLayout::legacy`, and a registry
            // entry for it would point at a project that never
            // touches the shared store.
            //
            // Best-effort: a registry write failure shouldn't fail
            // the install. Surface as `tracing::warn!` so the failure
            // is diagnosable but the install carries on. Validation
            // of importer keys is done by
            // [`crate::SymlinkDirectDependencies::run`] before we get
            // here, so by this point every key is known-safe.
            if config.enable_global_virtual_store {
                for importer_id in importers.keys() {
                    let project_dir = crate::symlink_direct_dependencies::importer_root_dir(
                        &workspace_root,
                        importer_id,
                    );
                    if let Err(error) =
                        pacquet_store_dir::register_project(&config.store_dir, &project_dir)
                    {
                        tracing::warn!(
                            target: "pacquet::install",
                            ?error,
                            importer_id = %importer_id,
                            "Failed to register importer in the global-virtual-store registry; install continues",
                        );
                    }
                }
            }

            frozen_result
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
            .map_err(InstallError::WithoutLockfile)?
        };

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
            build_modules_manifest(config, included, hoisted_dependencies),
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
        // finished committing (review on #442).
        //
        // Workspace installs (#431) ship every importer's section of
        // the wanted lockfile unchanged because the install fans out
        // across all of them. Once `--filter` lands (Stage 2 of
        // #299), this needs to narrow to the filtered lockfile
        // (selected importers × engine filter) so the saved current
        // lockfile reflects only what was actually materialized.
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
/// Fields pacquet does not populate yet (`pendingBuilds`, `skipped`,
/// `injectedDeps`, `ignoredBuilds`, `allowBuilds`) default to empty
/// / unset.
///
/// `hoistedDependencies` is now produced by the hoist pass in
/// `InstallFrozenLockfile::run` and threaded in here — empty for the
/// no-lockfile path and for installs where both hoist patterns are
/// `None`. Persisting it lets a subsequent install detect a hoist
/// pattern change and re-hoist appropriately (the partial-install
/// path tracked at pnpm/pacquet#433 will consume it; today every
/// install does the full hoist anyway).
fn build_modules_manifest(
    config: &Config,
    included: IncludedDependencies,
    hoisted_dependencies: HoistedDependencies,
) -> Modules {
    Modules {
        hoist_pattern: config.hoist_pattern.clone(),
        hoisted_dependencies,
        included,
        layout_version: Some(LayoutVersion),
        node_linker: Some(map_node_linker(&config.node_linker)),
        // `${name}@${version}` per upstream. `CARGO_PKG_VERSION`
        // resolves at compile time to this crate's package version.
        package_manager: concat!("pacquet@", env!("CARGO_PKG_VERSION")).to_string(),
        public_hoist_pattern: config.public_hoist_pattern.clone(),
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
