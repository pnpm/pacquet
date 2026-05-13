use crate::{
    AllowBuildPolicy, CreateVirtualDirBySnapshot, CreateVirtualDirError, VirtualStoreLayout,
    retry_config::retry_opts_from_config,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_config::Config;
use pacquet_executor::ScriptsPrependNodePath as ExecScriptsPrependNodePath;
use pacquet_git_fetcher::{GitFetchOutput, GitFetcher, GitFetcherError, GitHostedTarballFetcher};
use pacquet_graph_hasher::{host_arch, host_libc, host_platform};
use pacquet_lockfile::{
    BinaryArchive, BinaryResolution, LockfileResolution, PackageKey, PackageMetadata,
    PlatformSelector, SnapshotEntry, select_platform_variant,
};
use pacquet_network::ThrottledClient;
use pacquet_reporter::{LogEvent, LogLevel, ProgressLog, ProgressMessage, Reporter};
use pacquet_store_dir::{
    SharedReadonlyStoreIndex, SharedVerifiedFilesCache, StoreIndexWriter,
    git_hosted_store_index_key,
};
use pacquet_tarball::{
    DownloadTarballToStore, DownloadZipArchiveToStore, PrefetchedCasPaths, TarballError,
};
use pipe_trait::Pipe;
use std::{
    borrow::Cow,
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU8},
};

/// This subroutine downloads a package tarball, extracts it, installs it to a
/// virtual dir, then creates the symlink layout for the package. CAS file
/// import and symlink creation run concurrently via `rayon::join` inside
/// [`CreateVirtualDirBySnapshot::run`].
#[must_use]
pub struct InstallPackageBySnapshot<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Config,
    /// Install-scoped slot-directory mapping (GVS-aware). Drives the
    /// per-snapshot directory passed to
    /// [`CreateVirtualDirBySnapshot`] after the cold-batch download
    /// finishes. See [`crate::VirtualStoreLayout`].
    pub layout: &'a VirtualStoreLayout,
    pub store_index: Option<&'a SharedReadonlyStoreIndex>,
    pub store_index_writer: Option<&'a Arc<StoreIndexWriter>>,
    /// Install-scoped batched cache lookup result. See
    /// [`pacquet_tarball::prefetch_cas_paths`].
    pub prefetched_cas_paths: Option<&'a PrefetchedCasPaths>,
    /// Install-scoped `verifiedFilesCache` shared across every
    /// per-snapshot fetch. See `DownloadTarballToStore::verified_files_cache`
    /// for the rationale.
    pub verified_files_cache: &'a SharedVerifiedFilesCache,
    /// Install-scoped dedupe state for `pnpm:package-import-method`.
    /// See `link_file::log_method_once`.
    pub logged_methods: &'a AtomicU8,
    /// Install root, threaded into reporter events (`pnpm:progress`'s
    /// `requester`). Same value as the `prefix` in
    /// [`pacquet_reporter::StageLog`].
    pub requester: &'a str,
    pub package_key: &'a PackageKey,
    pub metadata: &'a PackageMetadata,
    pub snapshot: &'a SnapshotEntry,
    /// `allowBuilds` gate. Routed into the git fetcher for
    /// `preparePackage`'s `GIT_DEP_PREPARE_NOT_ALLOWED` check.
    /// Computed once per install in
    /// [`crate::InstallFrozenLockfile::run`] and threaded through
    /// [`crate::CreateVirtualStore`].
    pub allow_build_policy: &'a AllowBuildPolicy,
}

