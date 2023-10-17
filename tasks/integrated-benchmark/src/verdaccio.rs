use crate::verify::ensure_program;
use pipe_trait::Pipe;
use reqwest::Client;
use std::{
    fs::File,
    path::Path,
    process::{Child, Command, Stdio},
};
use tokio::time::{sleep, Duration};

#[derive(Debug)]
pub struct Verdaccio {
    process: Child,
}

impl Drop for Verdaccio {
    fn drop(&mut self) {
        let Verdaccio { process } = self;
        let pid = process.id();

        eprintln!("info: Terminating verdaccio with the kill command (kill {pid})...");
        match Command::new("kill").arg(pid.to_string()).output() {
            Err(error) => {
                eprintln!("warn: Failed to terminate verdaccio with the kill command: {error}");
            }
            Ok(output) => {
                if output.status.success() {
                    eprintln!("info: Verdaccio terminated");
                    return;
                }

                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("warn: Failed to terminate verdaccio with the kill command: {stderr}");
            }
        }

        eprintln!("info: Terminating verdaccio with SIGKILL...");
        if let Err(error) = process.kill() {
            eprintln!("warn: Failed to terminate verdaccio with SIGKILL: {error}");
        } else {
            eprintln!("info: Verdaccio terminated");
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VerdaccioOptions<'a> {
    pub client: &'a Client,
    pub listen: &'a str,
    pub stdout: &'a Path,
    pub stderr: &'a Path,
    pub max_retries: usize,
    pub retry_delay: Duration,
}

impl<'a> VerdaccioOptions<'a> {
    async fn is_registry_ready(self) -> bool {
        let VerdaccioOptions { client, listen, .. } = self;

        let Err(error) = client.head(listen).send().await else {
            return true;
        };

        if error.is_connect() {
            eprintln!("info: {error}");
            return false;
        }

        panic!("{error}");
    }

    async fn wait_for_registry(self) {
        let VerdaccioOptions { max_retries, retry_delay, .. } = self;
        let mut retries = max_retries;

        while !self.is_registry_ready().await {
            retries = retries.checked_sub(1).unwrap_or_else(|| {
                panic!("Failed to check for the registry for {max_retries} times")
            });

            sleep(retry_delay).await;
        }
    }

    async fn spawn(self) -> Verdaccio {
        let VerdaccioOptions { listen, stdout, stderr, .. } = self;

        ensure_program("verdaccio");

        let process = Command::new("verdaccio")
            .arg("--listen")
            .arg(listen)
            .stdin(Stdio::null())
            .stdout(File::create(stdout).expect("create file for stdout"))
            .stderr(File::create(stderr).expect("create file for stderr"))
            .spawn()
            .expect("spawn verdaccio");

        self.wait_for_registry().await;

        Verdaccio { process }
    }

    pub async fn spawn_if_necessary(self) -> Option<Verdaccio> {
        let VerdaccioOptions { listen, .. } = self;
        if self.is_registry_ready().await {
            eprintln!("info: {listen} is already available");
            None
        } else {
            eprintln!("info: spawning verdaccio...");
            self.spawn().await.pipe(Some)
        }
    }
}
