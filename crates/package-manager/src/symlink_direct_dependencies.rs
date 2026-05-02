use crate::symlink_package;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{Lockfile, PkgName, PkgNameVerPeer, ProjectSnapshot};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use pacquet_reporter::{
    AddedRoot, DependencyType, LogEvent, LogLevel, Reporter, RootLog, RootMessage,
};
use rayon::prelude::*;
use std::collections::HashMap;

/// This subroutine creates symbolic links in the `node_modules` directory for
/// the direct dependencies. The targets of the link are the virtual directories.
///
/// If package `foo@x.y.z` is declared as a dependency in `package.json`,
/// symlink `foo -> .pacquet/foo@x.y.z/node_modules/foo` shall be created
/// in the `node_modules` directory.
#[must_use]
pub struct SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub config: &'static Npmrc,
    pub importers: &'a HashMap<String, ProjectSnapshot>,
    pub dependency_groups: DependencyGroupList,
    /// Install root, threaded into the `pnpm:root` `prefix` field.
    /// Same value as the `prefix` in [`pacquet_reporter::StageLog`].
    pub requester: &'a str,
}

/// Error type of [`SymlinkDirectDependencies`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum SymlinkDirectDependenciesError {
    #[display(
        "Lockfile has no `importers.{root_key:?}` entry for the root project; pacquet cannot decide which direct dependencies to symlink into `node_modules`."
    )]
    #[diagnostic(code(pacquet_package_manager::missing_root_importer))]
    MissingRootImporter { root_key: String },
}

impl<'a, DependencyGroupList> SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub fn run<R: Reporter>(self) -> Result<(), SymlinkDirectDependenciesError> {
        let SymlinkDirectDependencies { config, importers, dependency_groups, requester } = self;

        let project_snapshot = importers.get(Lockfile::ROOT_IMPORTER_KEY).ok_or_else(|| {
            SymlinkDirectDependenciesError::MissingRootImporter {
                root_key: Lockfile::ROOT_IMPORTER_KEY.to_string(),
            }
        })?;

        // Iterate per group so each emit can label the dependency
        // with its [`DependencyType`] — pnpm's reporter renders the
        // diff with that hint, so dropping it would silently
        // misclassify devDependencies as prod.
        // [`ProjectSnapshot::dependencies_by_groups`] flattens the
        // groups together, which is convenient for the symlink loop
        // but loses the per-group identity we need for the emit.
        let entries: Vec<(&PkgName, &_, DependencyGroup)> = dependency_groups
            .into_iter()
            .flat_map(|group| {
                project_snapshot
                    .get_map_by_group(group)
                    .into_iter()
                    .flatten()
                    .map(move |(name, spec)| (name, spec, group))
            })
            .collect();

        entries.par_iter().for_each(|(name, spec, group)| {
            // TODO: the code below is not optimal
            let virtual_store_name =
                PkgNameVerPeer::new(PkgName::clone(name), spec.version.clone())
                    .to_virtual_store_name();

            let name_str = name.to_string();
            symlink_package(
                &config
                    .virtual_store_dir
                    .join(virtual_store_name)
                    .join("node_modules")
                    .join(&name_str),
                &config.modules_dir.join(&name_str),
            )
            .expect("symlink pkg"); // TODO: properly propagate this error

            // `pnpm:root added` mirrors pnpm's emit at
            // <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/linking/direct-dep-linker/src/linkDirectDeps.ts#L131>:
            // one event per direct dependency once the symlink has
            // been created. pacquet's frozen-lockfile snapshot doesn't
            // preserve npm-alias keys at this layer, so `realName`
            // mirrors `name`; the optional `id` / `latest` /
            // `linkedFrom` fields are out of pacquet's reach today
            // and skip from the wire shape rather than serializing as
            // JSON `null`.
            let dependency_type = match group {
                DependencyGroup::Prod => Some(DependencyType::Prod),
                DependencyGroup::Dev => Some(DependencyType::Dev),
                DependencyGroup::Optional => Some(DependencyType::Optional),
                DependencyGroup::Peer => None,
            };
            R::emit(&LogEvent::Root(RootLog {
                level: LogLevel::Debug,
                message: RootMessage::Added {
                    prefix: requester.to_owned(),
                    added: AddedRoot {
                        name: name_str,
                        real_name: name.to_string(),
                        version: Some(spec.version.version().to_string()),
                        dependency_type,
                        id: None,
                        latest: None,
                        linked_from: None,
                    },
                },
            }));
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests;
