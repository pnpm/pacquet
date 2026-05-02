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

    /// End-of-install marker (`pnpm:summary`). pnpm's reporter combines
    /// this with the accumulated `pnpm:root` events to render the final
    /// "+N -M" block.
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/summaryLogger.ts>.
    /// Emit site: <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/index.ts#L1663>.
    #[serde(rename = "pnpm:summary")]
    Summary(SummaryLog),

    /// The import method used to materialise files from the store
    /// (`pnpm:package-import-method`). Fires the first time each
    /// resolved method (`clone` / `hardlink` / `copy`) actually
    /// succeeds during an install — so for the `auto` and
    /// `clone-or-copy` config values, the wire value reflects the
    /// post-fallback method rather than the optimistic configured
    /// one. Up to three events per install (one per resolved method)
    /// gated by an install-scoped atomic in `pacquet-package-manager`.
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/packageImportMethodLogger.ts>.
    /// Emit site: <https://github.com/pnpm/pnpm/blob/086c5e91e8/fs/indexed-pkg-importer/src/index.ts#L32>.
    #[serde(rename = "pnpm:package-import-method")]
    PackageImportMethod(PackageImportMethodLog),

    /// Per-package status transitions (`pnpm:progress`). One of four
    /// `status` values per record: `resolved`, `fetched`,
    /// `found_in_store`, or `imported`. The first three carry
    /// `{ packageId, requester }`; `imported` carries
    /// `{ method, requester, to }`. Together they drive the
    /// "X/Y resolved, X/Y fetched, X/Y imported" counters in the
    /// default reporter.
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/progressLogger.ts>.
    #[serde(rename = "pnpm:progress")]
    Progress(ProgressLog),

    /// Per-tarball download progress (`pnpm:fetching-progress`). Two
    /// `status` values: `started` (one-shot per fetch attempt with
    /// `attempt`, `packageId`, and `size` from the response's
    /// `Content-Length`) and `in_progress` (throttled to ~200ms while
    /// the body streams, with `downloaded` and `packageId`).
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/fetchingProgressLogger.ts>.
    /// Emit site: <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/package-requester/src/packageRequester.ts#L560>.
    #[serde(rename = "pnpm:fetching-progress")]
    FetchingProgress(FetchingProgressLog),

    /// Project manifest snapshots (`pnpm:package-manifest`). Two
    /// presence-tagged shapes per pnpm's union: `initial` (emitted
    /// once at install start with the on-disk manifest) and
    /// `updated` (emitted after the manifest is rewritten — e.g.
    /// `pacquet add` saves a new dependency entry).
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/packageManifestLogger.ts>.
    #[serde(rename = "pnpm:package-manifest")]
    PackageManifest(PackageManifestLog),

    /// Per-direct-dependency add / remove events (`pnpm:root`). pnpm's
    /// reporter accumulates these and renders the "+N -M" block at
    /// `pnpm:summary` time.
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/rootLogger.ts>.
    /// Emit site: <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/linking/direct-dep-linker/src/linkDirectDeps.ts#L131>.
    #[serde(rename = "pnpm:root")]
    Root(RootLog),

    /// Aggregate add / remove counts emitted once per project after
    /// the link phase (`pnpm:stats`). Pnpm emits `added` and
    /// `removed` from separate sites; pacquet currently emits both
    /// together because pruning hasn't landed yet — see
    /// [`StatsMessage::Removed`].
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/statsLogger.ts>.
    /// Emit site: <https://github.com/pnpm/pnpm/blob/086c5e91e8/installing/deps-installer/src/install/link.ts#L363>.
    #[serde(rename = "pnpm:stats")]
    Stats(StatsLog),

    /// One per failed-and-being-retried HTTP request
    /// (`pnpm:request-retry`). Pnpm's default reporter surfaces these
    /// as `Will retry in <ms>. <N> retries left.` warnings; the
    /// `error` payload is what the JS reporter dispatches on
    /// (`httpStatusCode` / `status` / `errno` / `code`) to render the
    /// reason.
    ///
    /// Upstream: <https://github.com/pnpm/pnpm/blob/086c5e91e8/core/core-loggers/src/requestRetryLogger.ts>.
    /// Emit site: <https://github.com/pnpm/pnpm/blob/086c5e91e8/fetching/tarball-fetcher/src/remoteTarballFetcher.ts#L91>.
    #[serde(rename = "pnpm:request-retry")]
    RequestRetry(RequestRetryLog),
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

