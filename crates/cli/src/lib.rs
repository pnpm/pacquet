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
    configure_rayon_pool();
    CliArgs::parse().run().await
}

/// Size rayon's global pool at `2 × available_parallelism`. The link
/// phase is dominated by clonefile / hardlink syscalls that block the
/// calling thread on the kernel's metadata journal, not by CPU work,
/// so oversubscribing CPUs gives more in-flight syscalls and a higher
/// effective throughput. Empirically sweeping 4-200 threads on a
/// 1352-package warm install on macOS APFS, 2× was the knee — fewer
/// threads underutilize the journal, way more (100+) loses to context
/// switching and per-thread fixed costs (`user` time scales linearly
/// past 50 without any wall-time payoff).
///
/// Use [`std::thread::available_parallelism`] rather than the
/// workspace's existing `num_cpus::get()` so cgroup / CPU-quota
/// limits in containers and CI runners are respected — `num_cpus`
/// reports the host's logical CPU count, which on a quota-limited
/// runner can spin up far more rayon threads than the kernel will
/// actually schedule onto our cores (Copilot review on #292).
///
/// Honours an explicit `RAYON_NUM_THREADS` env var by skipping our
/// override (rayon's `build_global` errors if a pool is already set,
/// but env vars don't pre-init it — so we just apply a smaller
/// override only when nothing else has been configured). Best-effort:
/// if another part of the binary already initialised the pool, leave
/// it alone.
fn configure_rayon_pool() {
    if std::env::var_os("RAYON_NUM_THREADS").is_some() {
        return;
    }
    let n = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .saturating_mul(2)
        .max(4);
    let _ = rayon::ThreadPoolBuilder::new().num_threads(n).build_global();
}
