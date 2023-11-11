extern crate swc_malloc;

mod cli_args;
mod state;

use clap::Parser;
use cli_args::CliArgs;
use miette::set_panic_hook;
use pacquet_diagnostics::enable_tracing_by_env;
use state::State;

pub async fn main() -> miette::Result<()> {
    // We use rayon only for blocking syscalls, so we multiply the number of threads by 3.
    rayon::ThreadPoolBuilder::new()
        .num_threads(num_cpus::get() * 3)
        .build_global()
        .expect("build rayon thread pool");

    enable_tracing_by_env();
    set_panic_hook();
    CliArgs::parse().run().await
}