/// `pnpm:summary` payload. `prefix` identifies the importer; pnpm's
/// reporter uses it to look up the matching `pnpm:root` history and
/// render its "+N -M" diff. `level` is the [bunyan]-envelope severity,
/// common to every channel.
///
/// [bunyan]: https://github.com/trentm/node-bunyan
#[derive(Debug, Clone, Serialize)]
pub struct SummaryLog {
    pub level: LogLevel,
    pub prefix: String,
}

/// `pnpm:package-import-method` payload. The method names match pnpm's
/// wire shape exactly — anything else would silently fail to render
/// even though the JSON parses.
#[derive(Debug, Clone, Serialize)]
pub struct PackageImportMethodLog {
    pub level: LogLevel,
    pub method: PackageImportMethod,
}

/// Wire-format import method. pnpm only knows three values; pacquet's
/// config enum (`pacquet_npmrc::PackageImportMethod`) carries `Auto`
/// and `CloneOrCopy` on top of those, but those are dispatched-on by
/// the auto-importer's fallback chain, not emitted. The wire value is
/// the resolved method `link_file` actually used — `Clone` /
/// `Hardlink` / `Copy` — so an `auto` install that falls back to
/// hardlink emits `hardlink`, not the optimistic `clone`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageImportMethod {
    Clone,
    Hardlink,
    Copy,
}

/// `pnpm:progress` payload. The bunyan-envelope `level` is a fixed
/// outer field; the rest of the record is a status-tagged union via
/// `#[serde(flatten)]` so the wire shape stays flat (matching pnpm's
/// `ProgressMessage` discriminator on `status`).
#[derive(Debug, Clone, Serialize)]
pub struct ProgressLog {
    pub level: LogLevel,
    #[serde(flatten)]
    pub message: ProgressMessage,
}

/// `pnpm:progress` discriminated payload. `Resolved` / `Fetched` /
/// `FoundInStore` share `{ packageId, requester }`; `Imported` differs
/// (`{ method, requester, to }` — no `packageId`).
///
/// `requester` is the install root — same value as the
/// [`StageLog::prefix`] threaded through `Install::run`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProgressMessage {
    Resolved {
        #[serde(rename = "packageId")]
        package_id: String,
        requester: String,
    },
    Fetched {
        #[serde(rename = "packageId")]
        package_id: String,
        requester: String,
    },
    FoundInStore {
        #[serde(rename = "packageId")]
        package_id: String,
        requester: String,
    },
    Imported {
        method: PackageImportMethod,
        requester: String,
        to: String,
    },
}

/// `pnpm:fetching-progress` payload. Same flatten-on-status pattern as
/// [`ProgressLog`].
#[derive(Debug, Clone, Serialize)]
pub struct FetchingProgressLog {
    pub level: LogLevel,
    #[serde(flatten)]
    pub message: FetchingProgressMessage,
}

/// `pnpm:fetching-progress` discriminated payload. `Started` carries
/// the retry-attempt index and the `Content-Length`-derived size
/// (`null` when chunked / unknown — preserved as JSON `null`).
/// `InProgress` carries the running byte count; pacquet throttles
/// these to ~200ms per package, mirroring pnpm's reporter coalescing
/// window.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FetchingProgressMessage {
    Started {
        attempt: u32,
        #[serde(rename = "packageId")]
        package_id: String,
        size: Option<u64>,
    },
    InProgress {
        downloaded: u64,
        #[serde(rename = "packageId")]
        package_id: String,
    },
}

/// `pnpm:package-manifest` payload. The bunyan-envelope `level` is a
/// fixed outer field; the rest is a presence-tagged union — pnpm
/// keys on whether `initial` or `updated` is present rather than
/// using a `status` discriminator. `#[serde(untagged)]` matches
/// that shape; `#[serde(flatten)]` keeps `prefix` adjacent to
/// `initial` / `updated` at the top level.
#[derive(Debug, Clone, Serialize)]
pub struct PackageManifestLog {
    pub level: LogLevel,
    #[serde(flatten)]
    pub message: PackageManifestMessage,
}

