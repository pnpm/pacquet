// Swap the default system allocator for `mimalloc`. A package
// manager fan-outs thousands of short-lived `Vec<u8>` / `String` /
// `HashMap` allocations per install (tar entry buffers, CAFS
// paths, snapshot IDs, …); mimalloc's per-thread free lists and
// small-object fast path are a better match for that shape than
// the default system allocator on macOS and glibc-Linux.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod cli_args;
mod state;

use clap::Parser;
use cli_args::CliArgs;
use miette::set_panic_hook;
use pacquet_diagnostics::enable_tracing_by_env;
use state::State;

pub async fn main() -> miette::Result<()> {
    enable_tracing_by_env();
    set_panic_hook();
    CliArgs::parse().run().await
}
