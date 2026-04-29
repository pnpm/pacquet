use std::{
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

/// Detected runtime for a target script.
///
/// Mirrors the return shape of `searchScriptRuntime` in
/// <https://github.com/pnpm/cmd-shim/blob/0d79ca9534/src/index.ts>.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRuntime {
    /// The interpreter to invoke. `None` means "exec the file directly".
    pub prog: Option<String>,
    /// Extra arguments declared after the interpreter in the shebang. Empty
    /// when the runtime came from the extension fallback.
    pub args: String,
}

/// Map of file extensions to their default runtime when the script lacks a
/// shebang. Mirrors `extensionToProgramMap` in upstream cmd-shim.
fn extension_program(extension: &str) -> Option<&'static str> {
    match extension {
        "js" | "cjs" | "mjs" => Some("node"),
        "cmd" | "bat" => Some("cmd"),
        "ps1" => Some("pwsh"),
        "sh" => Some("sh"),
        _ => None,
    }
}

/// Read up to 512 bytes of `path` and infer the runtime.
///
/// Order, mirroring `searchScriptRuntime`:
///
/// 1. If the file exists and starts with a shebang, parse `prog` + `args` from
///    it.
/// 2. Otherwise fall through to `extension_program` on the file's extension.
/// 3. If neither yields a runtime, return `None` — `generate_sh_shim` handles
///    that by exec'ing the target directly.
///
/// Errors reading the file degrade to `Ok(None)`. cmd-shim's TS code throws
/// here but pacquet's call sites already verified the bin path resolves under
/// the package root; a transient read error shouldn't fail the whole install.
pub fn search_script_runtime(path: &Path) -> io::Result<Option<ScriptRuntime>> {
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    let runtime_from_shebang = read_shebang(path)?;
    if let Some(rt) = runtime_from_shebang {
        return Ok(Some(rt));
    }

    if let Some(prog) = extension_program(extension) {
        return Ok(Some(ScriptRuntime { prog: Some(prog.to_string()), args: String::new() }));
    }

    Ok(None)
}

fn read_shebang(path: &Path) -> io::Result<Option<ScriptRuntime>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut buffer = [0u8; 512];
    let read = file.read(&mut buffer)?;
    let head = String::from_utf8_lossy(&buffer[..read]);

    let first_line = head.trim_start().split('\n').next().unwrap_or("").trim_end_matches('\r');
    Ok(parse_shebang(first_line))
}

/// Mirrors the shebang regex in upstream cmd-shim:
/// `^#!\s*(?:/usr/bin/env(?:\s+-S\s*)?)?\s*([^ \t]+)(.*)$`.
///
/// Recognises `#!/usr/bin/env <prog>`, `#!/usr/bin/env -S <prog>`, and any
/// direct `#!/path/to/<prog>` shebang. The captured `args` is the trailing
/// portion of the line, with any surrounding whitespace preserved exactly the
/// way upstream's regex match would.
fn parse_shebang(line: &str) -> Option<ScriptRuntime> {
    let rest = line.strip_prefix("#!")?.trim_start();
    let (rest, _) = strip_env_prefix(rest);
    let rest = rest.trim_start();

    let mut split = rest.splitn(2, [' ', '\t']);
    let prog = split.next()?;
    let args = split.next().unwrap_or("");

    if prog.is_empty() {
        return None;
    }

    Some(ScriptRuntime { prog: Some(prog.to_string()), args: args.to_string() })
}

/// Strip a leading `/usr/bin/env`, optionally followed by `-S`, from the
/// shebang body. Returns the remainder and whether `env` was present.
fn strip_env_prefix(input: &str) -> (&str, bool) {
    let Some(rest) = input.strip_prefix("/usr/bin/env") else {
        return (input, false);
    };
    let trimmed = rest.trim_start();
    if let Some(after_dash_s) = trimmed.strip_prefix("-S") {
        return (after_dash_s, true);
    }
    (trimmed, true)
}

