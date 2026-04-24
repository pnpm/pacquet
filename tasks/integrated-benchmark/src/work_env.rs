use crate::{
    cli_args::{BenchmarkScenario, HyperfineOptions},
    fixtures::{LOCKFILE, PACKAGE_JSON, PNPM_WORKSPACE},
    verify::executor,
};
use itertools::Itertools;
use os_display::Quotable;
use pacquet_fs::file_mode::make_file_executable;
use pipe_trait::Pipe;
use std::{
    borrow::Cow,
    fmt,
    fs::{self, File},
    io::Write,
    iter,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug)]
pub struct WorkEnv {
    pub root: PathBuf,
    pub with_pnpm: bool,
    pub revisions: Vec<String>,
    pub registry: String,
    pub repository: PathBuf,
    pub scenario: BenchmarkScenario,
    pub hyperfine_options: HyperfineOptions,
    pub fixture_dir: Option<PathBuf>,
}

impl WorkEnv {
    const INIT_PROXY_CACHE: BenchId<'static> = BenchId::Static(".init-proxy-cache");
    const PNPM: BenchId<'static> = BenchId::Static("pnpm");

    fn root(&self) -> &'_ Path {
        &self.root
    }

    fn revision_names(&self) -> impl Iterator<Item = &'_ str> + '_ {
        self.revisions.iter().map(AsRef::as_ref)
    }

    fn revision_ids(&self) -> impl Iterator<Item = BenchId<'_>> + '_ {
        self.revision_names().map(BenchId::PacquetRevision)
    }

    fn registry(&self) -> &'_ str {
        &self.registry
    }

    fn repository(&self) -> &'_ Path {
        &self.repository
    }

    fn bench_dir(&self, id: BenchId) -> PathBuf {
        self.root().join(id.to_string())
    }

    fn script_path(&self, id: BenchId) -> PathBuf {
        self.bench_dir(id).join("install.bash")
    }

    fn bash_command(&self, id: BenchId) -> String {
        let script_path = self.script_path(id);
        let script_path = script_path.to_str().expect("convert script path to UTF-8");
        format!("bash {script_path}")
    }

    fn revision_repo(&self, revision: &str) -> PathBuf {
        self.bench_dir(BenchId::PacquetRevision(revision)).join("pacquet")
    }

    fn resolve_revision(&self, revision: &str) -> String {
        let output = Command::new("git")
            .current_dir(self.repository())
            .arg("rev-parse")
            .arg(revision)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .expect("git rev-parse");
        assert!(output.status.success());
        output
            .stdout
            .pipe(String::from_utf8)
            .expect("output of rev-parse is valid UTF-8")
            .trim()
            .to_string()
    }

    fn init(&self) {
        eprintln!("Initializing...");
        let id_list = self
            .revision_ids()
            .chain(iter::once(WorkEnv::INIT_PROXY_CACHE))
            .chain(self.with_pnpm.then_some(WorkEnv::PNPM));
        for id in id_list {
            eprintln!("ID: {id}");
            let dir = self.bench_dir(id);
            let for_pnpm = matches!(id, BenchId::Static(_));
            fs::create_dir_all(&dir).expect("create directory for the revision");
            create_package_json(&dir, self.fixture_dir.as_deref());
            create_pnpm_workspace(&dir, self.fixture_dir.as_deref());
            create_install_script(&dir, self.scenario, for_pnpm);
            create_npmrc(&dir, self.registry(), self.scenario);
            may_create_lockfile(&dir, self.scenario, self.fixture_dir.as_deref());
        }

        eprintln!("Populating proxy registry cache...");
        Command::new("bash")
            .arg(self.script_path(WorkEnv::INIT_PROXY_CACHE))
            .pipe_mut(executor("install.bash"))
    }

    fn build(&self) {
        eprintln!("Building...");
        for revision in self.revision_names() {
            eprintln!("Revision: {revision:?}");

            let repository = self.repository();
            let revision_repo = self.revision_repo(revision);
            if revision_repo.exists() {
                if !revision_repo.join(".git").exists() {
                    eprintln!("Initializing a git repository at {revision_repo:?}...");
                    Command::new("git")
                        .current_dir(&revision_repo)
                        .arg("init")
                        .arg(&revision_repo)
                        .arg("--initial-branch=__blank__")
                        .pipe(executor("git init"));
                }

                eprintln!("Fetching from {repository:?}...");
                Command::new("git")
                    .current_dir(&revision_repo)
                    .arg("fetch")
                    .arg(repository)
                    .pipe(executor("git fetch"));
            } else {
                eprintln!("Cloning {repository:?} to {revision_repo:?}...");
                Command::new("git")
                    .arg("clone")
                    .arg("--no-checkout")
                    .arg(repository)
                    .arg(&revision_repo)
                    .pipe(executor("git clone"));
            }

            let commit = self.resolve_revision(revision);
            eprintln!("Checking out {commit:?}...");
            Command::new("git")
                .current_dir(&revision_repo)
                .arg("checkout")
                .arg(commit)
                .pipe(executor("git checkout"));

            eprintln!("List of branches:");
            Command::new("git")
                .current_dir(&revision_repo)
                .arg("branch")
                .pipe(executor("git branch"));

            eprintln!("Building {revision:?}...");
            Command::new("cargo")
                .current_dir(&revision_repo)
                .arg("build")
                .arg("--release")
                .arg("--bin=pacquet")
                .pipe(executor("cargo build"));
        }
    }

    /// Shell command that wipes every measured bench dir's
    /// `node_modules` + `store-dir`. Hyperfine uses this as its
    /// `--prepare` so every timed iteration starts from a cold install,
    /// and `prewarm()` uses the same command to match.
    ///
    /// Covers every id we'll benchmark — pacquet revisions plus pnpm
    /// when `--with-pnpm` is set. Dropping pnpm from the cleanup once
    /// let it hit a no-op "already installed" code path on every run
    /// after warmup; keep them symmetric.
    fn cleanup_command(&self) -> String {
        let cleanup_targets = self
            .revision_ids()
            .chain(self.with_pnpm.then_some(WorkEnv::PNPM))
            .map(|id| self.bench_dir(id))
            .flat_map(|dir| [dir.join("node_modules"), dir.join("store-dir")])
            .map(|path| path.maybe_quote().to_string())
            .join(" ");
        format!("rm -rf {cleanup_targets}")
    }

    /// Run each benchmarked install once before hyperfine starts.
    ///
    /// Hyperfine runs its measured commands sequentially (no per-
    /// iteration interleaving) with only `--warmup 1` between them,
    /// which means the second-listed command's measured runs always
    /// start from a state already warmed by the first command's 10
    /// measured runs — OS page cache, verdaccio on-disk cache,
    /// registry-mock in-memory state, etc. That systematic bias
    /// makes the second-listed command look slightly faster in every
    /// PR (typically 1-3%) even when the code difference is zero.
    ///
    /// A dedicated prewarm pass in front of hyperfine puts every
    /// install path through the full fetch / decompress / CAFS-write
    /// cycle once, so by the time hyperfine's first `--warmup 1`
    /// fires, the shared state is already primed and the ordering
    /// advantage is gone.
    fn prewarm(&self) {
        eprintln!("Pre-warming before hyperfine...");
        let cleanup_command = self.cleanup_command();
        let ids: Vec<_> =
            self.revision_ids().chain(self.with_pnpm.then_some(WorkEnv::PNPM)).collect();
        for id in &ids {
            eprintln!("Pre-warm: {id}");
            Command::new("bash").arg("-c").arg(&cleanup_command).pipe_mut(executor("cleanup"));
            Command::new("bash").arg(self.script_path(*id)).pipe_mut(executor("prewarm install"));
        }
        // Final cleanup so hyperfine's first `--prepare` doesn't
        // surface any state we leaked.
        Command::new("bash").arg("-c").arg(&cleanup_command).pipe_mut(executor("cleanup"));
    }

    fn benchmark(&self) {
        let cleanup_command = self.cleanup_command();

        let mut command = Command::new("hyperfine");
        command.current_dir(self.root()).arg("--prepare").arg(&cleanup_command);

        self.hyperfine_options.append_to(&mut command);

        for id in self.revision_ids().chain(self.with_pnpm.then_some(WorkEnv::PNPM)) {
            command.arg("--command-name").arg(id.to_string()).arg(self.bash_command(id));
        }

        command
            .arg("--export-json")
            .arg(self.root().join("BENCHMARK_REPORT.json"))
            .arg("--export-markdown")
            .arg(self.root().join("BENCHMARK_REPORT.md"));

        executor("hyperfine")(&mut command);
    }

    pub fn run(&self) {
        self.init();
        self.build();
        self.prewarm();
        self.benchmark();
    }
}

