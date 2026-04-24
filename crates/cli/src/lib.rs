// Swap the default system allocator for `swc_malloc`, which pulls
// in mimalloc on macOS / Windows and jemalloc on Linux. A package
// manager fan-outs thousands of short-lived `Vec<u8>` / `String` /
// `HashMap` allocations per install (tar entry buffers, CAFS
// paths, snapshot IDs, …); the system allocators on macOS and
// glibc are noticeably slower than mimalloc / jemalloc on that
// workload. Activating the crate via `extern crate` is enough —
// `swc_malloc` embeds the `#[global_allocator]` declaration
// itself and picks the per-target backend at compile time.
extern crate swc_malloc;

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
