//! User-facing log channels for pacquet.
//!
//! Pacquet's progress, lifecycle, summary, and similar output is shaped to
//! match pnpm's so that emitted NDJSON is consumable by
//! `@pnpm/cli.default-reporter`. The wire format mirrors what
//! [`@pnpm/core-loggers`](https://github.com/pnpm/pnpm/tree/3b12eb27de/core/core-loggers/)
//! defines for each channel.
//!
//! # Adding a channel
//!
//! Only the variants pacquet currently emits live in [`LogEvent`]. New
//! channels are added incrementally as the surrounding code starts using
//! them.

use std::io::Write;
use std::sync::LazyLock;
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
    /// Install context: store directory, virtual-store directory, and
    /// whether a current lockfile (`node_modules/.pnpm/lock.yaml`) was
    /// loaded (`pnpm:context`).
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/contextLogger.ts>.
    /// Emit site: <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/context/src/index.ts#L196>.
    #[serde(rename = "pnpm:context")]
    Context(ContextLog),

    /// Coarse install-pipeline phase markers (`pnpm:stage`).
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/3b12eb27de/core/core-loggers/src/stageLogger.ts>.
    #[serde(rename = "pnpm:stage")]
    Stage(StageLog),
}

/// `pnpm:context` payload.
///
/// Emitted once per install when the install context has been
/// constructed. Field names match pnpm's wire shape (camelCase) so
/// `@pnpm/cli.default-reporter` accepts the record unchanged.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextLog {
    pub level: LogLevel,
    pub current_lockfile_exists: bool,
    pub store_dir: String,
    pub virtual_store_dir: String,
}

/// `pnpm:stage` payload.
///
/// `prefix` is the project root path the stage applies to, matching pnpm's
/// usage. `stage` is the phase marker; see [`Stage`].
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
        Envelope { time: now_millis(), hostname: &HOSTNAME, pid: std::process::id(), event };
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

/// Capability for obtaining the host name written into the [bunyan]-shaped
/// envelope.
///
/// Backed by a real syscall in production via [`RealApi`]. Tests can supply
/// their own implementation when behavior depends on the value.
///
/// [bunyan]: https://github.com/trentm/node-bunyan
pub trait GetHostName {
    fn get_host_name() -> String;
}

/// Production implementation of the capability traits in this crate.
///
/// Each trait method calls into the real underlying system facility (for
/// [`GetHostName`], the `gethostname` syscall via the [`gethostname`] crate).
pub struct RealApi;

impl GetHostName for RealApi {
    fn get_host_name() -> String {
        gethostname::gethostname().to_string_lossy().into_owned()
    }
}

// Process-wide cache of the host name. The value cannot change at runtime,
// and `gethostname` is one syscall we'd otherwise repeat on every emit.
// Initialized lazily through `RealApi::get_host_name` so tests that exercise
// the capability trait directly can do so without paying for the syscall.
static HOSTNAME: LazyLock<String> = LazyLock::new(RealApi::get_host_name);

#[cfg(test)]
mod tests;
