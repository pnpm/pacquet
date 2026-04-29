use super::*;
use crate::capabilities::{FsReadHead, RealApi};

#[test]
fn parses_env_node_shebang() {
    let rt = parse_shebang("#!/usr/bin/env node").unwrap();
    assert_eq!(rt.prog.as_deref(), Some("node"));
    assert_eq!(rt.args, "");
}

#[test]
fn parses_env_dash_s_shebang() {
    let rt = parse_shebang("#!/usr/bin/env -S node --experimental").unwrap();
    assert_eq!(rt.prog.as_deref(), Some("node"));
    assert_eq!(rt.args, "--experimental");
}

#[test]
fn parses_direct_shebang() {
    let rt = parse_shebang("#!/bin/sh -e").unwrap();
    assert_eq!(rt.prog.as_deref(), Some("/bin/sh"));
    assert_eq!(rt.args, "-e");
}

#[test]
fn rejects_non_shebang_lines() {
    assert!(parse_shebang("just text").is_none());
    assert!(parse_shebang("#! ").is_none());
}

#[test]
fn extension_fallback_picks_node_for_js() {
    assert_eq!(extension_program("js"), Some("node"));
    assert_eq!(extension_program("cjs"), Some("node"));
    assert_eq!(extension_program("mjs"), Some("node"));
}

#[test]
fn relative_target_traverses_into_sibling_package() {
    // shim at .../node_modules/.bin/cli; target at .../node_modules/foo/bin/cli.js
    let target = Path::new("/proj/node_modules/foo/bin/cli.js");
    let shim = Path::new("/proj/node_modules/.bin/cli");
    assert_eq!(relative_target(target, shim), "../foo/bin/cli.js");
}

/// Shim body for the typical `#!/usr/bin/env node` case must match the
/// exec template upstream produces verbatim, including the double space
/// between `$basedir/node` and the quoted target path (upstream's
/// `${args}` interpolates to empty between two literal spaces).
#[test]
fn generate_sh_shim_matches_pnpm_typical_case() {
    let target = Path::new("/proj/node_modules/typescript/bin/tsc");
    let shim = Path::new("/proj/node_modules/.bin/tsc");
    let runtime = ScriptRuntime { prog: Some("node".into()), args: String::new() };
    let body = generate_sh_shim(target, shim, Some(&runtime));

    assert!(body.starts_with("#!/bin/sh\n"), "shebang must come first");
    assert!(
        body.contains("if [ -x \"$basedir/node\" ]; then\n  exec \"$basedir/node\"  \"$basedir/../typescript/bin/tsc\" \"$@\"\nelse\n  exec node  \"$basedir/../typescript/bin/tsc\" \"$@\"\nfi\n"),
        "exec block must match pnpm's generateShShim template, body was:\n{body}",
    );
    assert!(
        body.ends_with("# cmd-shim-target=/proj/node_modules/typescript/bin/tsc\n"),
        "trailing target marker is required for is_shim_pointing_at parity",
    );
}

#[test]
fn is_shim_pointing_at_round_trips_through_marker() {
    let target = Path::new("/p/node_modules/typescript/bin/tsc");
    let shim = Path::new("/p/node_modules/.bin/tsc");
    let runtime = ScriptRuntime { prog: Some("node".into()), args: String::new() };
    let body = generate_sh_shim(target, shim, Some(&runtime));
    assert!(is_shim_pointing_at(&body, target));
    assert!(!is_shim_pointing_at(&body, Path::new("/elsewhere")));
}

/// Every fallback extension upstream's `extensionToProgramMap` recognises
/// must round-trip. Guards against silent regression if an extension is
/// removed from the table.
#[test]
fn extension_program_covers_every_known_extension() {
    assert_eq!(extension_program("js"), Some("node"));
    assert_eq!(extension_program("cjs"), Some("node"));
    assert_eq!(extension_program("mjs"), Some("node"));
    assert_eq!(extension_program("cmd"), Some("cmd"));
    assert_eq!(extension_program("bat"), Some("cmd"));
    assert_eq!(extension_program("ps1"), Some("pwsh"));
    assert_eq!(extension_program("sh"), Some("sh"));
    assert_eq!(extension_program("unknown"), None);
    assert_eq!(extension_program(""), None);
}

/// `parse_shebang` returns None when the line lacks `#!` entirely (handled
/// elsewhere) or when it's `#!` with only whitespace after — the
/// `prog.is_empty()` guard.
#[test]
fn parse_shebang_returns_none_for_empty_prog() {
    assert!(parse_shebang("#!\t").is_none());
    assert!(parse_shebang("#!").is_none(), "empty line after #! must yield None");
    assert!(parse_shebang("not a shebang").is_none());
}