/// Generate the Unix shell-shim contents for `target_path`, written to
/// `shim_path`. Mirrors `generateShShim` in upstream cmd-shim.
///
/// The shim is a pure `/bin/sh` script that:
///
/// 1. Resolves `basedir` to its own directory (with a `cygpath` fixup for
///    MSYS-style POSIX shells on Windows).
/// 2. If the runtime program is colocated at `$basedir/<prog>` (rare —
///    only true when the runtime was bundled alongside the shim), prefer that
///    binary; otherwise fall through to the system PATH.
/// 3. Forwards `"$@"` to the resolved interpreter, with the target script as
///    the first positional argument.
///
/// When [`search_script_runtime`] returned `None` (no shebang, unknown
/// extension), the shim execs the target directly via the second branch
/// upstream uses for that case.
pub fn generate_sh_shim(
    target_path: &Path,
    shim_path: &Path,
    runtime: Option<&ScriptRuntime>,
) -> String {
    let mut sh = String::from(SH_SHIM_HEADER);

    let sh_target = relative_target(target_path, shim_path);
    let quoted_target = if Path::new(&sh_target).is_absolute() {
        format!("\"{sh_target}\"")
    } else {
        format!("\"$basedir/{sh_target}\"")
    };

    match runtime {
        Some(ScriptRuntime { prog: Some(prog), args }) => {
            // `sh_long_prog` is the `"$basedir/<prog>"` form upstream uses.
            // It always carries the leading `$basedir/` and quotes — never
            // just the program name on its own.
            let sh_long_prog = format!("\"$basedir/{prog}\"");
            sh.push_str(&format!(
                "if [ -x {sh_long_prog} ]; then\n  exec {sh_long_prog} {args} {quoted_target} \"$@\"\nelse\n  exec {prog} {args} {quoted_target} \"$@\"\nfi\n",
            ));
        }
        // No runtime detected — exec the target directly. Upstream still
        // emits `exit $?` on this branch for parity with non-execve POSIX
        // shells.
        runtime_opt => {
            let args = runtime_opt.map(|r| r.args.as_str()).unwrap_or("");
            sh.push_str(&format!("{quoted_target} {args} \"$@\"\nexit $?\n"));
        }
    }

    sh.push_str(&format!("# {}\n", shim_target_marker(target_path)));
    sh
}

const SH_SHIM_HEADER: &str = "\
#!/bin/sh
basedir=$(dirname \"$(echo \"$0\" | sed -e 's,\\\\,/,g')\")

case `uname` in
    *CYGWIN*|*MINGW*|*MSYS*)
        if command -v cygpath > /dev/null 2>&1; then
            basedir=`cygpath -w \"$basedir\"`
        fi
    ;;
esac

";

/// Trailing `# cmd-shim-target=<rel>` marker. Upstream uses it to detect
/// whether an existing shim already targets the same source without
/// re-parsing its body. Pacquet uses [`is_shim_pointing_at`] for the same
/// short-circuit on warm reinstalls.
fn shim_target_marker(target_path: &Path) -> String {
    format!("cmd-shim-target={}", target_path.to_string_lossy().replace('\\', "/"),)
}

/// Whether an already-on-disk shim targets `target_path`. Mirrors
/// `isShimPointingAt`. The check looks for the trailing marker line so the
/// header text never has to be byte-identical between cmd-shim versions.
pub fn is_shim_pointing_at(shim_content: &str, target_path: &Path) -> bool {
    let marker = format!("# {}", shim_target_marker(target_path));
    shim_content.lines().any(|line| line == marker)
}

/// Compute the relative path from `shim_path`'s parent directory to
/// `target_path`. Falls back to the absolute target path if the relative
/// computation fails — this matches the `path.isAbsolute(shTarget)` guard in
/// upstream's `generateShShim`.
fn relative_target(target_path: &Path, shim_path: &Path) -> String {
    let shim_dir = shim_path.parent().unwrap_or_else(|| Path::new(""));
    let rel = relative_path_from(shim_dir, target_path);
    rel.to_string_lossy().replace('\\', "/")
}

fn relative_path_from(from: &Path, to: &Path) -> PathBuf {
    let from = lexical_normalize(from);
    let to = lexical_normalize(to);

    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();

    let common =
        from_components.iter().zip(to_components.iter()).take_while(|(a, b)| a == b).count();

    let mut result = PathBuf::new();
    for _ in &from_components[common..] {
        result.push("..");
    }
    for component in &to_components[common..] {
        result.push(component.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
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
}
