use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
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
        source: std::io::Error,
    },

    #[display("Failed to parse package.json at {path}: {source}")]
    #[diagnostic(code(pacquet_executor::parse_manifest))]
    ParseManifest {
        path: String,
        #[error(source)]
        source: serde_json::Error,
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
    pub optional: bool,
}

/// Run the preinstall, install, and postinstall lifecycle scripts for
/// a single dependency.
///
/// Ports `runPostinstallHooks` from
/// `https://github.com/pnpm/pnpm/blob/7e91e4b35f/exec/lifecycle/src/index.ts`.
///
/// Returns `true` if any script was present and executed.
pub fn run_postinstall_hooks(opts: RunPostinstallHooks<'_>) -> Result<bool, LifecycleScriptError> {
    let manifest_path = opts.pkg_root.join("package.json");
    let manifest_text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(LifecycleScriptError::ReadManifest {
                path: manifest_path.display().to_string(),
                source: err,
            });
        }
    };
    let manifest: serde_json::Value = serde_json::from_str(&manifest_text).map_err(|e| {
        LifecycleScriptError::ParseManifest { path: manifest_path.display().to_string(), source: e }
    })?;

    let scripts = manifest.get("scripts").and_then(|v| v.as_object());
    let get_script =
        |name: &str| -> Option<&str> { scripts.and_then(|s| s.get(name)).and_then(|v| v.as_str()) };

    let has_preinstall = get_script("preinstall").is_some();
    let has_postinstall = get_script("postinstall").is_some();

    let mut ran_any = false;

    if let Some(script) = get_script("preinstall") {
        run_lifecycle_hook("preinstall", script, &opts)?;
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
        run_lifecycle_hook("install", script, &opts)?;
        ran_any = true;
    }

    if let Some(script) = get_script("postinstall") {
        run_lifecycle_hook("postinstall", script, &opts)?;
        ran_any = true;
    }

    Ok(has_preinstall || install_script.is_some() || has_postinstall || ran_any)
}

/// Run a single lifecycle hook.
///
/// Ports the core of `runLifecycleHook` from
/// `https://github.com/pnpm/pnpm/blob/7e91e4b35f/exec/lifecycle/src/runLifecycleHook.ts`.
fn run_lifecycle_hook(
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

    let path_env = build_path_env(opts.pkg_root, opts.extra_bin_paths);

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(script)
        .current_dir(opts.pkg_root)
        .env("PATH", &path_env)
        .env("INIT_CWD", opts.init_cwd)
        .env("PNPM_SCRIPT_SRC_DIR", opts.pkg_root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    for (key, value) in opts.extra_env {
        cmd.env(key, value);
    }

    let status = cmd
        .spawn()
        .map_err(|e| LifecycleScriptError::Spawn {
            dep_path: opts.dep_path.to_string(),
            stage: stage.to_string(),
            source: e,
        })?
        .wait()
        .map_err(|e| LifecycleScriptError::Wait {
            dep_path: opts.dep_path.to_string(),
            stage: stage.to_string(),
            source: e,
        })?;

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

/// Build the `PATH` environment variable for lifecycle scripts.
///
/// Prepends the package's own `node_modules/.bin`, any extra bin paths
/// (from the caller), and the system PATH.
fn build_path_env(pkg_root: &Path, extra_bin_paths: &[PathBuf]) -> String {
    let own_bin = pkg_root.join("node_modules/.bin");
    let system_path = env::var_os("PATH").unwrap_or_default();

    let mut paths: Vec<PathBuf> = Vec::with_capacity(2 + extra_bin_paths.len());
    paths.push(own_bin);
    paths.extend_from_slice(extra_bin_paths);
    for path in env::split_paths(&system_path) {
        paths.push(path);
    }

    env::join_paths(paths).expect("join PATH entries").into_string().expect("PATH is valid UTF-8")
}
