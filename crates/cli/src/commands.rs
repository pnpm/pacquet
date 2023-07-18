use clap::{Parser, Subcommand};
use pacquet_package_json::DependencyGroup;

/// Experimental package manager for node.js written in rust.
#[derive(Parser, Debug)]
#[command(name = "pacquet")]
#[command(bin_name = "pacquet")]
#[command(version = "0.0.8")]
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
}

#[derive(Parser, Debug)]
pub struct AddArgs {
    /// Name of the package
    pub package: String,
    /// Add the package as a dev dependency
    #[arg(short = 'D', long = "save-dev", group = "dependency_group")]
    dev: bool,
    /// Add the package as a optional dependency
    #[arg(short = 'O', long = "save-optional", group = "dependency_group")]
    optional: bool,
    /// The directory with links to the store (default is node_modules/.pacquet).
    /// All direct and indirect dependencies of the project are linked into this directory
    #[arg(long = "virtual-store-dir", default_value = "node_modules/.pacquet")]
    pub virtual_store_dir: String,
}

impl AddArgs {
    pub fn get_dependency_group(&self) -> DependencyGroup {
        if self.dev {
            DependencyGroup::Dev
        } else if self.optional {
            DependencyGroup::Optional
        } else {
            DependencyGroup::Default
        }
    }
}