/// `parse_shebang_from_bytes` is the byte-level entry. It must trim a
/// leading BOM-free CRLF first line and survive non-UTF-8 bytes (lossy
/// decoding).
#[test]
fn parse_shebang_from_bytes_handles_crlf_and_lossy_utf8() {
    let bytes = b"#!/usr/bin/env node\r\nconsole.log('hi')\n";
    let rt = parse_shebang_from_bytes(bytes).expect("CRLF first line");
    assert_eq!(rt.prog.as_deref(), Some("node"));

    // Non-UTF-8 bytes after the shebang must not break parsing.
    let mut bytes = Vec::from(*b"#!/usr/bin/env node\n");
    bytes.extend_from_slice(&[0xff, 0xfe, 0xfd]);
    let rt = parse_shebang_from_bytes(&bytes).expect("non-UTF-8 tail tolerated");
    assert_eq!(rt.prog.as_deref(), Some("node"));
}

/// `generate_sh_shim` with `runtime: None` emits the `exit $?` arm that
/// upstream uses when no interpreter could be inferred. The shim execs the
/// target directly.
#[test]
fn generate_sh_shim_emits_direct_exec_when_no_runtime() {
    let target = Path::new("/proj/node_modules/foo/bin/cli");
    let shim = Path::new("/proj/node_modules/.bin/cli");
    let body = generate_sh_shim(target, shim, None);
    assert!(
        body.contains("\"$basedir/../foo/bin/cli\"  \"$@\"\nexit $?\n"),
        "no-runtime arm must exec the target directly, body:\n{body}",
    );
    assert!(body.ends_with("# cmd-shim-target=/proj/node_modules/foo/bin/cli\n"));
}

/// `generate_sh_shim` with `runtime: Some(.. prog: None ..)` uses the same
/// no-runtime arm, but threading the explicit `args`. Mirrors upstream's
/// fallback when `prog` couldn't be inferred but the runtime probe still
/// returned a `ScriptRuntime { prog: None, args }`.
#[test]
fn generate_sh_shim_threads_args_when_prog_is_none() {
    let target = Path::new("/p/cli");
    let shim = Path::new("/p/.bin/cli");
    let runtime = ScriptRuntime { prog: None, args: "--flag".to_string() };
    let body = generate_sh_shim(target, shim, Some(&runtime));
    assert!(
        body.contains("\"$basedir/../cli\" --flag \"$@\"\nexit $?\n"),
        "args must be threaded into the no-prog arm, body:\n{body}",
    );
}

/// `generate_sh_shim` with a target that lexically resolves to an absolute
/// path takes the `path::isAbsolute(shTarget)` branch upstream uses — the
/// quoted target stays absolute and skips the `$basedir/` prefix.
///
/// Unix-only: a path like `/abs/elsewhere/cli` is "absolute" only on Unix.
/// On Windows, `Path::is_absolute()` requires a drive letter (e.g.
/// `C:\abs\...`), so the same input takes the relative branch. The shim
/// produced by pacquet is a `/bin/sh` script regardless of host platform,
/// but the absolute-vs-relative classification of bin paths is itself
/// platform-dependent — this test pins behavior on Unix only.
#[cfg(unix)]
#[test]
fn generate_sh_shim_uses_absolute_target_when_no_common_prefix() {
    // `relative_path_from` of two paths with no common root produces an
    // absolute-ish path that still starts with `/` once joined; force the
    // absolute branch by constructing a target that's absolute and a shim
    // whose parent is empty.
    let target = Path::new("/abs/elsewhere/cli");
    let shim = Path::new("local-shim");
    let runtime = ScriptRuntime { prog: Some("node".into()), args: String::new() };
    let body = generate_sh_shim(target, shim, Some(&runtime));
    assert!(
        body.contains("\"/abs/elsewhere/cli\""),
        "absolute-target branch must skip $basedir prefix, body:\n{body}",
    );
}

/// `relative_target` of `from == to_parent` collapses to `.` (the
/// `result.is_empty()` branch in `relative_path_from`).
#[test]
fn relative_target_collapses_to_dot_when_paths_share_dir() {
    let target = Path::new("/proj/.bin/cli");
    let shim = Path::new("/proj/.bin/wrapper");
    assert_eq!(relative_target(target, shim), "cli");
}

