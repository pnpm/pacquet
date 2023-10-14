mod cli_args;
mod state;

use clap::Parser;
use cli_args::CliArgs;
use miette::set_panic_hook;
use pacquet_diagnostics::enable_tracing_by_env;

pub async fn main() -> miette::Result<()> {
    enable_tracing_by_env();
    set_panic_hook();
    CliArgs::parse().run().await
}
