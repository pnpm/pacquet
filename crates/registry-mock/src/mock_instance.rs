use crate::{kill_verdaccio::kill_all_verdaccio_children, node_registry_mock};
use advisory_lock::{AdvisoryFileLock, FileLockError, FileLockMode};
use assert_cmd::prelude::*;
use pipe_trait::Pipe;
use portpicker::pick_unused_port;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    env::temp_dir,
    fmt::Display,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
};
use sysinfo::{Pid, PidExt, Signal};
use tokio::{
    runtime::Builder,
    time::{sleep, Duration},
};

fn port_to_url(port: impl Display) -> String {
    format!("http://localhost:{port}/")
}

#[derive(Debug)]
pub struct MockInstance {
    process: Child,
}

impl Drop for MockInstance {
    fn drop(&mut self) {
        let MockInstance { process, .. } = self;
        let pid = process.id();
        eprintln!("info: Terminating all verdaccio instances below {pid}...");
        let kill_count = kill_all_verdaccio_children(Pid::from_u32(pid), Signal::Interrupt);
        eprintln!("info: Terminated {kill_count} verdaccio instances");
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MockInstanceOptions<'a> {
    pub client: &'a Client,
    pub port: &'a str,
    pub stdout: Option<&'a Path>,
    pub stderr: Option<&'a Path>,
    pub max_retries: usize,
    pub retry_delay: Duration,
}

impl<'a> MockInstanceOptions<'a> {
    async fn is_registry_ready(self) -> bool {
        let MockInstanceOptions { client, port, .. } = self;
        let url = port_to_url(port);

        let Err(error) = client.head(url).send().await else {
            return true;
        };

        if error.is_connect() {
            eprintln!("info: {error}");
            return false;
        }

        panic!("{error}");
    }

    async fn wait_for_registry(self) {
        let MockInstanceOptions { max_retries, retry_delay, .. } = self;
        let mut retries = max_retries;

        while !self.is_registry_ready().await {
            retries = retries.checked_sub(1).unwrap_or_else(|| {
                panic!("Failed to check for the registry for {max_retries} times")
            });

            sleep(retry_delay).await;
        }
    }

    async fn spawn(self) -> MockInstance {
        let MockInstanceOptions { port, stdout, stderr, .. } = self;

        eprintln!("Preparing...");
        node_registry_mock()
            .pipe(Command::new)
            .arg("prepare")
            .env("PNPM_REGISTRY_MOCK_PORT", port)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .assert()
            .success();

        let stdout = stdout.map_or_else(Stdio::null, |stdout| {
            File::create(stdout).expect("create file for stdout").into()
        });
        let stderr = stderr.map_or_else(Stdio::null, |stderr| {
            File::create(stderr).expect("create file for stderr").into()
        });
        let process = node_registry_mock()
            .pipe(Command::new)
            .env("PNPM_REGISTRY_MOCK_PORT", port)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("spawn mocked registry");

        self.wait_for_registry().await;

        MockInstance { process }
    }

    pub async fn spawn_if_necessary(self) -> Option<MockInstance> {
        let MockInstanceOptions { port, .. } = self;
        if self.is_registry_ready().await {
            eprintln!("info: {port} is already available");
            None
        } else {
            eprintln!("info: spawning mocked registry...");
            self.spawn().await.pipe(Some)
        }
    }
}

#[derive(Debug)]
#[must_use]
pub struct AutoMockInstance {
    anchor: RegistryAnchor,
}

impl AutoMockInstance {
    pub fn load_or_init() -> Self {
        let anchor = RegistryAnchor::load_or_init(|| {
            let port = pick_unused_port().expect("pick an unused port");
            let port_str = port.to_string();

            let mock_instance = Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime")
                .block_on({
                    MockInstanceOptions {
                        client: &Client::new(),
                        port: &port_str,
                        stdout: None,
                        stderr: None,
                        max_retries: 5,
                        retry_delay: Duration::from_millis(500),
                    }
                    .spawn()
                })
                .pipe(Box::new)
                .pipe(Box::leak);

            let listen = port_to_url(port);
            let pid = mock_instance.process.id();

            RegistryInfo { port, listen, pid }
        });

        AutoMockInstance { anchor }
    }