fn create_package_json(dst_dir: &Path, src_dir: Option<&Path>) {
    let dst = dst_dir.join("package.json");
    if let Some(src_dir) = src_dir {
        let src = src_dir.join("package.json");
        assert!(src.is_file(), "{src:?} must be a file");
        assert_ne!(src, dst);
        fs::copy(src, dst).expect("copy package.json for the revision");
    } else {
        fs::write(dst, PACKAGE_JSON).expect("write package.json for the revision");
    }
}

/// Copy or write the fixture's `pnpm-workspace.yaml`. Pacquet's `.npmrc`
/// sets `ignore-scripts=true` so no scripts actually run, but pnpm still
/// warns about `ERR_PNPM_IGNORED_BUILDS` for packages whose postinstalls
/// would have fired — the workspace file's `allowBuilds: {core-js: false,
/// es5-ext: false}` silences those specific warnings and keeps pnpm's
/// output clean so hyperfine doesn't see stderr noise.
fn create_pnpm_workspace(dst_dir: &Path, src_dir: Option<&Path>) {
    let dst = dst_dir.join("pnpm-workspace.yaml");
    if let Some(src_dir) = src_dir {
        let src = src_dir.join("pnpm-workspace.yaml");
        // The workspace file is optional when a custom fixture directory
        // is provided — only copy if present, so existing fixture dirs
        // that predate this helper don't start erroring out.
        if src.is_file() {
            assert_ne!(src, dst);
            fs::copy(src, dst).expect("copy pnpm-workspace.yaml for the revision");
        }
    } else {
        fs::write(dst, PNPM_WORKSPACE).expect("write pnpm-workspace.yaml for the revision");
    }
}

