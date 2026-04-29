use super::*;

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
