use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_package_manifest::{PackageManifestError, safe_read_package_json_from_dir};
use pacquet_reporter::{
    LifecycleLog, LifecycleMessage, LifecycleStdio, LogEvent, LogLevel, Reporter,
};
use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
};

/// Error from running lifecycle scripts.
///
/// Ports pnpm's error shape from `exec/lifecycle/src/runLifecycleHook.ts`.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum LifecycleScriptError {
    #[display("Failed to read package.json at {path}: {source}")]
    #[diagnostic(code(pacquet_executor::read_manifest))]
    ReadManifest {
        path: String,
        #[error(source)]
        source: PackageManifestError,
    },

    #[display("{dep_path} {stage}: `{script}` exited with {status}")]
    #[diagnostic(code(pacquet_executor::lifecycle_script_failed))]
    ScriptFailed { dep_path: String, stage: String, script: String, status: ExitStatus },

    #[display("Failed to spawn lifecycle script for {dep_path} {stage}: {source}")]
    #[diagnostic(code(pacquet_executor::spawn_lifecycle))]
    Spawn {
        dep_path: String,
        stage: String,
        #[error(source)]
        source: std::io::Error,
    },

    #[display("Failed waiting for lifecycle script for {dep_path} {stage}: {source}")]
    #[diagnostic(code(pacquet_executor::wait_lifecycle))]
    Wait {
        dep_path: String,
        stage: String,
        #[error(source)]
        source: std::io::Error,
    },
}

/// Options for [`run_postinstall_hooks`].
///
/// Ports the subset of `RunLifecycleHookOptions` from
/// `exec/lifecycle/src/runLifecycleHook.ts` that the headless
/// installer needs.
pub struct RunPostinstallHooks<'a> {
    pub dep_path: &'a str,
    pub pkg_root: &'a Path,
    pub root_modules_dir: &'a Path,
    pub init_cwd: &'a Path,
    pub extra_bin_paths: &'a [PathBuf],
    pub extra_env: &'a HashMap<String, String>,
}

/// Run the preinstall, install, and postinstall lifecycle scripts for
/// a single dependency.
///
/// Ports `runPostinstallHooks` from
/// `https://github.com/pnpm/pnpm/blob/80037699fb/exec/lifecycle/src/index.ts`.
///
/// Returns `true` if any script was present and executed.
pub fn run_postinstall_hooks<R: Reporter>(
    opts: RunPostinstallHooks<'_>,
) -> Result<bool, LifecycleScriptError> {
    let manifest = match safe_read_package_json_from_dir(opts.pkg_root) {
        Ok(Some(value)) => value,
        Ok(None) => return Ok(false),
        Err(source) => {
            return Err(LifecycleScriptError::ReadManifest {
                path: opts.pkg_root.join("package.json").display().to_string(),
                source,
            });
        }
    };

    let scripts = manifest.get("scripts").and_then(|v| v.as_object());
    let get_script =
        |name: &str| -> Option<&str> { scripts.and_then(|s| s.get(name)).and_then(|v| v.as_str()) };

    let mut ran_any = false;

    if let Some(script) = get_script("preinstall")
        && script != "npx only-allow pnpm"
    {
        run_lifecycle_hook::<R>("preinstall", script, &opts)?;
        ran_any = true;
    }

    let install_script = get_script("install").map(String::from).or_else(|| {
        if get_script("preinstall").is_none() && opts.pkg_root.join("binding.gyp").exists() {
            Some("node-gyp rebuild".to_string())
        } else {
            None
        }
    });
    if let Some(script) = &install_script
        && script != "npx only-allow pnpm"
    {
        run_lifecycle_hook::<R>("install", script, &opts)?;
        ran_any = true;
    }

    if let Some(script) = get_script("postinstall")
        && script != "npx only-allow pnpm"
    {
        run_lifecycle_hook::<R>("postinstall", script, &opts)?;
        ran_any = true;
    }

    Ok(ran_any)
}