/// `pnpm:package-manifest` discriminated payload. The `Value` carries
/// the entire on-disk `package.json` body — pnpm's reporter doesn't
/// pick fields out, it threads the manifest through to consumers
/// like the audit pipeline that need the full thing.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PackageManifestMessage {
    Initial { prefix: String, initial: serde_json::Value },
    Updated { prefix: String, updated: serde_json::Value },
}

/// `pnpm:root` payload. Same flatten-on-presence pattern as
/// [`PackageManifestLog`].
#[derive(Debug, Clone, Serialize)]
pub struct RootLog {
    pub level: LogLevel,
    #[serde(flatten)]
    pub message: RootMessage,
}

/// `pnpm:root` discriminated payload. pnpm's reporter dispatches on
/// whether `added` or `removed` is present; tag-on-presence matches
/// that. Pacquet only emits `added` today (no pruning pipeline yet)
/// — `Removed` is here to pin the wire shape so the channel is
/// usable when pruning lands.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum RootMessage {
    Added { prefix: String, added: AddedRoot },
    Removed { prefix: String, removed: RemovedRoot },
}

/// `added` payload on a [`RootMessage::Added`] event. `name` is the
/// directory name under `node_modules/` (the manifest alias for
/// npm-aliased entries; the package name otherwise). `real_name`
/// is the registry name. The other fields are optional in pnpm's
/// shape; pacquet populates what it has from the lockfile snapshot
/// today.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedRoot {
    pub name: String,
    pub real_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_type: Option<DependencyType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_from: Option<String>,
}

/// `removed` payload on a [`RootMessage::Removed`] event. Optional
/// fields match pnpm's shape and are skipped when absent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedRoot {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_type: Option<DependencyType>,
}

/// Direct-dependency category. Mirrors pnpm's three-value union;
/// peer dependencies are not a separate emit and don't appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyType {
    Prod,
    Dev,
    Optional,
}

/// `pnpm:stats` payload. Same flatten-on-presence pattern as
/// [`PackageManifestLog`] / [`RootLog`].
#[derive(Debug, Clone, Serialize)]
pub struct StatsLog {
    pub level: LogLevel,
    #[serde(flatten)]
    pub message: StatsMessage,
}

/// `pnpm:stats` discriminated payload. pnpm's reporter dispatches on
/// presence: an event carries either `added` *or* `removed`, never
/// both, because pnpm emits them from two separate sites.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum StatsMessage {
    Added { prefix: String, added: u64 },
    Removed { prefix: String, removed: u64 },
}

/// `pnpm:request-retry` payload. `attempt` is one-indexed (the failed
/// attempt that triggered the retry) and `timeout` is the
/// milliseconds the retry loop will sleep before the next attempt;
/// pnpm's default reporter renders both directly.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRetryLog {
    pub level: LogLevel,
    pub attempt: u32,
    pub error: RequestRetryError,
    pub max_retries: u32,
    pub method: String,
    pub timeout: u64,
    pub url: String,
}

/// JS-shaped error object the default-reporter dispatches on:
/// `error.httpStatusCode ?? error.status ?? error.errno ?? error.code`
/// is what gets rendered as the reason. pacquet populates whichever
/// field its [`pacquet_tarball::TarballError`] variant maps to (HTTP
/// status → `http_status_code`, decode / IO failures → `code`) and
/// always carries the rendered `message` so consumers that read
/// `err.message` directly still work.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRetryError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errno: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
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
///
/// **Thread safety.** `emit` may be invoked concurrently from
/// arbitrary threads — pacquet's import path runs `link_file` from a
/// rayon `par_iter`, and tarball download / store-index work runs
/// across tokio workers, all of which can fire reporter events at
/// once. Implementations must therefore guard any shared state they
/// touch (`Mutex`, atomic, or write-once initialization). Both
/// production sinks satisfy this: `SilentReporter` is a no-op, and
/// `NdjsonReporter` serializes per-event then writes under
/// `std::io::stderr().lock()`.
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
