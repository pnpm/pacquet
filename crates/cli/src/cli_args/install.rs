use crate::State;
use crate::cli_args::supported_architectures::SupportedArchitecturesArgs;
use clap::Args;
use miette::Context;
use pacquet_package_manager::Install;
use pacquet_package_manifest::DependencyGroup;
use pacquet_reporter::Reporter;

#[derive(Debug, Args)]
pub struct InstallDependencyOptions {
    /// pacquet will not install any package listed in devDependencies and will remove those insofar
    /// they were already installed, if the NODE_ENV environment variable is set to production.
    /// Use this flag to instruct pacquet to ignore NODE_ENV and take its production status from this
    /// flag instead.
    #[arg(short = 'P', long)]
    prod: bool,
    /// Only devDependencies are installed and dependencies are removed insofar they were
    /// already installed, regardless of the NODE_ENV.
    #[arg(short = 'D', long)]
    dev: bool,
    /// optionalDependencies are not installed.
    #[arg(long)]
    no_optional: bool,
}

impl InstallDependencyOptions {
    /// Convert the dependency options to an iterator of [`DependencyGroup`]
    /// which filters the types of dependencies to install.
    fn dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        let &InstallDependencyOptions { prod, dev, no_optional } = self;
        let has_both = prod == dev;
        let has_prod = has_both || prod;
        let has_dev = has_both || dev;
        let has_optional = !no_optional;
        std::iter::empty()
            .chain(has_prod.then_some(DependencyGroup::Prod))
            .chain(has_dev.then_some(DependencyGroup::Dev))
            .chain(has_optional.then_some(DependencyGroup::Optional))
    }
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    pub dependency_options: InstallDependencyOptions,

    /// `--cpu` / `--os` / `--libc` overrides for the optional-dep
    /// platform filter. Mirrors upstream pnpm's CLI flags; merges
    /// per-axis into `supportedArchitectures` loaded from
    /// `pnpm-workspace.yaml`.
    #[clap(flatten)]
    pub supported_architectures: SupportedArchitecturesArgs,

    /// Don't generate a lockfile and fail if the lockfile is outdated.
    #[clap(long)]
    pub frozen_lockfile: bool,
}

impl InstallArgs {
    pub async fn run<R: Reporter>(self, state: State) -> miette::Result<()> {
        let State { tarball_mem_cache, http_client, config, manifest, lockfile, resolved_packages } =
            &state;
        let InstallArgs { dependency_options, supported_architectures, frozen_lockfile } = self;

        // Merge CLI overrides with the yaml-derived value before
        // handing off to the install pipeline. `state.config` is a
        // shared `&'static Config`, so we compute the effective
        // `SupportedArchitectures` from a clone instead of mutating
        // in place; the install path takes the merged value as an
        // explicit parameter.
        let supported_architectures =
            supported_architectures.apply_to(config.supported_architectures.clone());

        Install {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile: lockfile.as_ref(),
            dependency_groups: dependency_options.dependency_groups(),
            frozen_lockfile,
            resolved_packages,
            supported_architectures,
        }
        .run::<R>()
        .await
        .wrap_err("installing dependencies")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests;
