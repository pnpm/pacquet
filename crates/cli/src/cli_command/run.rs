use clap::Args;

#[derive(Debug, Args)]
pub struct RunArgs {
    /// A pre-defined package script.
    pub command: String,

    /// Any additional arguments passed after the script name
    pub args: Vec<String>,

    /// You can use the --if-present flag to avoid exiting with a non-zero exit code when the
    /// script is undefined. This lets you run potentially undefined scripts without breaking the
    /// execution chain.
    #[arg(long = "if-present")]
    pub if_present: bool,
}