/// Error type of [`InstallPackageBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallPackageBySnapshotError {
    #[diagnostic(transparent)]
    DownloadTarball(#[error(source)] TarballError),

    #[diagnostic(transparent)]
    CreateVirtualDir(#[error(source)] CreateVirtualDirError),

    #[display(
        "Package `{package_key}` has a tarball resolution without an `integrity` field; pacquet cannot verify the download and refuses to install it."
    )]
    #[diagnostic(code(pacquet_package_manager::missing_tarball_integrity))]
    MissingTarballIntegrity { package_key: String },

    #[display(
        "Package `{package_key}` uses a `{resolution_kind}` resolution, which pacquet does not yet support."
    )]
    #[diagnostic(code(pacquet_package_manager::unsupported_resolution))]
    UnsupportedResolution { package_key: String, resolution_kind: &'static str },

    /// Failure from either git fetcher: the git-CLI path for
    /// `type: git` resolutions (clone / checkout / preparePackage /
    /// CAS import) or the git-hosted-tarball post-pass for
    /// `TarballResolution { gitHosted: true }` (materialize /
    /// preparePackage / packlist / re-import). Both share the same
    /// `GitFetcherError` taxonomy because they share `prepare_package`,
    /// `packlist`, and the CAS-import helpers; the variant covers
    /// every fetcher path that exits through `pacquet-git-fetcher`.
    #[diagnostic(transparent)]
    GitFetch(#[error(source)] GitFetcherError),

    /// No variant in a [`LockfileResolution::Variations`] matches the
    /// host triple `(os, cpu, libc?)`. Surfaces with the host triple
    /// plus the list of advertised target triples so the user can see
    /// at a glance whether they're running on an unsupported platform
    /// or whether the lockfile was generated without the host's
    /// architecture in mind.
    #[display(
        "Package `{package_key}` is a runtime dependency, but none of its declared variants matches the host triple (os = `{host_os}`, cpu = `{host_cpu}`, libc = `{host_libc:?}`). Available variants: {available_targets}"
    )]
    #[diagnostic(code(pacquet_package_manager::no_matching_platform_variant))]
    NoMatchingPlatformVariant {
        package_key: String,
        host_os: &'static str,
        host_cpu: &'static str,
        host_libc: Option<&'static str>,
        /// Pre-rendered list of the lockfile's advertised target
        /// triples, formatted as `os/cpu[+libc]`. Lives in the error
        /// payload rather than the lockfile (which is borrowed from
        /// the install request) so the error stays cheap to construct
        /// at the rejection site and isn't tied to the lockfile's
        /// lifetime.
        available_targets: String,
    },

    /// A variant inside a [`LockfileResolution::Variations`] carries
    /// a resolution other than [`LockfileResolution::Binary`].
    /// Upstream contract guarantees variants are atomic
    /// `BinaryResolution`s; this variant catches lockfile corruption
    /// or a future shape pacquet doesn't recognise rather than
    /// silently routing through and confusing the install pipeline.
    #[display(
        "Package `{package_key}` carries a runtime variant whose inner resolution is `{inner_kind}` rather than `binary`; pacquet only knows how to install binary-shaped variants."
    )]
    #[diagnostic(code(pacquet_package_manager::variant_has_non_binary_resolution))]
    VariantHasNonBinaryResolution { package_key: String, inner_kind: &'static str },
}