/// `relative_path_from` with leading `..` segments must preserve them
/// (cannot pop past root). Exercises the `out.push("..")` fallback in
/// `lexical_normalize`.
#[test]
fn lexical_normalize_keeps_leading_parent_segments() {
    let target = Path::new("../../shared/cli");
    let shim = Path::new("project/.bin/cli");
    let result = relative_target(target, shim);
    assert!(result.contains("shared/cli"), "leading `..` must propagate, got: {result}",);
}

/// `lexical_normalize` discards `.` (CurDir) components silently. Verify
/// via `relative_target` — a target with embedded `./` resolves the same
/// as without.
#[test]
fn lexical_normalize_drops_curdir_components() {
    let with_dot = relative_target(Path::new("/p/foo/./cli"), Path::new("/p/.bin/x"));
    let without_dot = relative_target(Path::new("/p/foo/cli"), Path::new("/p/.bin/x"));
    assert_eq!(with_dot, without_dot);
}

/// `search_script_runtime` reads a real file with a shebang and returns
/// the prog from it. End-to-end of the production path.
#[test]
fn search_script_runtime_reads_shebang_from_real_file() {
    use tempfile::tempdir;
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("script");
    std::fs::write(&path, "#!/usr/bin/env node\nbody\n").unwrap();
    let rt = search_script_runtime::<RealApi>(&path).unwrap().expect("runtime detected");
    assert_eq!(rt.prog.as_deref(), Some("node"));
}

/// `search_script_runtime` on a missing file must degrade to `Ok(None)`
/// — the install otherwise races against bin file extraction.
#[test]
fn search_script_runtime_returns_none_for_missing_file() {
    let nonexistent = Path::new("/definitely/not/a/real/path/cli");
    assert_eq!(search_script_runtime::<RealApi>(nonexistent).unwrap(), None);
}

/// `search_script_runtime` falls through to extension lookup when the
/// file has no shebang. A `.js` file without `#!` must still resolve to
/// `node`.
#[test]
fn search_script_runtime_falls_back_to_extension() {
    use tempfile::tempdir;
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("script.js");
    std::fs::write(&path, "console.log('no shebang')\n").unwrap();
    let rt = search_script_runtime::<RealApi>(&path).unwrap().expect("extension fallback");
    assert_eq!(rt.prog.as_deref(), Some("node"));
}

/// `search_script_runtime` returns `Ok(None)` when neither shebang nor
/// extension yields a runtime. Pure no-runtime path.
#[test]
fn search_script_runtime_returns_none_when_runtime_unknown() {
    use tempfile::tempdir;
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("script.unknown_ext");
    std::fs::write(&path, "no shebang here\n").unwrap();
    assert_eq!(search_script_runtime::<RealApi>(&path).unwrap(), None);
}