    pub fn listen(&self) -> &'_ str {
        &self.anchor.info.listen
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RegistryInfo {
    port: u16,
    listen: String,
    pid: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct RegistryAnchor {
    ref_count: u32,
    info: RegistryInfo,
}

impl Drop for RegistryAnchor {
    fn drop(&mut self) {
        // information from self is outdated, do not use it.

        let guard = GuardFile::lock();

        // load an up-to-date anchor, it is leaked to prevent dropping (again).
        let anchor = RegistryAnchor::load().pipe(Box::new).pipe(Box::leak);

        if let Some(ref_count) = anchor.ref_count.checked_sub(1) {
            anchor.ref_count = ref_count;
            anchor.save();
            if ref_count > 0 {
                eprintln!("info: The mocked server is still used by {ref_count} users. Skip.");
                return;
            }
        }

        eprintln!("info: There are no more users that use the mocked server");

        fn kill(pid: u32) {
            eprintln!("info: Terminating all verdaccio instances below {pid}...");
            let kill_count = kill_all_verdaccio_children(Pid::from_u32(pid), Signal::Interrupt);
            eprintln!("info: Terminated {kill_count} verdaccio instances");
        }

        let latest_pid = anchor.info.pid;
        let current_pid = self.info.pid;
        kill(latest_pid);

        if latest_pid != current_pid {
            eprintln!("info: Left-over detected (pid = {current_pid})");
            kill(current_pid);
        }

        RegistryAnchor::delete();
        guard.unlock();
    }
}

impl RegistryAnchor {
    fn path() -> &'static Path {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| temp_dir().join("pacquet-registry-mock-anchor.json"))
    }

    fn load() -> Self {
        RegistryAnchor::path()
            .pipe(fs::read_to_string)
            .expect("read the anchor")
            .pipe_as_ref(serde_json::from_str)
            .expect("parse anchor")
    }

    fn save(&self) {
        let text = serde_json::to_string_pretty(self).expect("convert anchor to JSON");
        fs::write(RegistryAnchor::path(), text).expect("write to anchor");
    }

    fn load_or_init<Init>(init: Init) -> Self
    where
        Init: FnOnce() -> RegistryInfo,
    {
        if let Some(guard) = GuardFile::try_lock() {
            let info = init();
            let anchor = RegistryAnchor { ref_count: 1, info };
            anchor.save();
            guard.unlock();
            anchor
        } else {
            let guard = GuardFile::lock();
            let mut anchor = RegistryAnchor::load();
            anchor.ref_count = anchor.ref_count.checked_add(1).expect("increment ref_count");
            anchor.save();
            guard.unlock();
            anchor
        }
    }

    fn delete() {
        fs::remove_file(RegistryAnchor::path()).expect("delete the anchor file");
    }
}

#[must_use]
struct GuardFile;

impl Drop for GuardFile {
    fn drop(&mut self) {
        GuardFile::path().unlock().expect("release file guard");
    }
}

impl GuardFile {
    fn path() -> &'static File {
        static PATH: OnceLock<File> = OnceLock::new();
        PATH.get_or_init(|| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(temp_dir().join("pacquet-registry-mock-anchor.lock"))
                .expect("open the guard file")
        })
    }

    fn lock() -> Self {
        GuardFile::path().lock(FileLockMode::Exclusive).expect("acquire file guard");
        GuardFile
    }

    fn try_lock() -> Option<Self> {
        match GuardFile::path().try_lock(FileLockMode::Exclusive) {
            Ok(()) => Some(GuardFile),
            Err(FileLockError::AlreadyLocked) => None,
            Err(FileLockError::Io(error)) => panic!("Failed to acquire the file guard: {error}"),
        }
    }

    fn unlock(self) {
        drop(self)
    }
}