impl<'a> InstallPackageBySnapshot<'a> {
    /// Execute the subroutine.
    pub async fn run<R: Reporter>(self) -> Result<(), InstallPackageBySnapshotError> {
        let InstallPackageBySnapshot {
            http_client,
            config,
            layout,
            store_index,
            store_index_writer,
            prefetched_cas_paths,
            verified_files_cache,
            logged_methods,
            requester,
            package_key,
            metadata,
            snapshot,
            allow_build_policy,
        } = self;

        // TODO: skip when already exists in store?
        let package_id = package_key.without_peer().to_string();
        emit_progress_resolved::<R>(&package_id, requester);

        // Adapter shared between the `Git` arm below and the
        // `gitHosted: true` post-pass on tarballs. Named local so
        // both fetchers can borrow it across their `.await` without
        // depending on temporary-lifetime extension.
        //
        // `AllowBuildPolicy::check` returns `None` when the package
        // is neither allow-listed nor deny-listed. Default-deny
        // (`None → false`) matches pnpm v11's policy: build scripts
        // have to be explicitly opted in to run.
        let allow_build_closure =
            |name: &str, version: &str| allow_build_policy.check(name, version).unwrap_or(false);
        let scripts_prepend_node_path = match config.scripts_prepend_node_path {
            pacquet_config::ScriptsPrependNodePath::Always => ExecScriptsPrependNodePath::Always,
            pacquet_config::ScriptsPrependNodePath::Never => ExecScriptsPrependNodePath::Never,
            pacquet_config::ScriptsPrependNodePath::WarnOnly => {
                ExecScriptsPrependNodePath::WarnOnly
            }
        };

        let cas_paths: HashMap<String, PathBuf> = match &metadata.resolution {
            LockfileResolution::Tarball(_) | LockfileResolution::Registry(_) => {
                let (tarball_url, integrity) =
                    tarball_url_and_integrity(&metadata.resolution, package_key, config)?;
                let raw_cas_paths = DownloadTarballToStore {
                    http_client,
                    store_dir: &config.store_dir,
                    store_index: store_index.cloned(),
                    store_index_writer: store_index_writer.cloned(),
                    verify_store_integrity: config.verify_store_integrity,
                    verified_files_cache: Arc::clone(verified_files_cache),
                    package_integrity: integrity,
                    package_unpacked_size: None,
                    package_url: &tarball_url,
                    package_id: &package_id,
                    requester,
                    prefetched_cas_paths,
                    retry_opts: retry_opts_from_config(config),
                    auth_headers: &config.auth_headers,
                    ignore_file_pattern: None,
                }
                .run_without_mem_cache::<R>()
                .await
                .map_err(InstallPackageBySnapshotError::DownloadTarball)?;

                // Run the git-hosted prepare+packlist pass for
                // tarballs sourced from a git host. Mirrors pnpm's
                // dispatch at `fetching/pick-fetcher/src/index.ts`:
                // a `gitHosted: true` tarball routes through
                // `gitHostedTarballFetcher` rather than the plain
                // `remoteTarballFetcher`, because the host's archive
                // endpoint doesn't run `prepare`/`prepublish*` and
                // the file set typically needs packlist filtering.
                if let LockfileResolution::Tarball(t) = &metadata.resolution
                    && t.git_hosted == Some(true)
                {
                    // `built = true` matches the dispatcher's default
                    // (`ignore_scripts: false` everywhere). When
                    // pacquet adds a configurable ignore-scripts mode
                    // this `true` flips to `!ignore_scripts`, in lock-
                    // step with the key shape `snapshot_cache_key`
                    // produces — otherwise the prefetch and the write
                    // would address different slots.
                    let files_index_file = git_hosted_store_index_key(&package_id, true);
                    let GitFetchOutput { cas_paths, built: _built } = GitHostedTarballFetcher {
                        cas_paths: raw_cas_paths,
                        path: t.path.as_deref(),
                        allow_build: &allow_build_closure,
                        ignore_scripts: false,
                        unsafe_perm: config.unsafe_perm,
                        user_agent: None,
                        scripts_prepend_node_path,
                        script_shell: None,
                        node_execpath: None,
                        npm_execpath: None,
                        store_dir: &config.store_dir,
                        package_id: &package_id,
                        requester,
                        store_index_writer,
                        files_index_file: &files_index_file,
                    }
                    .run::<R>()
                    .await
                    .map_err(InstallPackageBySnapshotError::GitFetch)?;
                    cas_paths
                } else {
                    raw_cas_paths
                }
            }
            LockfileResolution::Directory(_) => {
                return Err(InstallPackageBySnapshotError::UnsupportedResolution {
                    package_key: package_key.to_string(),
                    resolution_kind: "directory",
                });
            }
            // Slice A of #437 wires the lockfile types; the install
            // dispatch for `Binary` / `Variations` lands in Slice D.
            // Until then, surface the kind via the typed
            // `UnsupportedResolution` error so a v11 lockfile with a
            // Runtime artifacts (Node.js / Bun / Deno) — `Binary`
            // and `Variations` carry a `BinaryResolution` describing
            // the archive to fetch. `Variations` is the multi-
            // platform wrapper: pick the variant whose `targets`
            // includes the host triple, then route through the same
            // `BinaryResolution` extractor (mirrors upstream's
            // [`binary-fetcher/src/index.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/fetching/binary-fetcher/src/index.ts)).
            LockfileResolution::Binary(binary) => {
                fetch_binary_resolution_to_cas::<R>(
                    binary,
                    http_client,
                    config,
                    store_index,
                    store_index_writer,
                    verified_files_cache,
                    prefetched_cas_paths,
                    &package_id,
                    requester,
                )
                .await?
            }
            LockfileResolution::Variations(variations) => {
                let selector = host_platform_selector();
                let Some(variant) = select_platform_variant(&variations.variants, &selector) else {
                    return Err(InstallPackageBySnapshotError::NoMatchingPlatformVariant {
                        package_key: package_key.to_string(),
                        host_os: host_platform(),
                        host_cpu: host_arch(),
                        host_libc: match host_libc() {
                            "unknown" => None,
                            other => Some(other),
                        },
                        available_targets: render_variant_targets(&variations.variants),
                    });
                };
                // Upstream's `PlatformAssetResolution.resolution`
                // is always atomic (`BinaryResolution`); pacquet's
                // type widens to the full `LockfileResolution` for
                // serde uniformity but `select_platform_variant`'s
                // docs spell out that nested `Variations` would just
                // route their picked variant's inner shape back
                // through this dispatcher (no infinite recursion
                // because this arm doesn't call back into the
                // variant selector). The match below only
                // recognises `Binary`; anything else is either a
                // corrupt lockfile or a future shape pacquet hasn't
                // learned about yet, so reject loudly rather than
                // silently route through.
                let LockfileResolution::Binary(binary) = &variant.resolution else {
                    return Err(InstallPackageBySnapshotError::VariantHasNonBinaryResolution {
                        package_key: package_key.to_string(),
                        inner_kind: match &variant.resolution {
                            LockfileResolution::Tarball(_) => "tarball",
                            LockfileResolution::Registry(_) => "registry",
                            LockfileResolution::Directory(_) => "directory",
                            LockfileResolution::Git(_) => "git",
                            LockfileResolution::Variations(_) => "variations",
                            // Already matched above; reach is unreachable.
                            LockfileResolution::Binary(_) => "binary",
                        },
                    });
                };
                fetch_binary_resolution_to_cas::<R>(
                    binary,
                    http_client,
                    config,
                    store_index,
                    store_index_writer,
                    verified_files_cache,
                    prefetched_cas_paths,
                    &package_id,
                    requester,
                )
                .await?
            }
            LockfileResolution::Git(git_resolution) => {
                // Same `built = true` rationale as the git-hosted
                // tarball branch above — key shape stays in lock-step
                // with `snapshot_cache_key`.
                let files_index_file = git_hosted_store_index_key(&package_id, true);
                let GitFetchOutput { cas_paths, built: _built } = GitFetcher {
                    repo: &git_resolution.repo,
                    commit: &git_resolution.commit,
                    path: git_resolution.path.as_deref(),
                    git_shallow_hosts: &config.git_shallow_hosts,
                    allow_build: &allow_build_closure,
                    ignore_scripts: false,
                    unsafe_perm: config.unsafe_perm,
                    user_agent: None,
                    scripts_prepend_node_path,
                    script_shell: None,
                    node_execpath: None,
                    npm_execpath: None,
                    store_dir: &config.store_dir,
                    package_id: &package_id,
                    requester,
                    store_index_writer,
                    files_index_file: &files_index_file,
                    git_bin: None,
                }
                .run::<R>()
                .await
                .map_err(InstallPackageBySnapshotError::GitFetch)?;
                cas_paths
            }
        };

        CreateVirtualDirBySnapshot {
            layout,
            cas_paths: &cas_paths,
            import_method: config.package_import_method,
            logged_methods,
            requester,
            package_id: &package_id,
            package_key,
            snapshot,
        }
        .run::<R>()
        .map_err(InstallPackageBySnapshotError::CreateVirtualDir)?;

        Ok(())
    }
}

