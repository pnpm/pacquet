use super::{LifecycleScriptError, RunPostinstallHooks, run_postinstall_hooks};
use pacquet_package_manifest::PackageManifestError;
use pacquet_reporter::{
    LifecycleMessage, LifecycleStdio, LogEvent, LogLevel, Reporter, SilentReporter,
};
use pretty_assertions::assert_eq;
use std::{collections::HashMap, fs, sync::Mutex};
use tempfile::tempdir;

/// Recording-fake reporter that pushes every emitted [`LogEvent`] into
/// `EVENTS`. The static lives in this test function's own scope, so
/// other tests have independent buffers.
#[test]
fn lifecycle_emits_script_stdio_and_exit_in_order() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().expect("lock").clear();

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().expect("lock").push(event.clone());
        }
    }

    let dir = tempdir().expect("create temp dir");
    let pkg_root = dir.path();
    let manifest = serde_json::json!({
        "name": "x",
        "version": "1.0.0",
        "scripts": { "postinstall": "echo HELLO; echo BAD 1>&2" },
    });
    fs::write(pkg_root.join("package.json"), manifest.to_string()).expect("write manifest");

    let extra_env: HashMap<String, String> = HashMap::new();
    let extra_bin_paths: Vec<std::path::PathBuf> = vec![];
    let opts = RunPostinstallHooks {
        dep_path: "/x@1.0.0",
        pkg_root,
        root_modules_dir: pkg_root,
        init_cwd: pkg_root,
        extra_bin_paths: &extra_bin_paths,
        extra_env: &extra_env,
    };

    let ran = run_postinstall_hooks::<RecordingReporter>(opts).expect("postinstall");
    assert!(ran, "postinstall script should report executed");

    let captured = EVENTS.lock().expect("lock").clone();
    dbg!(&captured);

    // Sequence: Script (postinstall) → some Stdio events → Exit (0).
    let first = captured.first().expect("at least one event");
    let LogEvent::Lifecycle(first) = first else {
        panic!("first event must be Lifecycle, got {first:?}");
    };
    assert_eq!(first.level, LogLevel::Debug);
    assert!(
        matches!(
            &first.message,
            LifecycleMessage::Script { dep_path, stage, script, .. }
                if dep_path == "/x@1.0.0"
                && stage == "postinstall"
                && script.contains("echo HELLO"),
        ),
        "first event must be Script(postinstall): {first:?}",
    );

    let last = captured.last().expect("at least one event");
    let LogEvent::Lifecycle(last) = last else {
        panic!("last event must be Lifecycle, got {last:?}");
    };
    assert!(
        matches!(
            &last.message,
            LifecycleMessage::Exit { dep_path, exit_code, stage, .. }
                if dep_path == "/x@1.0.0" && *exit_code == 0 && stage == "postinstall",
        ),
        "last event must be Exit(0): {last:?}",
    );

    // Stdio events between Script and Exit. Match by line content rather
    // than by index because the order between stdout and stderr is
    // race-y (each pumps from its own thread).
    let stdio: Vec<_> = captured
        .iter()
        .filter_map(|e| match e {
            LogEvent::Lifecycle(l) => match &l.message {
                LifecycleMessage::Stdio { line, stdio, .. } => Some((stdio, line.as_str())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    dbg!(&stdio);
    assert!(
        stdio.iter().any(|(s, l)| **s == LifecycleStdio::Stdout && *l == "HELLO"),
        "stdout 'HELLO' must be emitted: {stdio:?}",
    );
    assert!(
        stdio.iter().any(|(s, l)| **s == LifecycleStdio::Stderr && *l == "BAD"),
        "stderr 'BAD' must be emitted: {stdio:?}",
    );
}

/// Failing scripts emit a Script event, the captured stdio, and an Exit
/// event with the resolved non-zero exit code, then return a
/// [`LifecycleScriptError::ScriptFailed`].
#[test]
fn lifecycle_emits_exit_with_nonzero_code_on_failure() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().expect("lock").clear();

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().expect("lock").push(event.clone());
        }
    }

    let dir = tempdir().expect("create temp dir");
    let pkg_root = dir.path();
    let manifest = serde_json::json!({
        "name": "y",
        "version": "1.0.0",
        "scripts": { "postinstall": "exit 7" },
    });
    fs::write(pkg_root.join("package.json"), manifest.to_string()).expect("write manifest");

    let extra_env: HashMap<String, String> = HashMap::new();
    let extra_bin_paths: Vec<std::path::PathBuf> = vec![];
    let opts = RunPostinstallHooks {
        dep_path: "/y@1.0.0",
        pkg_root,
        root_modules_dir: pkg_root,
        init_cwd: pkg_root,
        extra_bin_paths: &extra_bin_paths,
        extra_env: &extra_env,
    };

    let err = run_postinstall_hooks::<RecordingReporter>(opts).expect_err("script must fail");
    eprintln!("ERR: {err}");

    let captured = EVENTS.lock().expect("lock").clone();
    dbg!(&captured);

    let last = captured.last().expect("at least one event");
    let LogEvent::Lifecycle(last) = last else {
        panic!("last event must be Lifecycle, got {last:?}");
    };
    assert!(
        matches!(&last.message, LifecycleMessage::Exit { exit_code, .. } if *exit_code == 7),
        "last event must be Exit(7): {last:?}",
    );
}

