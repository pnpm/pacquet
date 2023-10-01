use crate::fixtures::{CLEAN_INSTALL_SCRIPT, FROZEN_LOCKFILE_SCRIPT, LOCKFILE};
use clap::{Args, Parser, ValueEnum};
use std::{path::PathBuf, process::Command};

#[derive(Debug, Parser)]
pub struct CliArgs {
    /// Task to benchmark.
    #[clap(long, short = 't')]
    pub task: BenchmarkTask,

    /// URL to the local virtual registry.
    #[clap(long, short, default_value = "http://localhost:4873")]
    pub registry: String,

    /// Path to the git repository of pacquet.
    #[clap(long, short = 'R', default_value = ".")]
    pub repository: PathBuf,

    /// Override default `package.json`.
    #[clap(long, short)]
    pub package_json: Option<PathBuf>,

    /// Flags to pass to `hyperfine`.
    #[clap(flatten)]
    pub hyperfine_options: HyperfineOptions,

    /// Path to the work environment.
    #[clap(long, short, default_value = "bench-work-env")]
    pub work_env: PathBuf,

    /// Benchmark against pnpm.
    #[clap(long)]
    pub with_pnpm: bool,

    /// Branch name, tag name, or commit id of the pacquet repo.
    #[clap(required = true)]
    pub revisions: Vec<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BenchmarkTask {
    /// Benchmark clean install without lockfile and without local cache.
    CleanInstall,
    /// Benchmark install with a frozen lockfile and without local cache.
    FrozenLockfile,
}

impl BenchmarkTask {
    /// Infer content corresponding content of the install script.
    pub fn install_script_content(self) -> &'static str {
        match self {
            BenchmarkTask::CleanInstall => CLEAN_INSTALL_SCRIPT,
            BenchmarkTask::FrozenLockfile => FROZEN_LOCKFILE_SCRIPT,
        }
    }

    /// Return `lockfile=true` or `lockfile=false` for use in generating `.npmrc`.
    pub fn npmrc_lockfile_setting(self) -> &'static str {
        match self {
            BenchmarkTask::CleanInstall => "lockfile=false",
            BenchmarkTask::FrozenLockfile => "lockfile=true",
        }
    }

    /// Whether to use a lockfile.
    pub fn lockfile(self) -> Option<&'static str> {
        match self {
            BenchmarkTask::CleanInstall => None,
            BenchmarkTask::FrozenLockfile => Some(LOCKFILE),
        }
    }
}

#[derive(Debug, Args)]
pub struct HyperfineOptions {
    /// Number of warmup runs to perform before the actual measured benchmark.
    #[clap(long, default_value_t = 1)]
    pub warmup: u8,

    /// Minimum number of runs for each command.
    #[clap(long)]
    pub min_runs: Option<u8>,

    /// Maximum number of runs for each command.
    #[clap(long)]
    pub max_runs: Option<u8>,

    /// Exact number of runs for each command.
    #[clap(long)]
    pub runs: Option<u8>,

    /// Ignore non-zero exit codes of the benchmarked program.
    #[clap(long)]
    pub ignore_failure: bool,
}

impl HyperfineOptions {
    pub fn append_to(&self, hyperfine_command: &mut Command) {
        let &HyperfineOptions { warmup, min_runs, max_runs, runs, ignore_failure } = self;
        hyperfine_command.arg("--warmup").arg(warmup.to_string());
        if let Some(min_runs) = min_runs {
            hyperfine_command.arg("--min-runs").arg(min_runs.to_string());
        }
        if let Some(max_runs) = max_runs {
            hyperfine_command.arg("--max-runs").arg(max_runs.to_string());
        }
        if let Some(runs) = runs {
            hyperfine_command.arg("--runs").arg(runs.to_string());
        }
        if ignore_failure {
            hyperfine_command.arg("--ignore-failures");
        }
    }
}