/// Resolve the tarball URL + integrity for tarball- and registry-shaped
/// resolutions. Factored out so the per-resolution-type dispatch in
/// [`InstallPackageBySnapshot::run`] reads top-down: each variant builds
/// its own `cas_paths`.
fn tarball_url_and_integrity<'a>(
    resolution: &'a LockfileResolution,
    package_key: &PackageKey,
    config: &'a Config,
) -> Result<(Cow<'a, str>, &'a ssri::Integrity), InstallPackageBySnapshotError> {
    match resolution {
        LockfileResolution::Tarball(tarball_resolution) => {
            let integrity = tarball_resolution.integrity.as_ref().ok_or_else(|| {
                InstallPackageBySnapshotError::MissingTarballIntegrity {
                    package_key: package_key.to_string(),
                }
            })?;
            Ok((tarball_resolution.tarball.as_str().pipe(Cow::Borrowed), integrity))
        }
        LockfileResolution::Registry(registry_resolution) => {
            let registry = config.registry.strip_suffix('/').unwrap_or(&config.registry);
            let name = &package_key.name;
            let version = package_key.suffix.version();
            let bare_name = name.bare.as_str();
            let tarball_url = format!("{registry}/{name}/-/{bare_name}-{version}.tgz");
            Ok((Cow::Owned(tarball_url), &registry_resolution.integrity))
        }
        // Caller (`run`) only invokes this helper for the tarball /
        // registry arms; git, directory, binary, and variations
        // resolutions never reach here. Return an unreachable-style
        // error so a future caller that forgets to gate gets a
        // clear panic in debug.
        LockfileResolution::Directory(_)
        | LockfileResolution::Git(_)
        | LockfileResolution::Binary(_)
        | LockfileResolution::Variations(_) => {
            unreachable!("tarball_url_and_integrity called with non-tarball resolution");
        }
    }
}

