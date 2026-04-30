//! User-facing log channels for pacquet.
//!
//! Pacquet's progress, lifecycle, summary, and similar output is shaped to
//! match pnpm's so that emitted NDJSON is consumable by
//! `@pnpm/cli.default-reporter`. The wire format mirrors what
//! [`@pnpm/core-loggers`](https://github.com/pnpm/pnpm/tree/3b12eb27de/core/core-loggers/src)
//! defines for each channel.
//!
//! # Adding a channel
//!
//! Only the variants pacquet currently emits live in [`LogEvent`]. New
//! channels are added incrementally as the surrounding code starts using
//! them.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// One log channel from `@pnpm/core-loggers`.
///
/// Variants are added as pacquet starts emitting them. The `name` tag in
/// the serialized JSON identifies the channel; consumers (notably
/// `@pnpm/cli.default-reporter`) dispatch on this value.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "name")]
pub enum LogEvent {
    /// Coarse install-pipeline phase markers (`pnpm:stage`).
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/3b12eb27de/core/core-loggers/src/stageLogger.ts>.
    #[serde(rename = "pnpm:stage")]
    Stage(StageLog),
}

/// `pnpm:stage` payload.
///
/// `prefix` is the project root path the stage applies to, matching pnpm's
/// usage. `stage` is the phase marker — see [`Stage`].
#[derive(Debug, Clone, Serialize)]
pub struct StageLog {
    pub level: LogLevel,
    pub prefix: String,
    pub stage: Stage,
}

/// `pnpm:stage` phase marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    ResolutionStarted,
    ResolutionDone,
    ImportingStarted,
    ImportingDone,
}

/// Severity level on the [bunyan]-shaped envelope.
///
/// pnpm's logger uses the [bole] library, which writes one of these strings
/// for every record. Each channel pins the level pnpm itself uses (e.g.
/// `pnpm:stage` is always emitted at `debug`).
///
/// [bunyan]: https://github.com/trentm/node-bunyan
/// [bole]: https://github.com/rvagg/bole
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Capability for emitting log events.
///
/// Implementations are unit structs; any implementation-internal state
/// lives in module-level `static`s. Emitting code is generic over
/// `R: Reporter` and calls `R::emit(...)`; the production entry point
/// monomorphises with the chosen sink.
///
/// [`Reporter::emit`] must not panic. A serialization or I/O failure is
/// swallowed so a reporter problem can never crash an install.
pub trait Reporter {
    fn emit(event: &LogEvent);
}

/// `--reporter=silent`: every event is dropped.
pub struct SilentReporter;

impl Reporter for SilentReporter {
    fn emit(_event: &LogEvent) {}
}

/// `--reporter=ndjson`: writes one [bunyan]-shaped JSON record per event to
/// stderr, terminated by `\n`. The wire format matches what pnpm itself
/// produces under `--reporter=ndjson`, so the same consumers work
/// unmodified.
///
/// Today this writes synchronously under the stderr lock. When the volume
/// of emit sites grows past coarse start/end markers, the writer should
/// move behind an MPSC channel.
///
/// [bunyan]: https://github.com/trentm/node-bunyan
pub struct NdjsonReporter;

impl Reporter for NdjsonReporter {
    fn emit(event: &LogEvent) {
        let mut buf = Vec::with_capacity(256);
        if write_record(&mut buf, event).is_err() {
            return;
        }
        buf.push(b'\n');
        let _ = std::io::stderr().lock().write_all(&buf);
    }
}

fn write_record(buf: &mut Vec<u8>, event: &LogEvent) -> serde_json::Result<()> {
    let envelope =
        Envelope { time: now_millis(), hostname: hostname(), pid: std::process::id(), event };
    serde_json::to_writer(buf, &envelope)
}

