use crate::{
    cli_args::{BenchmarkScenario, HyperfineOptions},
    fixtures::PACKAGE_JSON,
};
use itertools::Itertools;
use os_display::Quotable;
use pipe_trait::Pipe;
use std::{
    fs::{self, File, Permissions},
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
    pub package_json: Option<PathBuf>,
}

impl WorkEnv {
    const INIT_PROXY_CACHE: &str = ".init-proxy-cache";
    const PNPM: &str = "PNPM";

    fn root(&self) -> &'_ Path {
        &self.root
    }

    fn revisions(&self) -> impl Iterator<Item = &'_ str> + '_ {
        self.revisions.iter().map(AsRef::as_ref)
    }

    fn registry(&self) -> &'_ str {
        &self.registry
    }

    fn repository(&self) -> &'_ Path {
        &self.repository
    }

    fn revision_root(&self, revision: &str) -> PathBuf {
        self.root().join(revision)
    }

    fn revision_install_script(&self, revision: &str) -> PathBuf {
        self.revision_root(revision).join("install.bash")
    }

    fn revision_repo(&self, revision: &str) -> PathBuf {
        self.revision_root(revision).join("pacquet")
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
        let entries = self
            .revisions()
            .map(|revision| (revision, false))
            .chain(iter::once((WorkEnv::INIT_PROXY_CACHE, true)))
            .chain(self.with_pnpm.then_some((WorkEnv::PNPM, true)));
        for (revision, for_pnpm) in entries {
            eprintln!("Revision: {revision:?}");
            let dir = self.revision_root(revision);
            fs::create_dir_all(&dir).expect("create directory for the revision");
            create_package_json(&dir, self.package_json.as_deref());
            create_install_script(&dir, self.scenario, for_pnpm);
            create_npmrc(&dir, self.registry(), self.scenario);
            may_create_lockfile(&dir, self.scenario);
        }

        eprintln!("Populating proxy registry cache...");
        self.revision_install_script(WorkEnv::INIT_PROXY_CACHE)
            .pipe(Command::new)
            .pipe_mut(executor("install.bash"))
    }

    fn build(&self) {
        eprintln!("Building...");
        for revision in self.revisions() {
            eprintln!("Revision: {revision:?}");

            let repository = self.repository();
            let revision_repo = self.revision_repo(revision);
            if revision_repo.exists() {
                eprintln!("Updating {revision_repo:?} to upstream...");
                Command::new("git")
                    .current_dir(&revision_repo)
                    .arg("fetch")
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

    fn benchmark(&self) {
        let cleanup_targets = self
            .revisions()
            .map(|revision| self.revision_root(revision))
            .flat_map(|revision| [revision.join("node_modules"), revision.join("store-dir")])
            .map(|path| path.maybe_quote().to_string())
            .join(" ");
        let cleanup_command = format!("rm -rf {cleanup_targets}");

        let mut command = Command::new("hyperfine");
        command.current_dir(self.root()).arg("--prepare").arg(&cleanup_command);

        self.hyperfine_options.append_to(&mut command);

        for revision in self.revisions().chain(self.with_pnpm.then_some(WorkEnv::PNPM)) {
            command.arg("--command-name").arg(revision).arg(self.revision_install_script(revision));
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
        self.benchmark();
    }
}

fn create_package_json(dir: &Path, src: Option<&Path>) {
    let dst = dir.join("package.json");
    if let Some(src) = src {
        assert!(src.is_file(), "{src:?} must be a file");
        assert_ne!(src, dst);
        fs::copy(src, dst).expect("copy package.json for the revision");
    } else {
        fs::write(dst, PACKAGE_JSON).expect("write package.json for the revision");
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

fn may_create_lockfile(dir: &Path, scenario: BenchmarkScenario) {
    if let Some(lockfile) = scenario.lockfile() {
        let path = dir.join("pnpm-lock.yaml");
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

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = Permissions::from_mode(0o777);
        fs::set_permissions(path, permissions).expect("make the script executable");
    }
}

fn executor<'a>(message: &'a str) -> impl FnOnce(&'a mut Command) {
    move |command| {
        let output = command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()
            .expect(message);
        assert!(output.status.success(), "Process exits with non-zero status: {message}");
    }
}