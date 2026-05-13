use super::{emit_progress_resolved, host_platform_selector, render_variant_targets};
use pacquet_graph_hasher::{host_arch, host_libc, host_platform};
use pacquet_lockfile::{LockfileResolution, PlatformAssetResolution, PlatformAssetTarget};
use pacquet_reporter::{LogEvent, ProgressMessage, Reporter};
use pretty_assertions::assert_eq;
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
                    if package_id == "react@18.0.0" && requester == "/proj",
            ),
        ),
        "expected a single Resolved event with matching identifiers; got {captured:?}",
    );
}

/// `host_platform_selector` builds the selector that drives runtime-
/// variant matching. The `os` / `cpu` fields are always populated
/// (from `host_platform()` / `host_arch()`); `libc` is the
/// interesting one — pacquet must translate the
/// "non-Linux ⇒ no libc constraint" rule pnpm enforces:
/// `process.platform === 'linux' ? family : null`.
///
/// Asserting platform-specific shape directly would mean four
/// `cfg`-gated tests; instead, run the live `host_*` functions and
/// pin the *relationship* — `host_libc() == "unknown"` iff the
/// selector's `libc` field is `None`. The relationship covers both
/// the macOS / Windows / BSD non-Linux case (`libc` always `None`)
/// and the Linux case (`libc` always `Some("glibc")` /
/// `Some("musl")`).
#[test]
fn host_platform_selector_omits_libc_on_non_linux_hosts() {
    let selector = host_platform_selector();
    let libc_known = host_libc() != "unknown";
    assert_eq!(selector.os, host_platform());
    assert_eq!(selector.cpu, host_arch());
    assert_eq!(
        selector.libc.is_some(),
        libc_known,
        "selector.libc should be Some iff host_libc() reports glibc/musl (Linux); got selector={selector:?}, host_libc={:?}",
        host_libc(),
    );
    if libc_known {
        assert_eq!(selector.libc.as_deref(), Some(host_libc()));
    }
}

/// `render_variant_targets` renders the lockfile's advertised
/// target triples for inclusion in the
/// `NoMatchingPlatformVariant` error message. Each target lands as
/// `os/cpu` with an optional `+libc` suffix, joined with `, ` so
/// the rendered list is greppable from terminal output.
#[test]
fn render_variant_targets_formats_each_triple_with_optional_libc() {
    let variants = vec![
        PlatformAssetResolution {
            // Inner resolution is unused by the renderer; pick any
            // shape that round-trips through serde (Directory keeps
            // the fixture light).
            resolution: LockfileResolution::Directory(pacquet_lockfile::DirectoryResolution {
                directory: "fixture".into(),
            }),
            targets: vec![
                PlatformAssetTarget { os: "darwin".into(), cpu: "arm64".into(), libc: None },
                PlatformAssetTarget {
                    os: "linux".into(),
                    cpu: "x64".into(),
                    libc: Some("musl".into()),
                },
            ],
        },
        PlatformAssetResolution {
            resolution: LockfileResolution::Directory(pacquet_lockfile::DirectoryResolution {
                directory: "fixture".into(),
            }),
            targets: vec![PlatformAssetTarget {
                os: "win32".into(),
                cpu: "x64".into(),
                libc: None,
            }],
        },
    ];

    let rendered = render_variant_targets(&variants);
    assert_eq!(rendered, "darwin/arm64, linux/x64+musl, win32/x64");
}
