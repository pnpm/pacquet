use crate::package_manager::{PackageManager, PackageManagerError};
use clap::Parser;
use pacquet_package_json::DependencyGroup;
use pacquet_package_manager::Install;

#[derive(Debug, Parser)]
pub struct CliInstallDependencyOptions {
    /// pacquet will not install any package listed in devDependencies and will remove those insofar
    /// they were already installed, if the NODE_ENV environment variable is set to production.
    /// Use this flag to instruct pacquet to ignore NODE_ENV and take its production status from this
    /// flag instead.
    #[arg(short = 'P', long = "prod")]
    pub prod: bool,
    /// Only devDependencies are installed and dependencies are removed insofar they were
    /// already installed, regardless of the NODE_ENV.
    #[arg(short = 'D', long = "dev")]
    pub dev: bool,
    /// optionalDependencies are not installed.
    #[arg(long = "no-optional")]
    pub no_optional: bool,
}

impl CliInstallDependencyOptions {
    /// Convert the command arguments to an iterator of [`DependencyGroup`]
    /// which filters the types of dependencies to install.
    fn dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        let &CliInstallDependencyOptions { prod, dev, no_optional } = self;
        let has_both = prod == dev;
        let has_prod = has_both || prod;
        let has_dev = has_both || dev;
        let has_optional = !no_optional;
        std::iter::empty()
            .chain(has_prod.then_some(DependencyGroup::Default))
            .chain(has_dev.then_some(DependencyGroup::Dev))
            .chain(has_optional.then_some(DependencyGroup::Optional))
    }
}

#[derive(Parser, Debug)]
pub struct InstallArgs {
    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    pub dependency_options: CliInstallDependencyOptions,

    /// Don't generate a lockfile and fail if the lockfile is outdated.
    #[clap(long)]
    pub frozen_lockfile: bool,
}

impl PackageManager {
    /// Jobs of the `install` command.
    pub async fn install(&self, args: &InstallArgs) -> Result<(), PackageManagerError> {
        let PackageManager { config, http_client, tarball_cache, lockfile, package_json } = self;
        let InstallArgs { dependency_options, frozen_lockfile } = args;

        Install {
            tarball_cache,
            http_client,
            config,
            package_json,
            lockfile: lockfile.as_ref(),
            dependency_groups: dependency_options.dependency_groups(),
            frozen_lockfile: *frozen_lockfile,
        }
        .run()
        .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_package_json::DependencyGroup;
    use pretty_assertions::assert_eq;

    #[test]
    fn install_args_to_dependency_groups() {
        use DependencyGroup::{Default, Dev, Optional};
        let create_list =
            |opts: CliInstallDependencyOptions| opts.dependency_groups().collect::<Vec<_>>();

        // no flags -> prod + dev + optional
        assert_eq!(
            create_list(CliInstallDependencyOptions {
                prod: false,
                dev: false,
                no_optional: false
            }),
            [Default, Dev, Optional],
        );

        // --prod -> prod + optional
        assert_eq!(
            create_list(CliInstallDependencyOptions { prod: true, dev: false, no_optional: false }),
            [Default, Optional],
        );

        // --dev -> dev + optional
        assert_eq!(
            create_list(CliInstallDependencyOptions { prod: false, dev: true, no_optional: false }),
            [Dev, Optional],
        );

        // --no-optional -> prod + dev
        assert_eq!(
            create_list(CliInstallDependencyOptions { prod: false, dev: false, no_optional: true }),
            [Default, Dev],
        );

        // --prod --no-optional -> prod
        assert_eq!(
            create_list(CliInstallDependencyOptions { prod: true, dev: false, no_optional: true }),
            [Default],
        );

        // --dev --no-optional -> dev
        assert_eq!(
            create_list(CliInstallDependencyOptions { prod: false, dev: true, no_optional: true }),
            [Dev],
        );

        // --prod --dev -> prod + dev + optional
        assert_eq!(
            create_list(CliInstallDependencyOptions { prod: true, dev: true, no_optional: false }),
            [Default, Dev, Optional],
        );

        // --prod --dev --no-optional -> prod + dev
        assert_eq!(
            create_list(CliInstallDependencyOptions { prod: true, dev: true, no_optional: true }),
            [Default, Dev],
        );
    }
}