// Wraps a [`LogEvent`] with the bunyan envelope fields pnpm's logger adds.
// `#[serde(flatten)]` merges the channel-specific tag and payload fields up
// to the top level of the JSON object so the wire format is one flat record
// per line.
#[derive(Serialize)]
struct Envelope<'a> {
    time: u128,
    hostname: &'a str,
    pid: u32,
    #[serde(flatten)]
    event: &'a LogEvent,
}

fn now_millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Capability for reading process environment variables.
///
/// Lets env-dependent helpers in this crate be exercised without touching
/// the real process environment. The production implementation is
/// [`RealEnvVar`].
pub trait EnvVar {
    fn var(name: &str) -> Option<String>;
}

/// Production [`EnvVar`] backed by [`std::env::var`].
pub struct RealEnvVar;

impl EnvVar for RealEnvVar {
    fn var(name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Resolve the hostname for the [bunyan]-shaped envelope from `E`.
///
/// pnpm's logger (via [bole]) populates this from `os.hostname()`. Pacquet
/// reads it from the standard environment variables instead so we don't
/// pay for a syscall on every reporter init: `HOSTNAME` on Unix shells,
/// `COMPUTERNAME` on Windows. Empty string when neither is set —
/// downstream consumers (notably `@pnpm/cli.default-reporter`) only
/// dispatch on `name`, so this field is informational.
///
/// [bunyan]: https://github.com/trentm/node-bunyan
/// [bole]: https://github.com/rvagg/bole
fn resolve_hostname<E: EnvVar>() -> String {
    E::var("HOSTNAME").or_else(|| E::var("COMPUTERNAME")).unwrap_or_default()
}

// Cached hostname for production use. Kept non-generic because `static`
// items inside generic functions are *not* monomorphised per type
// parameter, so a generic `OnceLock` cache would leak the first
// instantiation's value across every `E`.
fn hostname() -> &'static str {
    use std::sync::OnceLock;
    static HOSTNAME: OnceLock<String> = OnceLock::new();
    HOSTNAME.get_or_init(resolve_hostname::<RealEnvVar>).as_str()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    use super::*;

    /// Stage log serializes with the channel name flattened into the
    /// envelope alongside `time`, `hostname`, `pid`, and the payload
    /// fields. This is the wire shape `@pnpm/cli.default-reporter`
    /// consumes — adding a wrapper object would break it.
    #[test]
    fn stage_event_matches_pnpm_wire_shape() {
        let event = LogEvent::Stage(StageLog {
            level: LogLevel::Debug,
            prefix: "/some/project".to_string(),
            stage: Stage::ImportingStarted,
        });
        let envelope =
            Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };

        let json: Value = envelope
            .pipe_ref(serde_json::to_string)
            .expect("serialize envelope")
            .pipe_as_ref(serde_json::from_str)
            .expect("parse JSON");

        assert_eq!(json["name"], "pnpm:stage");
        assert_eq!(json["stage"], "importing_started");
        assert_eq!(json["level"], "debug");
        assert_eq!(json["prefix"], "/some/project");
        assert_eq!(json["time"], 1_700_000_000_000_u64);
        assert_eq!(json["hostname"], "host");
        assert_eq!(json["pid"], 4242);
    }

    /// Phase markers serialize as the snake_case strings pnpm uses.
    #[test]
    fn stage_phases_serialize_in_pnpm_form() {
        let cases = [
            (Stage::ResolutionStarted, "resolution_started"),
            (Stage::ResolutionDone, "resolution_done"),
            (Stage::ImportingStarted, "importing_started"),
            (Stage::ImportingDone, "importing_done"),
        ];
        for (stage, expected) in cases {
            let json = serde_json::to_string(&stage).expect("serialize stage");
            assert_eq!(json, format!("\"{expected}\""), "phase {expected}");
        }
    }

