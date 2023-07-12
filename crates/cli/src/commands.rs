use clap::{Parser, Subcommand};

/// Experimental package manager for node.js written in rust.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Subcommands,
}

#[derive(Subcommand, Debug)]
pub enum Subcommands {
    Init,
    Add(AddArgs),
}

#[derive(Parser, Debug)]
/// Add a package
pub struct AddArgs {
    /// Name of the package
    #[arg(short, long)]
    pub package: String,
}
