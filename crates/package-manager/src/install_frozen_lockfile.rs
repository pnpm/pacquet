use crate::{
    CreateVirtualStore, CreateVirtualStoreError, LinkBins, LinkBinsError,
    SymlinkDirectDependencies, SymlinkDirectDependenciesError,
    check_platform::is_platform_supported,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{
    Lockfile, PackageKey, PackageMetadata, PkgNameVerPeer, ProjectSnapshot, SnapshotEntry,
};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use std::collections::{HashMap, HashSet, VecDeque};

/// This subroutine installs dependencies from a frozen lockfile.
///
/// **Brief overview:**
/// * Iterate over each snapshot in the v9 `snapshots:` map.
/// * Fetch the tarball for the matching `packages:` entry.
/// * Extract each tarball into the store directory.
/// * Import the files from the store dir to each `node_modules/.pacquet/{name}@{version}/node_modules/{name}/`.
/// * Create dependency symbolic links in each `node_modules/.pacquet/{name}@{version}/node_modules/`.
/// * Create a symbolic link at each `node_modules/{name}`.
/// * Create `.bin` symlinks for all installed packages with executables.
#[must_use]
pub struct InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub importers: &'a HashMap<String, ProjectSnapshot>,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    pub dependency_groups: DependencyGroupList,
}

/// Error type of [`InstallFrozenLockfile`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallFrozenLockfileError {
    #[diagnostic(transparent)]
    CreateVirtualStore(#[error(source)] CreateVirtualStoreError),

    #[diagnostic(transparent)]
    SymlinkDirectDependencies(#[error(source)] SymlinkDirectDependenciesError),

    #[diagnostic(transparent)]
    LinkBins(#[error(source)] LinkBinsError),
}

impl<'a, DependencyGroupList> InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) -> Result<(), InstallFrozenLockfileError> {
        let InstallFrozenLockfile {
            http_client,
            config,
            importers,
            packages,
            snapshots,
            dependency_groups,
        } = self;

        // TODO: check if the lockfile is out-of-date

        // Collect once so we can pass the same slice to symlink and bin-link steps.
        let dependency_groups: Vec<DependencyGroup> = dependency_groups.into_iter().collect();

        // Compute which optional snapshots are incompatible with the current
        // platform before touching the filesystem. Snapshots in this set are
        // skipped by CreateVirtualStore, SymlinkDirectDependencies, and
        // LinkBins. Mirrors pnpm's `packageIsInstallable` + optional-path
        // tracking in the headless restorer.
        let skipped_snapshots =
            compute_skipped_snapshots(importers, snapshots, packages.unwrap_or(&HashMap::new()));

        CreateVirtualStore {
            http_client,
            config,
            packages,
            snapshots,
            skipped_snapshots: &skipped_snapshots,
        }
        .run()
        .await
        .map_err(InstallFrozenLockfileError::CreateVirtualStore)?;

        SymlinkDirectDependencies {
            config,
            importers,
            dependency_groups: dependency_groups.iter().copied(),
            skipped_snapshots: &skipped_snapshots,
        }
        .run()
        .map_err(InstallFrozenLockfileError::SymlinkDirectDependencies)?;

        LinkBins {
            config,
            importers,
            packages,
            snapshots,
            dependency_groups: &dependency_groups,
            skipped_snapshots: &skipped_snapshots,
        }
        .run()
        .map_err(InstallFrozenLockfileError::LinkBins)?;

        Ok(())
    }
}

/// Compute the set of snapshot keys that should be skipped.
///
/// A snapshot is skipped when **both** conditions hold:
/// 1. It is only reachable via optional dependency edges (if it is also
///    reachable via a required edge it must be installed even if the platform
///    check fails, and a separate diagnostic should be emitted — not yet
///    implemented).
/// 2. Its `os` / `cpu` / `libc` constraints (from the `packages:` section)
///    are incompatible with the current platform.
///
/// Algorithm: BFS from the root importer, tracking whether each reachable
/// snapshot was reached via at least one non-optional path (`required`) or
/// only via optional paths (`optional_only`). Required beats optional: once a
/// snapshot is in `required` it cannot be demoted.
///
/// Upstream reference:
/// <https://github.com/pnpm/pnpm/blob/3f37d17b23/config/package-is-installable/src/checkPlatform.ts>
fn compute_skipped_snapshots(
    importers: &HashMap<String, ProjectSnapshot>,
    snapshots: Option<&HashMap<PackageKey, SnapshotEntry>>,
    packages: &HashMap<PackageKey, PackageMetadata>,
) -> HashSet<PackageKey> {
    let Some(snapshots) = snapshots else { return HashSet::new() };

    let mut required: HashSet<PackageKey> = HashSet::new();
    let mut optional_only: HashSet<PackageKey> = HashSet::new();
    // (key, is_on_optional_path)
    let mut queue: VecDeque<(PackageKey, bool)> = VecDeque::new();

    let enqueue = |key: PackageKey,
                   is_optional: bool,
                   required: &mut HashSet<PackageKey>,
                   optional_only: &mut HashSet<PackageKey>,
                   queue: &mut VecDeque<(PackageKey, bool)>| {
        if is_optional {
            if !required.contains(&key) && optional_only.insert(key.clone()) {
                queue.push_back((key, true));
            }
        } else {
            let newly_required = required.insert(key.clone());
            let was_optional = optional_only.remove(&key);
            if newly_required || was_optional {
                queue.push_back((key, false));
            }
        }
    };

    // Seed from root importer's direct dependencies.
    if let Some(root) = importers.get(Lockfile::ROOT_IMPORTER_KEY) {
        for (name, spec) in
            root.dependencies_by_groups([DependencyGroup::Prod, DependencyGroup::Dev])
        {
            let key = PkgNameVerPeer::new(name.clone(), spec.version.clone());
            enqueue(key, false, &mut required, &mut optional_only, &mut queue);
        }
        for (name, spec) in root.dependencies_by_groups([DependencyGroup::Optional]) {
            let key = PkgNameVerPeer::new(name.clone(), spec.version.clone());
            enqueue(key, true, &mut required, &mut optional_only, &mut queue);
        }
    }

    // BFS through snapshot dependency edges.
    while let Some((key, is_optional_path)) = queue.pop_front() {
        let Some(snapshot) = snapshots.get(&key) else { continue };

        for (dep_name, dep_ref) in snapshot.dependencies.iter().flatten() {
            let dep_key = dep_ref.resolve(dep_name);
            enqueue(dep_key, is_optional_path, &mut required, &mut optional_only, &mut queue);
        }
        for (dep_name, dep_ref) in snapshot.optional_dependencies.iter().flatten() {
            let dep_key = dep_ref.resolve(dep_name);
            // Optional deps are always on an optional path regardless of parent.
            enqueue(dep_key, true, &mut required, &mut optional_only, &mut queue);
        }
    }

    // A snapshot is skipped when it is optional-only AND platform-incompatible.
    optional_only
        .into_iter()
        .filter(|key| {
            let metadata_key = key.without_peer();
            let Some(meta) = packages.get(&metadata_key) else { return false };
            !is_platform_supported(meta.os.as_deref(), meta.cpu.as_deref(), meta.libc.as_deref())
        })
        .collect()
}
