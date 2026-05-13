use crate::{SkippedSnapshots, link_direct_dep_bins, symlink_package};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_cmd_shim::LinkBinsError;
use pacquet_config::Config;
use pacquet_lockfile::{
    ImporterDepVersion, PkgName, PkgNameVerPeer, ProjectSnapshot, ResolvedDependencySpec,
};
use pacquet_package_manifest::DependencyGroup;
use pacquet_reporter::{
    AddedRoot, DependencyType, LogEvent, LogLevel, Reporter, RootLog, RootMessage,
};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// Create the `node_modules/` symlinks for every importer in the lockfile.
///
/// For each `importers.<id>` entry:
///
/// - Resolve the importer's `rootDir = workspace_root.join(id)` (with
///   `id == "."` meaning the workspace root itself).
/// - For every direct dependency in the importer's groups, create the
///   appropriate symlink under `rootDir/node_modules/`. Snapshots that
///   resolve through the shared virtual store get a link to
///   `<virtual_store_dir>/<name>@<ver>/node_modules/<name>`. `link:`
///   snapshots (cross-importer `workspace:` deps) get a direct symlink
///   to the dependee's `rootDir`.
/// - Emit one `pnpm:root added` per direct dependency with the
///   importer's `rootDir` as the event prefix, matching upstream's
///   per-project emit at
///   <https://github.com/pnpm/pnpm/blob/94240bc046/installing/linking/direct-dep-linker/src/linkDirectDeps.ts#L131>.
///
/// The virtual store dir (`config.virtual_store_dir`) stays singular —
/// it lives at `workspace_root/node_modules/.pnpm`. Only the per-project
/// `node_modules/` and its symlinks fan out.
#[must_use]
pub struct SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub config: &'static Config,
    pub importers: &'a HashMap<String, ProjectSnapshot>,
    pub dependency_groups: DependencyGroupList,
    /// Workspace root. For a single-project install this is the
    /// directory containing the user's `package.json`; for a real
    /// workspace it's the directory containing `pnpm-workspace.yaml`.
    /// Same value as the `lockfileDir` upstream pnpm uses for
    /// `pnpm:stage` / `pnpm:summary` events.
    pub workspace_root: &'a Path,
    /// Snapshots the installability pass marked optional+incompatible.
    /// A direct dep whose resolved snapshot key is in this set is
    /// omitted from `node_modules/<name>` (no symlink, no
    /// `pnpm:root added` event, no bin linking). Mirrors pnpm's
    /// `linkDirectDeps` walk skipping entries whose `depPath` is
    /// in `skipPkgIds`.
    pub skipped: &'a SkippedSnapshots,
}

/// Error type of [`SymlinkDirectDependencies`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum SymlinkDirectDependenciesError {
    #[diagnostic(transparent)]
    LinkBins(#[error(source)] LinkBinsError),
}

impl<'a, DependencyGroupList> SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub fn run<R: Reporter>(self) -> Result<(), SymlinkDirectDependenciesError> {
        let SymlinkDirectDependencies {
            config,
            importers,
            dependency_groups,
            workspace_root,
            skipped,
        } = self;

        // Collect once so the same group order can drive every importer.
        // Upstream calls `linkDirectDeps` once with a per-importer
        // `dependencies` list, so the group order is shared across all
        // importers anyway.
        let dependency_groups: Vec<DependencyGroup> = dependency_groups.into_iter().collect();

        // Sorted iteration so `pnpm:root` event order stays
        // deterministic. The wire shape doesn't require this, but a
        // deterministic order makes assertions in tests (and the
        // upstream snapshot tests we will be porting) tractable.
        let mut keys: Vec<&str> = importers.keys().map(String::as_str).collect();
        keys.sort_unstable();

        for importer_id in keys {
            // Safe: we just iterated `importers.keys()`.
            let project_snapshot = &importers[importer_id];
            let project_dir = importer_root_dir(workspace_root, importer_id);
            let modules_dir = project_dir.join("node_modules");

            link_one_importer::<R>(
                config,
                project_snapshot,
                &project_dir,
                &modules_dir,
                dependency_groups.iter().copied(),
                skipped,
            )?;
        }

        Ok(())
    }
}

