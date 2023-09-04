use crate::fixtures::{CLEANUP_SCRIPT, INSTALL_SCRIPT, PACKAGE_JSON};
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
    pub revisions: Vec<String>,
    pub registry: String,
    pub repository: PathBuf,
}

impl WorkEnv {
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

    fn revision_cleanup_script(&self, revision: &str) -> PathBuf {
        self.revision_root(revision).join("cleanup.bash")
    }

    fn revision_install_script(&self, revision: &str) -> PathBuf {
        self.revision_root(revision).join("install.bash")
    }

    fn revision_repo(&self, revision: &str) -> PathBuf {
        self.revision_root(revision).join("pacquet")
    }

    fn init(&self) {
        const INIT_PROXY_CACHE: &str = ".init-proxy-cache";

        eprintln!("Initializing...");
        for revision in self.revisions().chain(iter::once(INIT_PROXY_CACHE)) {
            eprintln!("Revision: {revision:?}");
            let dir = self.revision_root(revision);
            fs::create_dir_all(&dir).expect("create directory for the revision");
            fs::write(dir.join("package.json"), PACKAGE_JSON)
                .expect("create package.json for the revision");
            create_script(&self.revision_cleanup_script(revision), CLEANUP_SCRIPT);
            create_script(&self.revision_install_script(revision), INSTALL_SCRIPT);
            create_npmrc(&dir, self.registry());
        }

        eprintln!("Populating proxy registry cache...");
        Command::new("pnpm")
            .current_dir(self.revision_root(INIT_PROXY_CACHE))
            .arg("install")
            .pipe(executor("pnpm install"));
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
                    .arg("origin")
                    .arg(revision)
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

            eprintln!("Checking out {revision:?}...");
            Command::new("git")
                .current_dir(&revision_repo)
                .arg("checkout")
                .arg(revision)
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
        command.current_dir(self.root()).arg("--warmup=1").arg("--prepare").arg(&cleanup_command);

        for revision in self.revisions() {
            command.arg("--command-name").arg(revision).arg(self.revision_install_script(revision));
        }

        executor("hyperfine")(&mut command);
    }

    pub fn run(&self) {
        self.init();
        self.build();
        self.benchmark();
    }
}

fn create_npmrc(dir: &Path, registry: &str) {
    let path = dir.join(".npmrc");
    let store_dir = dir.join("store-dir");
    let store_dir = store_dir.to_str().expect("path to store-dir is valid UTF-8");
    let mut file = File::create(path).expect("create .npmrc");
    writeln!(file, "registry={registry}").unwrap();
    writeln!(file, "store-dir={store_dir}").unwrap();
    writeln!(file, "auto-install-peers=false").unwrap();
    writeln!(file, "lockfile=false").unwrap();
}

fn create_script(path: &Path, content: &str) {
    eprintln!("Creating script {path:?}...");
    fs::write(path, content).expect("write content to script");
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
