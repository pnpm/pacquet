use crate::{node_registry_mock, registry_mock};
use assert_cmd::prelude::*;
use derive_more::Deref;
use pipe_trait::Pipe;
use reqwest::Client;
use std::{
    env::temp_dir,
    fmt::Display,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
};
use tokio::time::{sleep, Duration};
use which::which;

fn port_to_url(port: impl Display) -> String {
    format!("http://127.0.0.1:{port}")
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

#[derive(Debug, Deref)]
pub struct AutoMockInstance {
    #[deref]
    mock_instance: MockInstance,
    #[deref(ignore)]
    port_lock_file: PathBuf,
    #[deref(ignore)]
    listen: String,
}

impl Drop for AutoMockInstance {
    fn drop(&mut self) {
        let AutoMockInstance { port_lock_file, .. } = &self;
        if let Err(error) = fs::remove_file(port_lock_file) {
            eprintln!("warning: Failed to remove port lock file at {port_lock_file:?}: {error}");
        }
    }
}

impl AutoMockInstance {
    const STARTING_PORT: u32 = 4873;

    async fn init() -> Self {
        let port_lock_dir = temp_dir().join("pacquet-registry-mock.lock");
        fs::create_dir_all(&port_lock_dir).expect("create port lock dir");

        for port in AutoMockInstance::STARTING_PORT.. {
            let port_str = port.to_string();
            let port_lock_file = port_lock_dir.join(&port_str);
            if port_lock_file.exists() {
                continue;
            }

            fs::write(&port_lock_file, format!("mocked registry for pacquet at port {port}"))
                .expect("create port lock file");

            let mock_instance = MockInstanceOptions {
                client: &Client::new(),
                port: &port_str,
                stdout: None,
                stderr: None,
                max_retries: 5,
                retry_delay: Duration::from_millis(500),
            }
            .spawn()
            .await;
            let listen = port_to_url(port);
            return AutoMockInstance { mock_instance, port_lock_file, listen };
        }

        panic!("Cannot find suitable port");
    }

    pub fn listen(&self) -> &'_ str {
        &self.listen
    }

    pub fn get_or_init() -> &'static Self {
        static SINGLE_INSTANCE: OnceLock<AutoMockInstance> = OnceLock::new();
        SINGLE_INSTANCE.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("build tokio runtime")
                .block_on(AutoMockInstance::init())
        })
    }
}
