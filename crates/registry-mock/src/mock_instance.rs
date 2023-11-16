use crate::{node_registry_mock, registry_mock};
use advisory_lock::{AdvisoryFileLock, FileLockMode};
use assert_cmd::prelude::*;
use pipe_trait::Pipe;
use portpicker::pick_unused_port;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    env::temp_dir,
    fmt::Display,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
};
use tokio::{
    runtime::Builder,
    time::{sleep, Duration},
};
use which::which;

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

        eprintln!("info: Terminating mocked registry with the kill command (kill {pid})...");
        match Command::new("kill").arg(pid.to_string()).output() {
            Err(error) => {
                eprintln!(
                    "warn: Failed to terminate mocked registry with the kill command: {error}"
                );
            }
            Ok(output) => {
                if output.status.success() {
                    eprintln!("info: Mocked registry terminated");
                    return;
                }

                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!(
                    "warn: Failed to terminate mocked registry with the kill command: {stderr}"
                );
            }
        }

        eprintln!("info: Terminating mocked registry with SIGKILL...");
        if let Err(error) = process.kill() {
            eprintln!("warn: Failed to terminate mocked registry with SIGKILL: {error}");
        } else {
            eprintln!("info: mocked registry terminated");
        }
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

        eprintln!("Installing pnpm packages...");
        which("pnpm")
            .expect("find pnpm command")
            .pipe(Command::new)
            .args(["install", "--frozen-lockfile", "--prefer-offline"])
            .current_dir(registry_mock())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .assert()
            .success();

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
    user_count: u32,
    info: RegistryInfo,
}

impl Drop for RegistryAnchor {
    fn drop(&mut self) {
        // information from self is outdated, do not use it.

        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(RegistryAnchor::path())
            .expect("open the anchor file");
        lock.lock(FileLockMode::Exclusive).expect("acquire anchor lock");

        // load an up-to-date anchor, it is leaked to prevent dropping (again).
        let anchor = RegistryAnchor::load(&mut lock)
            .expect("load an existing anchor")
            .pipe(Box::new)
            .pipe(Box::leak);
        assert_eq!(&self.info, &anchor.info);

        anchor.user_count = anchor.user_count.checked_sub(1).expect("decrement user_count");
        if anchor.user_count > 0 {
            anchor.save(&mut lock);
            eprintln!(
                "info: The mocked server is still used by {} users. Skip.",
                anchor.user_count
            );
            return;
        }

        let pid = anchor.info.pid;
        eprintln!("info: There are no more users that use the mocked server");
        eprintln!("info: Terminating mocked registry with the kill command (kill {pid})...");

        match Command::new("kill").arg(pid.to_string()).output() {
            Err(error) => {
                eprintln!(
                    "warn: Failed to terminate mocked registry with the kill command: {error}"
                );
            }
            Ok(output) => {
                if output.status.success() {
                    eprintln!("info: Mocked registry terminated");
                    return;
                }

                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!(
                    "warn: Failed to terminate mocked registry with the kill command: {stderr}"
                );
            }
        }

        RegistryAnchor::delete();
        drop(lock)
    }
}

impl RegistryAnchor {
    fn path() -> &'static Path {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| temp_dir().join("pacquet-registry-mock-anchor.json"))
    }

    fn load(lock: &mut File) -> Option<Self> {
        let mut text = String::new();
        match lock.read_to_string(&mut text) {
            Ok(_) if text.trim().is_empty() => None,
            Ok(_) => text
                .pipe_as_ref(serde_json::from_str::<RegistryAnchor>)
                .expect("parse anchor text")
                .pipe(Some),
            Err(error) => panic!("Failed to load anchor: {error}"),
        }
    }

    fn save(&self, lock: &mut File) {
        let text = serde_json::to_string_pretty(self).expect("convert anchor to JSON");
        lock.write_all(text.as_bytes()).expect("write to anchor");
    }

    fn load_or_init<Init>(init: Init) -> Self
    where
        Init: FnOnce() -> RegistryInfo,
    {
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(RegistryAnchor::path())
            .expect("open the anchor file");
        lock.lock(FileLockMode::Exclusive).expect("acquire anchor lock");

        if let Some(mut anchor) = RegistryAnchor::load(&mut lock) {
            anchor.user_count = anchor.user_count.checked_add(1).expect("increment user_count");
            anchor.save(&mut lock);
            return anchor;
        }
        let info = init();
        let anchor = RegistryAnchor { user_count: 1, info };
        anchor.save(&mut lock);
        drop(lock);

        anchor
    }

    fn delete() {
        fs::remove_file(RegistryAnchor::path()).expect("delete the anchor file");
    }
}