fn create_npmrc(dir: &Path, registry: &str, scenario: BenchmarkScenario) {
    let path = dir.join(".npmrc");
    let store_dir = dir.join("store-dir");
    let store_dir = store_dir.to_str().expect("path to store-dir is valid UTF-8");
    eprintln!("Creating config file {path:?}...");
    let mut file = File::create(path).expect("create .npmrc");
    writeln!(file, "registry={registry}").unwrap();
    writeln!(file, "store-dir={store_dir}").unwrap();
    writeln!(file, "auto-install-peers=false").unwrap();
    writeln!(file, "ignore-scripts=true").unwrap();
    writeln!(file, "{}", scenario.npmrc_lockfile_setting()).unwrap();
}

fn may_create_lockfile(dst_dir: &Path, scenario: BenchmarkScenario, src_dir: Option<&Path>) {
    let load_lockfile = || -> Cow<'_, str> {
        let Some(src_dir) = src_dir else { return Cow::Borrowed(LOCKFILE) };
        src_dir
            .join("pnpm-lock.yaml")
            .pipe(fs::read_to_string)
            .expect("read fixture lockfile")
            .pipe(Cow::Owned)
    };
    if let Some(lockfile) = scenario.lockfile(load_lockfile) {
        let path = dst_dir.join("pnpm-lock.yaml");
        fs::write(path, lockfile).expect("write pnpm-lock.yaml for the revision");
    }
}

fn create_install_script(dir: &Path, scenario: BenchmarkScenario, for_pnpm: bool) {
    let path = dir.join("install.bash");

    eprintln!("Creating script {path:?}...");
    let mut file = File::create(&path).expect("create install.bash");

    writeln!(file, "#!/bin/bash").unwrap();
    writeln!(file, "set -o errexit -o nounset -o pipefail").unwrap();
    writeln!(file, r#"cd "$(dirname "$0")""#).unwrap();

    let command = if for_pnpm { "pnpm" } else { "./pacquet/target/release/pacquet" };
    write!(file, "exec {command} install").unwrap();
    for arg in scenario.install_args() {
        write!(file, " {arg}").unwrap();
    }
    writeln!(file).unwrap();

    make_file_executable(&file).expect("make the script executable");
}

#[derive(Debug, Clone, Copy)]
enum BenchId<'a> {
    PacquetRevision(&'a str),
    Static(&'a str),
}

impl<'a> fmt::Display for BenchId<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BenchId::PacquetRevision(revision) => write!(f, "pacquet@{revision}"),
            BenchId::Static(name) => write!(f, "{name}"),
        }
    }
}
