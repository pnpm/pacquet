use std::sync::Mutex;

use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use serde_json::Value;

use crate::{
    ContextLog, Envelope, GetHostName, LogEvent, LogLevel, PackageImportMethod,
    PackageImportMethodLog, RealApi, Reporter, SilentReporter, Stage, StageLog, SummaryLog,
};

/// Context log serializes with the camelCase field names
/// `@pnpm/cli.default-reporter` expects (`currentLockfileExists`,
/// `storeDir`, `virtualStoreDir`); snake_case names would silently
/// fail to render even though the JSON is structurally valid.
#[test]
fn context_event_matches_pnpm_wire_shape() {
    let event = LogEvent::Context(ContextLog {
        level: LogLevel::Debug,
        current_lockfile_exists: false,
        store_dir: "/store".to_string(),
        virtual_store_dir: "/proj/node_modules/.pacquet".to_string(),
    });
    let envelope = Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };

    let json: Value = envelope
        .pipe_ref(serde_json::to_string)
        .expect("serialize envelope")
        .pipe_as_ref(serde_json::from_str)
        .expect("parse JSON");

    assert_eq!(json["name"], "pnpm:context");
    assert_eq!(json["level"], "debug");
    assert_eq!(json["currentLockfileExists"], false);
    assert_eq!(json["storeDir"], "/store");
    assert_eq!(json["virtualStoreDir"], "/proj/node_modules/.pacquet");
}

/// Stage log serializes with the channel name flattened into the
/// envelope alongside `time`, `hostname`, `pid`, and the payload
/// fields. This is the wire shape `@pnpm/cli.default-reporter`
/// consumes; adding a wrapper object would break it.
#[test]
fn stage_event_matches_pnpm_wire_shape() {
    let event = LogEvent::Stage(StageLog {
        level: LogLevel::Debug,
        prefix: "/some/project".to_string(),
        stage: Stage::ImportingStarted,
    });
    let envelope = Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };

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

/// Summary log serializes with the channel name flattened into the
/// envelope alongside `prefix` and the [bunyan]-envelope `level`.
/// `prefix` is what pnpm's reporter uses to find the matching
/// `pnpm:root` history and render its "+N -M" block.
///
/// [bunyan]: https://github.com/trentm/node-bunyan
#[test]
fn summary_event_matches_pnpm_wire_shape() {
    let event = LogEvent::Summary(SummaryLog {
        level: LogLevel::Debug,
        prefix: "/some/project".to_string(),
    });
    let envelope = Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };

    let json: Value = envelope
        .pipe_ref(serde_json::to_string)
        .expect("serialize envelope")
        .pipe_as_ref(serde_json::from_str)
        .expect("parse JSON");

    assert_eq!(json["name"], "pnpm:summary");
    assert_eq!(json["level"], "debug");
    assert_eq!(json["prefix"], "/some/project");
}

/// Package-import-method log carries the chosen method as one of
/// pnpm's three lowercase strings; anything else (e.g. the camelCase
/// `cloneOrCopy` from pacquet's config enum) would silently fail to
/// render.
#[test]
fn package_import_method_event_matches_pnpm_wire_shape() {
    let event = LogEvent::PackageImportMethod(PackageImportMethodLog {
        level: LogLevel::Debug,
        method: PackageImportMethod::Clone,
    });
    let envelope = Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };

    let json: Value = envelope
        .pipe_ref(serde_json::to_string)
        .expect("serialize envelope")
        .pipe_as_ref(serde_json::from_str)
        .expect("parse JSON");

    assert_eq!(json["name"], "pnpm:package-import-method");
    assert_eq!(json["level"], "debug");
    assert_eq!(json["method"], "clone");

    for (method, expected) in [
        (PackageImportMethod::Clone, "clone"),
        (PackageImportMethod::Hardlink, "hardlink"),
        (PackageImportMethod::Copy, "copy"),
    ] {
        let json = serde_json::to_string(&method).expect("serialize method");
        assert_eq!(json, format!("\"{expected}\""), "method {expected}");
    }
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

/// [`SilentReporter`] is observably a no-op. Any test fake is harder
/// to write than just calling it.
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
    assert!(matches!(&captured[1], LogEvent::Stage(StageLog { stage: Stage::ImportingDone, .. })));
}

/// A test fake of [`GetHostName`] returns whatever value its impl
/// declares. This proves the capability trait is dispatchable from a
/// test, which is what consumers of the trait need to know.
#[test]
fn get_host_name_capability_is_mockable() {
    struct FakeApi;
    impl GetHostName for FakeApi {
        fn get_host_name() -> String {
            "fixture-host".to_owned()
        }
    }
    assert_eq!(FakeApi::get_host_name(), "fixture-host");
}

/// [`RealApi::get_host_name`] returns the value of `gethostname(2)`,
/// which any real environment populates with at least one byte.
#[test]
fn real_api_returns_a_non_empty_host_name() {
    let host = RealApi::get_host_name();
    eprintln!("RealApi::get_host_name() = {host:?}");
    assert!(!host.is_empty());
}