/// Build the host's [`PlatformSelector`] for runtime-variant
/// matching. Mirrors pnpm's call shape at the binary-fetcher
/// dispatch site: `{ os: process.platform, cpu: process.arch, libc:
/// process.platform === 'linux' ? family : null }`.
///
/// `host_libc()` returns `"unknown"` on every non-Linux host and
/// `"glibc"` / `"musl"` on Linux. Translate `"unknown"` to `None`
/// so [`select_platform_variant`]'s asymmetric libc rule applies
/// the same way upstream's does: `None` and `Some("glibc")` both
/// require the variant to omit `libc`, and `Some("musl")` requires
/// an exact match.
pub(crate) fn host_platform_selector() -> PlatformSelector {
    let libc = match host_libc() {
        "unknown" => None,
        other => Some(other.to_string()),
    };
    PlatformSelector { os: host_platform().to_string(), cpu: host_arch().to_string(), libc }
}

/// Fetch a [`BinaryResolution`] into the CAS, returning the
/// per-file `{relative_path → cas_path}` map the snapshot's virtual
/// directory needs. Dispatches on the archive type:
///
/// - [`BinaryArchive::Tarball`] uses [`DownloadTarballToStore`]
///   with `package_unpacked_size: None` (binary archives don't
///   carry that hint upstream either).
/// - [`BinaryArchive::Zip`] uses [`DownloadZipArchiveToStore`]
///   with `archive_prefix: binary.prefix.as_deref()` so the runtime
///   archive's top-level wrapper (e.g.
///   `node-v22.0.0-darwin-arm64/`) is stripped before the CAS keys
///   are written.
///
/// The Node-runtime `NODE_EXTRAS_IGNORE_PATTERN` filter that strips
/// bundled `npm` / `corepack` from the archive will land in Slice
/// D2; for now the filter slot stays `None` and the full archive
/// contents are imported. Bin-link cmd-shims for the runtime
/// executables likewise wait for Slice D2.
#[expect(
    clippy::too_many_arguments,
    reason = "matches the field set DownloadTarballToStore / DownloadZipArchiveToStore need"
)]
async fn fetch_binary_resolution_to_cas<R: Reporter>(
    binary: &BinaryResolution,
    http_client: &ThrottledClient,
    config: &'static Config,
    store_index: Option<&SharedReadonlyStoreIndex>,
    store_index_writer: Option<&Arc<StoreIndexWriter>>,
    verified_files_cache: &SharedVerifiedFilesCache,
    prefetched_cas_paths: Option<&PrefetchedCasPaths>,
    package_id: &str,
    requester: &str,
) -> Result<HashMap<String, PathBuf>, InstallPackageBySnapshotError> {
    match binary.archive {
        BinaryArchive::Tarball => DownloadTarballToStore {
            http_client,
            store_dir: &config.store_dir,
            store_index: store_index.cloned(),
            store_index_writer: store_index_writer.cloned(),
            verify_store_integrity: config.verify_store_integrity,
            verified_files_cache: Arc::clone(verified_files_cache),
            package_integrity: &binary.integrity,
            package_unpacked_size: None,
            package_url: &binary.url,
            package_id,
            requester,
            prefetched_cas_paths,
            retry_opts: retry_opts_from_config(config),
            auth_headers: &config.auth_headers,
            ignore_file_pattern: None,
        }
        .run_without_mem_cache::<R>()
        .await
        .map_err(InstallPackageBySnapshotError::DownloadTarball),
        BinaryArchive::Zip => DownloadZipArchiveToStore {
            http_client,
            store_dir: &config.store_dir,
            store_index: store_index.cloned(),
            store_index_writer: store_index_writer.cloned(),
            verify_store_integrity: config.verify_store_integrity,
            verified_files_cache: Arc::clone(verified_files_cache),
            package_integrity: &binary.integrity,
            package_url: &binary.url,
            package_id,
            requester,
            prefetched_cas_paths,
            retry_opts: retry_opts_from_config(config),
            auth_headers: &config.auth_headers,
            archive_prefix: binary.prefix.as_deref(),
            ignore_file_pattern: None,
        }
        .run_without_mem_cache::<R>()
        .await
        .map_err(InstallPackageBySnapshotError::DownloadTarball),
    }
}