/// Run a single lifecycle hook and emit `pnpm:lifecycle` events.
///
/// Ports the core of `runLifecycleHook` from
/// `https://github.com/pnpm/pnpm/blob/80037699fb/exec/lifecycle/src/runLifecycleHook.ts`.
///
/// Mirrors the upstream emit ordering: a `Script` event before the spawn,
/// `Stdio` events for each stdout/stderr line, then an `Exit` event with
/// the resolved exit code.
fn run_lifecycle_hook<R: Reporter>(
    stage: &str,
    script: &str,
    opts: &RunPostinstallHooks<'_>,
) -> Result<(), LifecycleScriptError> {
    tracing::debug!(
        target: "pacquet::lifecycle",
        dep_path = opts.dep_path,
        stage,
        script,
        pkg_root = %opts.pkg_root.display(),
    );

    let pkg_root_str = opts.pkg_root.to_string_lossy().into_owned();

    // Mirrors `lifecycleLogger.debug({ depPath, optional, script, stage, wd })`
    // at <https://github.com/pnpm/pnpm/blob/80037699fb/exec/lifecycle/src/runLifecycleHook.ts#L102>.
    R::emit(&LogEvent::Lifecycle(LifecycleLog {
        level: LogLevel::Debug,
        message: LifecycleMessage::Script {
            dep_path: opts.dep_path.to_string(),
            optional: false,
            script: script.to_string(),
            stage: stage.to_string(),
            wd: pkg_root_str.clone(),
        },
    }));

    let path_env = build_path_env(opts.pkg_root, opts.extra_bin_paths);

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(script)
        .current_dir(opts.pkg_root)
        .env("PATH", &path_env)
        .env("INIT_CWD", opts.init_cwd)
        .env("PNPM_SCRIPT_SRC_DIR", opts.pkg_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in opts.extra_env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().map_err(|e| LifecycleScriptError::Spawn {
        dep_path: opts.dep_path.to_string(),
        stage: stage.to_string(),
        source: e,
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_handle = stdout.map(|s| {
        spawn_line_pump::<R>(s, LifecycleStdio::Stdout, opts.dep_path, stage, &pkg_root_str)
    });
    let stderr_handle = stderr.map(|s| {
        spawn_line_pump::<R>(s, LifecycleStdio::Stderr, opts.dep_path, stage, &pkg_root_str)
    });

    let status = child.wait().map_err(|e| LifecycleScriptError::Wait {
        dep_path: opts.dep_path.to_string(),
        stage: stage.to_string(),
        source: e,
    })?;

    // Joining the pumps after `wait` ensures every line they read is
    // emitted before the `Exit` event below, matching pnpm's ordering.
    if let Some(h) = stdout_handle {
        let _ = h.join();
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }

    // Mirrors `lifecycleLogger.debug({ depPath, exitCode, optional, stage, wd })`
    // at <https://github.com/pnpm/pnpm/blob/80037699fb/exec/lifecycle/src/runLifecycleHook.ts#L165>.
    R::emit(&LogEvent::Lifecycle(LifecycleLog {
        level: LogLevel::Debug,
        message: LifecycleMessage::Exit {
            dep_path: opts.dep_path.to_string(),
            exit_code: status.code().unwrap_or(-1),
            optional: false,
            stage: stage.to_string(),
            wd: pkg_root_str,
        },
    }));

    if !status.success() {
        return Err(LifecycleScriptError::ScriptFailed {
            dep_path: opts.dep_path.to_string(),
            stage: stage.to_string(),
            script: script.to_string(),
            status,
        });
    }

    Ok(())
}

/// Spawn a thread that reads `reader` line-by-line and emits a
/// `LifecycleMessage::Stdio` event per line. Mirrors the per-chunk
/// logging callback at
/// <https://github.com/pnpm/pnpm/blob/80037699fb/exec/lifecycle/src/runLifecycleHook.ts#L147>.
fn spawn_line_pump<R: Reporter>(
    reader: impl Read + Send + 'static,
    stdio: LifecycleStdio,
    dep_path: &str,
    stage: &str,
    wd: &str,
) -> thread::JoinHandle<()> {
    let dep_path = dep_path.to_string();
    let stage = stage.to_string();
    let wd = wd.to_string();
    thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            let Ok(line) = line else {
                // Stop pumping on read error — an EBADF or EPIPE means
                // the child closed the stream. Errors are not fatal to
                // the install; the wait below will surface a non-zero
                // exit code if the child failed because of them.
                break;
            };
            R::emit(&LogEvent::Lifecycle(LifecycleLog {
                level: LogLevel::Debug,
                message: LifecycleMessage::Stdio {
                    dep_path: dep_path.clone(),
                    line,
                    stage: stage.clone(),
                    stdio,
                    wd: wd.clone(),
                },
            }));
        }
    })
}

/// Build the `PATH` environment variable for lifecycle scripts.
///
/// Prepends the package's own `node_modules/.bin`, any extra bin paths
/// (from the caller), and the system PATH.
fn build_path_env(pkg_root: &Path, extra_bin_paths: &[PathBuf]) -> OsString {
    let own_bin = pkg_root.join("node_modules/.bin");
    let system_path = env::var_os("PATH").unwrap_or_default();

    let mut paths: Vec<PathBuf> = Vec::with_capacity(2 + extra_bin_paths.len());
    paths.push(own_bin);
    paths.extend_from_slice(extra_bin_paths);
    for path in env::split_paths(&system_path) {
        paths.push(path);
    }

    env::join_paths(paths).unwrap_or(system_path)
}

#[cfg(test)]
mod tests;
