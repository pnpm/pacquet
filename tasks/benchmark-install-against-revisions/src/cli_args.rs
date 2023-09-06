use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct CliArgs {
    /// URL to the local virtual registry.
    #[clap(long, short, default_value = "http://localhost:4873")]
    pub registry: String,

    /// Path to the git repository of pacquet.
    #[clap(long, short = 'R', default_value = ".")]
    pub repository: PathBuf,

    /// Override default `package.json`.
    #[clap(long, short)]
    pub package_json: Option<PathBuf>,

    /// Path to the work environment.
    #[clap(long, short, default_value = "tmp")]
    pub work_env: PathBuf,

    /// Branch name, tag name, or commit id of the pacquet repo.
    #[clap(required = true)]
    pub revisions: Vec<String>,
}
