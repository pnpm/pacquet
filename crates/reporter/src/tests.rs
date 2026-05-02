use std::sync::Mutex;

use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use serde_json::Value;

use crate::{
    ContextLog, Envelope, FetchingProgressLog, FetchingProgressMessage, GetHostName, LogEvent,
    LogLevel, PackageImportMethod, PackageImportMethodLog, ProgressLog, ProgressMessage, RealApi,
    Reporter, SilentReporter, Stage, StageLog, SummaryLog,
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
/// pnpm's three lowercase strings; anything else (e.g. the
/// kebab-case `clone-or-copy` that `pacquet_npmrc::PackageImportMethod`
/// deserializes from) would silently fail to render.
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
        assert_eq!(json, format!("\"{expected}\""));
    }
}

/// `pnpm:progress` flattens its `status`-tagged payload into the
/// envelope. The three "store-ish" statuses (`resolved`, `fetched`,
/// `found_in_store`) carry `packageId` and `requester`; `imported`
/// substitutes `method` / `to` with no `packageId`. Mirroring pnpm's
/// shape exactly because the JS reporter's switch on `status` is the
/// dispatch.
#[test]
fn progress_event_matches_pnpm_wire_shape() {
    for (message, expected_status) in [
        (
            ProgressMessage::Resolved {
                package_id: "react@18.0.0".to_string(),
                requester: "/proj".to_string(),
            },
            "resolved",
        ),
        (
            ProgressMessage::Fetched {
                package_id: "react@18.0.0".to_string(),
                requester: "/proj".to_string(),
            },
            "fetched",
        ),
        (
            ProgressMessage::FoundInStore {
                package_id: "react@18.0.0".to_string(),
                requester: "/proj".to_string(),
            },
            "found_in_store",
        ),
    ] {
        let event = LogEvent::Progress(ProgressLog { level: LogLevel::Debug, message });
        let envelope =
            Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };

        let json: Value = envelope
            .pipe_ref(serde_json::to_string)
            .expect("serialize envelope")
            .pipe_as_ref(serde_json::from_str)
            .expect("parse JSON");

        assert_eq!(json["name"], "pnpm:progress");
        assert_eq!(json["level"], "debug");
        assert_eq!(json["status"], expected_status);
        assert_eq!(json["packageId"], "react@18.0.0");
        assert_eq!(json["requester"], "/proj");
    }

    let event = LogEvent::Progress(ProgressLog {
        level: LogLevel::Debug,
        message: ProgressMessage::Imported {
            method: PackageImportMethod::Hardlink,
            requester: "/proj".to_string(),
            to: "/proj/node_modules/.pacquet/react@18.0.0/node_modules/react".to_string(),
        },
    });
    let envelope = Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };
    let json: Value = envelope
        .pipe_ref(serde_json::to_string)
        .expect("serialize envelope")
        .pipe_as_ref(serde_json::from_str)
        .expect("parse JSON");

    assert_eq!(json["name"], "pnpm:progress");
    assert_eq!(json["status"], "imported");
    assert_eq!(json["method"], "hardlink");
    assert_eq!(json["requester"], "/proj");
    assert_eq!(json["to"], "/proj/node_modules/.pacquet/react@18.0.0/node_modules/react");
    // `imported` deliberately omits `packageId` — match pnpm's shape
    // so consumers that read `progress.packageId` only on the three
    // store-ish statuses don't trip on a stray field.
    assert!(json.get("packageId").is_none(), "imported must not carry packageId");
}

/// `pnpm:fetching-progress` flattens its two-state `status` enum into
/// the envelope. `started` carries `attempt` / `packageId` / `size`
/// (the `Content-Length`-derived value, serialized as JSON `null`
/// when the response is chunked / unknown); `in_progress` carries the
/// running `downloaded` byte count.
#[test]
fn fetching_progress_event_matches_pnpm_wire_shape() {
    let event = LogEvent::FetchingProgress(FetchingProgressLog {
        level: LogLevel::Debug,
        message: FetchingProgressMessage::Started {
            attempt: 1,
            package_id: "react@18.0.0".to_string(),
            size: Some(123_456),
        },
    });
    let envelope = Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };
    let json: Value = envelope
        .pipe_ref(serde_json::to_string)
        .expect("serialize envelope")
        .pipe_as_ref(serde_json::from_str)
        .expect("parse JSON");
    assert_eq!(json["name"], "pnpm:fetching-progress");
    assert_eq!(json["status"], "started");
    assert_eq!(json["attempt"], 1);
    assert_eq!(json["packageId"], "react@18.0.0");
    assert_eq!(json["size"], 123_456);

    // Unknown / chunked response: `size` must serialize as JSON null,
    // matching pnpm's `size: number | null` shape. The default-reporter
    // checks `size != null` to decide whether to render a percent
    // gauge; emitting an absent field would silently break that.
    let event = LogEvent::FetchingProgress(FetchingProgressLog {
        level: LogLevel::Debug,
        message: FetchingProgressMessage::Started {
            attempt: 0,
            package_id: "react@18.0.0".to_string(),
            size: None,
        },
    });
    let envelope = Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };
    let json: Value = envelope
        .pipe_ref(serde_json::to_string)
        .expect("serialize envelope")
        .pipe_as_ref(serde_json::from_str)
        .expect("parse JSON");
    assert!(json.get("size").is_some_and(serde_json::Value::is_null), "size must be JSON null");

    let event = LogEvent::FetchingProgress(FetchingProgressLog {
        level: LogLevel::Debug,
        message: FetchingProgressMessage::InProgress {
            downloaded: 65_536,
            package_id: "react@18.0.0".to_string(),
        },
    });
    let envelope = Envelope { time: 1_700_000_000_000, hostname: "host", pid: 4242, event: &event };
    let json: Value = envelope
        .pipe_ref(serde_json::to_string)
        .expect("serialize envelope")
        .pipe_as_ref(serde_json::from_str)
        .expect("parse JSON");
    assert_eq!(json["status"], "in_progress");
    assert_eq!(json["downloaded"], 65_536);
    assert_eq!(json["packageId"], "react@18.0.0");
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
