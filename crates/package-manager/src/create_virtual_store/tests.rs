use super::emit_warm_snapshot_progress;
use pacquet_reporter::{LogEvent, ProgressMessage, Reporter};
use std::sync::Mutex;

/// `emit_warm_snapshot_progress` fires `resolved` then
/// `found_in_store` in that order for one (package_id, requester)
/// pair. Both events carry the same identifiers — pnpm's per-package
/// counter relies on the pair to pin the tick to the right package
/// row.
#[test]
fn emits_resolved_then_found_in_store_with_matching_identifiers() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().unwrap().push(event.clone());
        }
    }

    EVENTS.lock().unwrap().clear();
    emit_warm_snapshot_progress::<RecordingReporter>("react@18.0.0", "/proj");

    let captured = EVENTS.lock().unwrap();
    assert!(
        matches!(
            captured.as_slice(),
            [
                LogEvent::Progress(r),
                LogEvent::Progress(f),
            ] if matches!(
                &r.message,
                ProgressMessage::Resolved { package_id, requester }
                    if package_id == "react@18.0.0" && requester == "/proj"
            ) && matches!(
                &f.message,
                ProgressMessage::FoundInStore { package_id, requester }
                    if package_id == "react@18.0.0" && requester == "/proj"
            )
        ),
        "warm-snapshot pair must be (Resolved, FoundInStore) with matching identifiers; got {captured:?}",
    );
}