/// `search_script_runtime` propagates IO errors that aren't `NotFound`.
/// Real-fs can't trigger e.g. `PermissionDenied` portably, so plug a
/// fake `FsReadHead` per the DI principles in
/// <https://github.com/pnpm/pacquet/pull/332#issuecomment-4345054524>.
#[test]
fn search_script_runtime_propagates_non_not_found_io_errors() {
    struct PermissionDeniedApi;
    impl FsReadHead for PermissionDeniedApi {
        fn read_head(_: &Path, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    }
    let err = search_script_runtime::<PermissionDeniedApi>(Path::new("any"))
        .expect_err("non-NotFound IO error must propagate");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
}

/// A `FsReadHead` that returns 0 bytes (empty file) yields no shebang —
/// the parse step then falls through to the extension fallback. Pin the
/// behavior so a future tweak to the empty-buffer handling stays
/// compatible with the no-shebang case.
#[test]
fn search_script_runtime_reads_zero_bytes_then_falls_through() {
    struct EmptyReadApi;
    impl FsReadHead for EmptyReadApi {
        fn read_head(_: &Path, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    // `.js` extension still resolves to `node` even with empty content.
    let rt =
        search_script_runtime::<EmptyReadApi>(Path::new("/x.js")).unwrap().expect("ext fallback");
    assert_eq!(rt.prog.as_deref(), Some("node"));

    // No extension and no shebang → Ok(None).
    let rt = search_script_runtime::<EmptyReadApi>(Path::new("/x")).unwrap();
    assert_eq!(rt, None);
}

/// `RealApi::read_head` is the production capability. Tests that exercise
/// it indirectly cover most paths; this one pins the contract directly.
#[test]
fn real_fs_read_head_reads_up_to_buffer_size() {
    use tempfile::tempdir;
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("data");
    std::fs::write(&path, "hello world").unwrap();
    let mut buf = [0u8; 1024];
    let read = RealApi::read_head(&path, &mut buf).unwrap();
    assert_eq!(read, 11);
    assert_eq!(&buf[..read], b"hello world");
}

/// `RealApi::read_head` propagates `NotFound` so the shebang reader can
/// distinguish a missing file from a real IO error and degrade to
/// `Ok(None)`.
#[test]
fn real_fs_read_head_propagates_not_found() {
    let mut buf = [0u8; 16];
    let err = RealApi::read_head(Path::new("/no/such/file"), &mut buf).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

/// `generate_cmd_shim` produces a Windows `.cmd` shim with CRLF line
/// endings, `%~dp0\<rel>` for the target, and the
/// `@IF EXIST … (… ) ELSE ( @SET PATHEXT=... … )` exec block matching
/// upstream's template.
#[test]
fn generate_cmd_shim_matches_pnpm_template() {
    let target = Path::new("/proj/node_modules/typescript/bin/tsc");
    let shim = Path::new("/proj/node_modules/.bin/tsc.cmd");
    let runtime = ScriptRuntime { prog: Some("node".into()), args: String::new() };
    let body = generate_cmd_shim(target, shim, Some(&runtime));

    assert!(body.starts_with("@SETLOCAL\r\n"), "must start with @SETLOCAL CRLF");
    assert!(
        body.contains("@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\..\\typescript\\bin\\tsc\" %*\r\n) ELSE (\r\n  @SET PATHEXT=%PATHEXT:;.JS;=;%\r\n  node  \"%~dp0\\..\\typescript\\bin\\tsc\" %*\r\n)\r\n"),
        "exec block must match pnpm's generateCmdShim template, body was:\n{body}",
    );
}

/// `generate_cmd_shim` with no runtime exec's the target directly via
/// the `@<target> %*` shape.
#[test]
fn generate_cmd_shim_emits_direct_exec_when_no_runtime() {
    let target = Path::new("/p/cli");
    let shim = Path::new("/p/.bin/cli.cmd");
    let body = generate_cmd_shim(target, shim, None);
    assert!(
        body.contains("@\"%~dp0\\..\\cli\""),
        "no-runtime arm must exec the target directly, body:\n{body}",
    );
}

/// `generate_pwsh_shim` produces a `.ps1` shim with the `$basedir`
/// header, `Test-Path "$basedir/<prog>$exe"` exec block, and pipeline-
/// input handling matching upstream.
#[test]
fn generate_pwsh_shim_matches_pnpm_template() {
    let target = Path::new("/proj/node_modules/typescript/bin/tsc");
    let shim = Path::new("/proj/node_modules/.bin/tsc.ps1");
    let runtime = ScriptRuntime { prog: Some("node".into()), args: String::new() };
    let body = generate_pwsh_shim(target, shim, Some(&runtime));

    assert!(body.starts_with("#!/usr/bin/env pwsh\n"), "ps1 shim must start with pwsh shebang");
    assert!(
        body.contains("$basedir=Split-Path $MyInvocation.MyCommand.Definition -Parent"),
        "must declare $basedir from MyInvocation",
    );
    assert!(body.contains("$exe=\".exe\""), "Windows-detection branch must set $exe to .exe");
    assert!(
        body.contains(
            "if (Test-Path \"$basedir/node$exe\") {\n  # Support pipeline input\n  if ($MyInvocation.ExpectingInput) {\n    $input | & \"$basedir/node$exe\"  \"$basedir/../typescript/bin/tsc\" $args\n  } else {\n    & \"$basedir/node$exe\"  \"$basedir/../typescript/bin/tsc\" $args\n  }",
        ),
        "exec-with-basedir-prog block must match pnpm's generatePwshShim template, body was:\n{body}",
    );
    assert!(body.ends_with("exit $ret\n"));
}

/// `generate_pwsh_shim` with no runtime falls back to executing the
/// target directly with `$LASTEXITCODE` propagation.
#[test]
fn generate_pwsh_shim_emits_direct_exec_when_no_runtime() {
    let target = Path::new("/p/cli");
    let shim = Path::new("/p/.bin/cli.ps1");
    let body = generate_pwsh_shim(target, shim, None);
    assert!(
        body.contains("& \"$basedir/../cli\""),
        "no-runtime arm must exec the target directly, body:\n{body}",
    );
    assert!(body.ends_with("exit $LASTEXITCODE\n"));
}