/// Resolve `importer_id` (a lockfile key) against the workspace root.
///
/// Pnpm's lockfile spec uses `"."` for the root importer and
/// forward-slash POSIX paths for sub-importers. Mirroring that here
/// keeps lockfiles written by pacquet and pnpm interchangeable. The
/// returned path is platform-native (`Path::join` handles the
/// conversion on Windows).
fn importer_root_dir(workspace_root: &Path, importer_id: &str) -> PathBuf {
    if importer_id == "." || importer_id.is_empty() {
        workspace_root.to_path_buf()
    } else {
        // `importer_id` is POSIX in the lockfile; `Path::join` accepts
        // forward slashes and converts to native separators.
        workspace_root.join(importer_id)
    }
}

fn link_one_importer<R: Reporter>(
    config: &Config,
    project_snapshot: &ProjectSnapshot,
    project_dir: &Path,
    modules_dir: &Path,
    dependency_groups: impl IntoIterator<Item = DependencyGroup>,
    skipped: &SkippedSnapshots,
) -> Result<(), SymlinkDirectDependenciesError> {
    // Iterate per group so each emit can label the dependency
    // with its [`DependencyType`]. pnpm's reporter renders the
    // diff with that hint, so dropping it would silently
    // misclassify devDependencies as prod.
    // [`ProjectSnapshot::dependencies_by_groups`] flattens the
    // groups together, which is convenient for the symlink loop
    // but loses the per-group identity we need for the emit.
    //
    // Peers are filtered upfront: pnpm doesn't emit `pnpm:root`
    // for peer dependencies (they're materialised through their
    // host package, not directly under `node_modules/`), and
    // [`ProjectSnapshot::get_map_by_group`] also returns `None`
    // for `Peer` so this filter is belt-and-braces. It lets
    // the per-group → [`DependencyType`] match below stay
    // exhaustive without a misleading `Peer` arm that maps to
    // an "absent" type.
    //
    // Dedup with a `HashSet<PkgName>`, first-wins. A v9 lockfile
    // pnpm itself wrote shouldn't list the same package across
    // multiple importer sections (pnpm's resolver normalises:
    // a package with `optional: true` lands in
    // `optionalDependencies` only). But pacquet ingests
    // user-supplied lockfiles, and a malformed one with the same
    // key in two sections would race two `symlink_package` calls
    // to the same `node_modules/<name>` and emit duplicate
    // `pnpm:root added` events. First-wins picks up the highest-
    // priority group from the caller-supplied
    // `dependency_groups` order. The CLI today passes
    // `[Prod, Dev, Optional]`, matching pnpm's
    // dependencies-over-optional precedence.
    let mut seen: HashSet<&PkgName> = HashSet::new();
    let entries: Vec<(&PkgName, &ResolvedDependencySpec, DependencyGroup)> = dependency_groups
        .into_iter()
        .filter(|group| !matches!(group, DependencyGroup::Peer))
        .flat_map(|group| {
            project_snapshot
                .get_map_by_group(group)
                .into_iter()
                .flatten()
                .map(move |(name, spec)| (name, spec, group))
        })
        .filter(|(name, _, _)| seen.insert(*name))
        // Drop direct deps whose resolved snapshot landed in the
        // skipped set. Without this filter, the symlink would
        // either dangle (no virtual-store slot was created) or —
        // worse — point at a half-installed slot from a prior
        // install. Mirrors pnpm's `linkDirectDeps` walk skipping
        // entries whose `depPath` is in `skipPkgIds`. `link:` deps
        // never participate in the virtual store, so they are
        // exempt from the skipped check (the resolved snapshot key
        // wouldn't exist in the set anyway).
        .filter(|(name, spec, _)| match &spec.version {
            ImporterDepVersion::Regular(ver) => {
                let resolved = PkgNameVerPeer::new(PkgName::clone(name), ver.clone());
                !skipped.contains(&resolved)
            }
            ImporterDepVersion::Link(_) => true,
        })
        .collect();

    // `prefix` for the `pnpm:root` envelope. Upstream uses the
    // project's `rootDir` so the JS reporter can scope progress to
    // the right project — `lockfileDir` is reserved for the install-
    // wide stage / summary events. See
    // <https://github.com/pnpm/pnpm/blob/94240bc046/installing/linking/direct-dep-linker/src/linkDirectDeps.ts#L131>.
    let prefix = project_dir.to_string_lossy().into_owned();

    entries.par_iter().for_each(|(name, spec, group)| {
        let name_str = name.to_string();
        let target_path: PathBuf = match &spec.version {
            ImporterDepVersion::Regular(ver_peer) => {
                // TODO: the code below is not optimal
                let virtual_store_name =
                    PkgNameVerPeer::new(PkgName::clone(name), ver_peer.clone())
                        .to_virtual_store_name();
                config
                    .virtual_store_dir
                    .join(virtual_store_name)
                    .join("node_modules")
                    .join(&name_str)
            }
            ImporterDepVersion::Link(target) => {
                // `link:<path>` values are relative to the
                // importer's `rootDir` (or absolute). Resolve them
                // here so the on-disk symlink points at the right
                // sibling project. Pnpm does the same conversion in
                // `lockfileToDepGraph` —
                // <https://github.com/pnpm/pnpm/blob/94240bc046/lockfile/types/src/index.ts>
                // — but pacquet's lockfile snapshot already carries
                // the raw `link:` payload, so the resolution lives
                // at the install layer.
                let candidate = Path::new(target);
                if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    project_dir.join(candidate)
                }
            }
        };

        symlink_package(&target_path, &modules_dir.join(&name_str)).expect("symlink pkg"); // TODO: properly propagate this error

        // `pnpm:root added` mirrors pnpm's emit at
        // <https://github.com/pnpm/pnpm/blob/94240bc046/installing/linking/direct-dep-linker/src/linkDirectDeps.ts#L131>:
        // one event per direct dependency once the symlink has
        // been created. pacquet's frozen-lockfile snapshot doesn't
        // preserve npm-alias keys at this layer, so `realName`
        // mirrors `name`; the optional `id` / `latest` /
        // `linkedFrom` fields are out of pacquet's reach today
        // and skip from the wire shape rather than serializing as
        // JSON `null`.
        let dependency_type = match group {
            DependencyGroup::Prod => DependencyType::Prod,
            DependencyGroup::Dev => DependencyType::Dev,
            DependencyGroup::Optional => DependencyType::Optional,
            // Filtered upfront. See the comment on the `entries`
            // builder above.
            DependencyGroup::Peer => unreachable!("peers are filtered out before this point"),
        };
        // For a `link:` dep, upstream's `version` field is the
        // resolved `link:<path>` payload (re-prepended on the
        // wire) so reporters can render the link target. Pacquet
        // mirrors that here; for `Regular` deps we keep the
        // semver-only formatting upstream uses on the wire.
        let version = match &spec.version {
            ImporterDepVersion::Regular(ver) => Some(ver.version().to_string()),
            ImporterDepVersion::Link(target) => Some(format!("link:{target}")),
        };
        // Pacquet's lockfile snapshot doesn't track the
        // npm-alias key separately from the resolved package
        // name at this layer, so `name` and `real_name` carry
        // the same value. Clone the already-built string
        // instead of formatting `name` a second time.
        let real_name = name_str.clone();
        R::emit(&LogEvent::Root(RootLog {
            level: LogLevel::Debug,
            message: RootMessage::Added {
                prefix: prefix.clone(),
                added: AddedRoot {
                    name: name_str,
                    real_name,
                    version,
                    dependency_type: Some(dependency_type),
                    id: None,
                    latest: None,
                    linked_from: None,
                },
            },
        }));
    });

    // After the symlinks exist, walk them to discover each
    // direct dep's `package.json` and link declared bins into
    // `<modules_dir>/.bin`. Mirrors pnpm v11's `linkBinsOfPackages`
    // call site for direct deps.
    let dep_names: Vec<String> = entries.iter().map(|(name, _, _)| name.to_string()).collect();
    link_direct_dep_bins(modules_dir, &dep_names)
        .map_err(SymlinkDirectDependenciesError::LinkBins)?;

    Ok(())
}

#[cfg(test)]
mod tests;
