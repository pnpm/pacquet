use super::emit_progress_resolved;
use pacquet_reporter::{LogEvent, ProgressMessage, Reporter};
use std::sync::Mutex;

/// `emit_progress_resolved` fires exactly one `pnpm:progress`
/// `resolved` event with the supplied (`package_id`, `requester`).
/// The pair pins pnpm's per-package counter to the right row.
#[test]
fn emits_resolved_with_supplied_identifiers() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().unwrap().push(event.clone());
        }
    }

    EVENTS.lock().unwrap().clear();
    emit_progress_resolved::<RecordingReporter>("react@18.0.0", "/proj");

    let captured = EVENTS.lock().unwrap();
    assert!(
        matches!(
            captured.as_slice(),
            [LogEvent::Progress(log)] if matches!(
                &log.message,
                ProgressMessage::Resolved { package_id, requester }
                    if package_id == "react@18.0.0" && requester == "/proj"
            )
        ),
        "expected a single Resolved event with matching identifiers; got {captured:?}",
    );
}