/// `SilentReporter` works as the production no-op. Same script, but no
/// recording — proves the function compiles and runs under the
/// production sink without touching the wire.
#[test]
fn lifecycle_runs_under_silent_reporter() {
    let dir = tempdir().expect("create temp dir");
    let pkg_root = dir.path();
    let manifest = serde_json::json!({
        "name": "z",
        "version": "1.0.0",
        "scripts": { "postinstall": "echo z" },
    });
    fs::write(pkg_root.join("package.json"), manifest.to_string()).expect("write manifest");

    let extra_env: HashMap<String, String> = HashMap::new();
    let extra_bin_paths: Vec<std::path::PathBuf> = vec![];
    let opts = RunPostinstallHooks {
        dep_path: "/z@1.0.0",
        pkg_root,
        root_modules_dir: pkg_root,
        init_cwd: pkg_root,
        extra_bin_paths: &extra_bin_paths,
        extra_env: &extra_env,
    };

    let ran = run_postinstall_hooks::<SilentReporter>(opts).expect("postinstall");
    assert!(ran, "postinstall script should report executed: ran={ran}");
}

/// Missing `package.json` is treated as "no scripts to run" — mirrors
/// upstream `safeReadPackageJsonFromDir` returning `null` on `ENOENT`
/// and `runPostinstallHooks` returning `false` for `null` packages
/// (`https://github.com/pnpm/pnpm/blob/80037699fb/exec/lifecycle/src/index.ts#L22-L23`).
#[test]
fn missing_manifest_returns_false() {
    let dir = tempdir().expect("create temp dir");
    let pkg_root = dir.path();
    // No package.json written.

    let extra_env: HashMap<String, String> = HashMap::new();
    let extra_bin_paths: Vec<std::path::PathBuf> = vec![];
    let opts = RunPostinstallHooks {
        dep_path: "/missing@1.0.0",
        pkg_root,
        root_modules_dir: pkg_root,
        init_cwd: pkg_root,
        extra_bin_paths: &extra_bin_paths,
        extra_env: &extra_env,
    };

    let ran = run_postinstall_hooks::<SilentReporter>(opts).expect("missing manifest is OK");
    assert!(!ran, "missing manifest must report no scripts ran: ran={ran}");
}

/// Malformed `package.json` surfaces as a `ReadManifest` error wrapping
/// `PackageManifestError::Serialization`. Mirrors upstream which throws
/// `BAD_PACKAGE_JSON` from `readPackageJson` and lets it propagate
/// through `safeReadPackageJsonFromDir` (only `ENOENT` is swallowed) at
/// `https://github.com/pnpm/pnpm/blob/80037699fb/pkg-manifest/reader/src/index.ts#L20-L46`.
#[test]
fn malformed_manifest_propagates_error() {
    let dir = tempdir().expect("create temp dir");
    let pkg_root = dir.path();
    fs::write(pkg_root.join("package.json"), "{ this is not valid json")
        .expect("write malformed manifest");

    let extra_env: HashMap<String, String> = HashMap::new();
    let extra_bin_paths: Vec<std::path::PathBuf> = vec![];
    let opts = RunPostinstallHooks {
        dep_path: "/malformed@1.0.0",
        pkg_root,
        root_modules_dir: pkg_root,
        init_cwd: pkg_root,
        extra_bin_paths: &extra_bin_paths,
        extra_env: &extra_env,
    };

    let err = run_postinstall_hooks::<SilentReporter>(opts).expect_err("malformed JSON must fail");
    eprintln!("ERR: {err}");
    assert!(
        matches!(
            err,
            LifecycleScriptError::ReadManifest {
                source: PackageManifestError::Serialization(_),
                ..
            },
        ),
        "expected ReadManifest(Serialization), got {err:?}",
    );
}
