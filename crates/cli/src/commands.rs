use clap::{Parser, Subcommand};
use pacquet_package_json::DependencyGroup;

/// Experimental package manager for node.js written in rust.
#[derive(Parser, Debug)]
#[command(name = "pacquet")]
#[command(bin_name = "pacquet")]
#[command(version = "0.1.0")]
#[command(about = "Experimental package manager for node.js")]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Subcommands,
}

#[derive(Subcommand, Debug)]
pub enum Subcommands {
    /// Initialize a package.json
    Init,
    /// Add a package
    Add(AddArgs),
    /// Runs a package's "test" script, if one was provided.
    Test,
    /// Runs a defined package script.
    #[clap(name = "run")]
    RunScript(RunScriptArgs),
}

#[derive(Parser, Debug)]
pub struct AddArgs {
    /// Name of the package
    pub package: String,
    /// Install the specified packages as regular dependencies.
    #[arg(short = 'P', long = "save-prod", group = "dependency_group")]
    save_prod: bool,
    /// Install the specified packages as devDependencies.
    #[arg(short = 'D', long = "save-dev", group = "dependency_group")]
    save_dev: bool,
    /// Install the specified packages as optionalDependencies.
    #[arg(short = 'O', long = "save-optional", group = "dependency_group")]
    save_optional: bool,
    /// Saved dependencies will be configured with an exact version rather than using
    /// pacquet's default semver range operator.
    #[arg(short = 'E', long = "save-exact")]
    pub save_exact: bool,
    /// The directory with links to the store (default is node_modules/.pacquet).
    /// All direct and indirect dependencies of the project are linked into this directory
    #[arg(long = "virtual-store-dir", default_value = "node_modules/.pacquet")]
    pub virtual_store_dir: String,
}

impl AddArgs {
    pub fn get_dependency_group(&self) -> DependencyGroup {
        if self.save_dev {
            DependencyGroup::Dev
        } else if self.save_optional {
            DependencyGroup::Optional
        } else {
            DependencyGroup::Default
        }
    }
}

#[derive(Parser, Debug)]
pub struct RunScriptArgs {
    /// A pre-defined package script.
    pub command: String,
}