    /// [`SilentReporter`] is observably a no-op: any test fake is harder to
    /// write than just calling it.
    #[test]
    fn silent_reporter_drops_events() {
        // The point is that no panic, no I/O, and no observable side
        // effect happens. The test passes by virtue of the call returning.
        SilentReporter::emit(&LogEvent::Stage(StageLog {
            level: LogLevel::Debug,
            prefix: String::new(),
            stage: Stage::ImportingStarted,
        }));
    }

    #[test]
    fn recording_fake_captures_emitted_events() {
        static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());

        struct RecordingReporter;
        impl Reporter for RecordingReporter {
            fn emit(event: &LogEvent) {
                EVENTS.lock().unwrap().push(event.clone());
            }
        }

        fn install_step<R: Reporter>() {
            R::emit(&LogEvent::Stage(StageLog {
                level: LogLevel::Debug,
                prefix: "/proj".to_string(),
                stage: Stage::ImportingStarted,
            }));
            R::emit(&LogEvent::Stage(StageLog {
                level: LogLevel::Debug,
                prefix: "/proj".to_string(),
                stage: Stage::ImportingDone,
            }));
        }

        install_step::<RecordingReporter>();

        let captured = EVENTS.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(matches!(
            &captured[0],
            LogEvent::Stage(StageLog { stage: Stage::ImportingStarted, .. })
        ));
        assert!(matches!(
            &captured[1],
            LogEvent::Stage(StageLog { stage: Stage::ImportingDone, .. })
        ));
    }

    // Per-thread mock environment used by the [`resolve_hostname`] tests.
    // `cargo nextest` runs each test in its own process, and the std test
    // runner runs each test on its own thread, so a `thread_local!` is
    // enough isolation either way.
    struct MockEnv;

    thread_local! {
        static MOCK_ENV: RefCell<HashMap<&'static str, &'static str>> =
            RefCell::new(HashMap::new());
    }

    impl EnvVar for MockEnv {
        fn var(name: &str) -> Option<String> {
            MOCK_ENV.with(|m| m.borrow().get(name).map(|s| (*s).to_string()))
        }
    }

    fn with_mock_env<R>(entries: &[(&'static str, &'static str)], f: impl FnOnce() -> R) -> R {
        MOCK_ENV.with(|m| {
            let mut map = m.borrow_mut();
            map.clear();
            for (k, v) in entries {
                map.insert(k, v);
            }
        });
        let result = f();
        MOCK_ENV.with(|m| m.borrow_mut().clear());
        result
    }

    /// `HOSTNAME` is the preferred source — Unix shells set it.
    #[test]
    fn resolve_hostname_prefers_hostname_env() {
        let got = with_mock_env(&[("HOSTNAME", "unix-host")], resolve_hostname::<MockEnv>);
        assert_eq!(got, "unix-host");
    }

    /// On Windows, `COMPUTERNAME` is the standard variable; pacquet falls
    /// back to it when `HOSTNAME` is unset.
    #[test]
    fn resolve_hostname_falls_back_to_computername() {
        let got = with_mock_env(&[("COMPUTERNAME", "WIN-PC")], resolve_hostname::<MockEnv>);
        assert_eq!(got, "WIN-PC");
    }

    /// When both are set, `HOSTNAME` wins. Some Windows shells (e.g. Git
    /// Bash, WSL bridges) populate both; the Unix-style variable is the
    /// one pnpm itself observes via `os.hostname()` indirection on
    /// MINGW/MSYS hosts.
    #[test]
    fn resolve_hostname_hostname_wins_over_computername() {
        let got = with_mock_env(
            &[("HOSTNAME", "primary"), ("COMPUTERNAME", "secondary")],
            resolve_hostname::<MockEnv>,
        );
        assert_eq!(got, "primary");
    }

    /// Neither set → empty string. The bunyan envelope's `hostname` field
    /// is informational, so this is acceptable rather than an error.
    #[test]
    fn resolve_hostname_empty_when_unset() {
        let got = with_mock_env(&[], resolve_hostname::<MockEnv>);
        assert_eq!(got, "");
    }
}