/// Render a variant's target list as a human-readable string for
/// inclusion in the [`InstallPackageBySnapshotError::NoMatchingPlatformVariant`]
/// error. Each target is rendered as `os/cpu` or `os/cpu+libc`,
/// joined with `, `.
fn render_variant_targets(variants: &[pacquet_lockfile::PlatformAssetResolution]) -> String {
    let mut entries: Vec<String> = Vec::new();
    for variant in variants {
        for target in &variant.targets {
            match &target.libc {
                Some(libc) => entries.push(format!("{}/{}+{libc}", target.os, target.cpu)),
                None => entries.push(format!("{}/{}", target.os, target.cpu)),
            }
        }
    }
    entries.join(", ")
}

/// `pnpm:progress` `resolved` for a frozen-lockfile snapshot the
/// cold-batch path is about to fetch. Mirrors pnpm's emit at
/// <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-resolver/src/resolveDependencies.ts#L1586>:
/// one event per (resolved) package, fired before the fetch
/// attempt. In pacquet's frozen-lockfile path the lockfile *is* the
/// resolution, so each snapshot is "already resolved" by the time
/// we reach this site.
///
/// Pulled out of [`InstallPackageBySnapshot::run`] so the
/// event-construction code is unit-testable; the call site itself
/// only fires when a non-empty cold-batch lockfile install runs,
/// which the existing test suite doesn't cover.
fn emit_progress_resolved<R: Reporter>(package_id: &str, requester: &str) {
    R::emit(&LogEvent::Progress(ProgressLog {
        level: LogLevel::Debug,
        message: ProgressMessage::Resolved {
            package_id: package_id.to_owned(),
            requester: requester.to_owned(),
        },
    }));
}

#[cfg(test)]
mod tests;
