use crate::State;
use clap::Args;
use miette::Context;
use pacquet_package_json::DependencyGroup;
use pacquet_package_manager::Add;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct AddDependencyOptions {
    /// Install the specified packages as regular dependencies.
    #[clap(short = 'P', long)]
    save_prod: bool,
    /// Install the specified packages as devDependencies.
    #[clap(short = 'D', long)]
    save_dev: bool,
    /// Install the specified packages as optionalDependencies.
    #[clap(short = 'O', long)]
    save_optional: bool,
    /// Using --save-peer will add one or more packages to peerDependencies and install them as dev dependencies
    #[clap(long)]
    save_peer: bool,
}

impl AddDependencyOptions {
    /// Convert the `--save-*` flags to an iterator of [`DependencyGroup`]
    /// which selects which target group to save to.
    fn dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        let &AddDependencyOptions { save_prod, save_dev, save_optional, save_peer } = self;
        let has_prod = save_prod || (!save_dev && !save_optional && !save_peer); // no --save-* flags implies --save-prod
        let has_dev = save_dev || (!save_prod && !save_optional && save_peer); // --save-peer with nothing else implies --save-dev
        let has_optional = save_optional;
        let has_peer = save_peer;
        std::iter::empty()
            .chain(has_prod.then_some(DependencyGroup::Default))
            .chain(has_dev.then_some(DependencyGroup::Dev))
            .chain(has_optional.then_some(DependencyGroup::Optional))
            .chain(has_peer.then_some(DependencyGroup::Peer))
    }
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Name of the package
    pub package: String,
    /// --save-prod, --save-dev, --save-optional, --save-peer
    #[clap(flatten)]
    pub dependency_options: AddDependencyOptions,
    /// Saved dependencies will be configured with an exact version rather than using
    /// pacquet's default semver range operator.
    #[clap(short = 'E', long = "save-exact")]
    pub save_exact: bool,
    /// The directory with links to the store (default is node_modules/.pacquet).
    /// All direct and indirect dependencies of the project are linked into this directory
    #[clap(long = "virtual-store-dir", default_value = "node_modules/.pacquet")]
    pub virtual_store_dir: Option<PathBuf>, // TODO: make use of this
}

impl AddArgs {
    /// Execute the subcommand.
    pub async fn run(self, mut state: State) -> miette::Result<()> {
        // TODO: if a package already exists in another dependency group, don't remove the existing entry.

        let State { config, package_json, lockfile, http_client, tarball_cache } = &mut state;

        Add {
            tarball_cache,
            http_client,
            config,
            package_json,
            lockfile: lockfile.as_ref(),
            list_dependency_groups: || self.dependency_options.dependency_groups(),
            package: &self.package,
            save_exact: self.save_exact,
        }
        .run()
        .await
        .wrap_err("adding a new package")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_package_json::DependencyGroup;
    use pretty_assertions::assert_eq;

    #[test]
    fn dependency_options_to_dependency_groups() {
        use DependencyGroup::{Default, Dev, Optional, Peer};
        let create_list = |opts: AddDependencyOptions| opts.dependency_groups().collect::<Vec<_>>();

        // no flags -> prod
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: false,
                save_peer: false
            }),
            [Default]
        );

        // --save-prod -> prod
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: true,
                save_dev: false,
                save_optional: false,
                save_peer: false
            }),
            [Default]
        );

        // --save-dev -> dev
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: true,
                save_optional: false,
                save_peer: false
            }),
            [Dev]
        );

        // --save-optional -> optional
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: true,
                save_peer: false
            }),
            [Optional]
        );

        // --save-peer -> dev + peer
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: false,
                save_peer: true
            }),
            [Dev, Peer]
        );

        // --save-prod --save-peer -> prod + peer
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: true,
                save_dev: false,
                save_optional: false,
                save_peer: true
            }),
            [Default, Peer]
        );

        // --save-dev --save-peer -> dev + peer
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: true,
                save_optional: false,
                save_peer: true
            }),
            [Dev, Peer]
        );

        // --save-optional --save-peer -> optional + peer
        assert_eq!(
            create_list(AddDependencyOptions {
                save_prod: false,
                save_dev: false,
                save_optional: true,
                save_peer: true
            }),
            [Optional, Peer]
        );
    }
}
