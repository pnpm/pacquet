use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// One bin entry resolved from a package's `package.json`.
///
/// `name` is the command name as it should appear under `node_modules/.bin/`.
/// `path` is the absolute path to the script the shim invokes.
///
/// Mirrors `Command` in pnpm v11's `@pnpm/bins.resolver`:
/// <https://github.com/pnpm/pnpm/blob/4750fd370c/bins/resolver/src/index.ts>.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub path: PathBuf,
}

/// Bin names that legitimately ship inside a different package than their own
/// name. Mirrors `BIN_OWNER_OVERRIDES` in
/// <https://github.com/pnpm/pnpm/blob/4750fd370c/bins/resolver/src/index.ts>.
///
/// Used by [`pkg_owns_bin`] for conflict resolution between two packages
/// declaring the same bin name.
const BIN_OWNER_OVERRIDES: &[(&str, &[&str])] = &[
    ("npx", &["npm"]),
    ("pn", &["pnpm", "@pnpm/exe"]),
    ("pnpm", &["@pnpm/exe"]),
    ("pnpx", &["pnpm", "@pnpm/exe"]),
    ("pnx", &["pnpm", "@pnpm/exe"]),
];

/// Whether `pkg_name` is a legitimate owner of the given `bin_name`. The
/// default rule is "the package named `X` owns the `X` bin"; overrides cover
/// cases like `npx` shipping inside `npm`. Mirrors `pkgOwnsBin`.
pub fn pkg_owns_bin(bin_name: &str, pkg_name: &str) -> bool {
    if bin_name == pkg_name {
        return true;
    }
    BIN_OWNER_OVERRIDES
        .iter()
        .find(|(name, _)| *name == bin_name)
        .is_some_and(|(_, owners)| owners.contains(&pkg_name))
}

/// Read every bin declared by `manifest` and return them as [`Command`]s
/// rooted at `pkg_path`.
///
/// Handles the three cases pnpm supports, in order:
///
/// 1. `bin` as a string. The bin name is the package's own `name` (with any
///    `@scope/` prefix stripped). Empty / missing `name` skips the entry, in
///    parity with pnpm's `INVALID_PACKAGE_NAME` guard.
/// 2. `bin` as an object. Each `(commandName, relativePath)` becomes a
///    command, with `@scope/` stripped from the key.
/// 3. Fallback: `directories.bin` — every regular file under the directory
///    becomes a command. Pacquet's first iteration omits this path; pnpm tests
///    that exercise it are listed in `plans/TEST_PORTING.md`.
///
/// Validation, exactly mirroring pnpm:
///
/// - Bin name must be URL-safe (`name == encodeURIComponent(name)`) or be the
///   single-character `$`. This is the path-traversal guard.
/// - Bin path must resolve under `pkg_path` (`is_subdir`). Prevents a
///   malicious manifest from writing shims that exec a sibling package.
pub fn get_bins_from_package_manifest(manifest: &Value, pkg_path: &Path) -> Vec<Command> {
    let pkg_name = manifest.get("name").and_then(Value::as_str);
    if let Some(bin) = manifest.get("bin") {
        return commands_from_bin(bin, pkg_name, pkg_path);
    }
    // `directories.bin` deferred — see the module-level note. Returning an
    // empty list matches pnpm's behavior when neither `bin` nor an existing
    // `directories.bin` is present.
    Vec::new()
}

fn commands_from_bin(bin: &Value, pkg_name: Option<&str>, pkg_path: &Path) -> Vec<Command> {
    let mut entries: Vec<(String, String)> = Vec::new();
    match bin {
        Value::String(rel_path) => {
            let Some(name) = pkg_name else {
                return Vec::new();
            };
            entries.push((name.to_string(), rel_path.clone()));
        }
        Value::Object(map) => {
            for (key, value) in map {
                let Some(rel_path) = value.as_str() else {
                    continue;
                };
                entries.push((key.clone(), rel_path.to_string()));
            }
        }
        _ => return Vec::new(),
    }

    let mut commands = Vec::with_capacity(entries.len());
    for (command_name, bin_relative_path) in entries {
        // Strip any `@scope/` prefix. Mirrors `commandsFromBin`'s
        // `commandName[0] === '@'` branch.
        let bin_name = if command_name.starts_with('@') {
            match command_name.find('/') {
                Some(slash) => command_name[slash + 1..].to_string(),
                None => command_name,
            }
        } else {
            command_name
        };

        if !is_safe_bin_name(&bin_name) {
            continue;
        }

        let bin_path = pkg_path.join(&bin_relative_path);
        if !is_subdir(pkg_path, &bin_path) {
            continue;
        }

        commands.push(Command { name: bin_name, path: bin_path });
    }
    commands
}

/// Whether `name` matches the URL-safe character set allowed by JavaScript's
/// `encodeURIComponent`, or is the single-character escape hatch `$` pnpm
/// permits for awkward but legitimate bin names. Together these are the only
/// names pnpm allows the linker to write to disk.
///
/// `encodeURIComponent` leaves the following bytes unescaped:
/// `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
fn is_safe_bin_name(name: &str) -> bool {
    if name == "$" {
        return true;
    }
    if name.is_empty() {
        return false;
    }
    name.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
    })
}

/// Whether `child` resolves to a path under `parent`, after lexically
/// normalising `..` segments. Mirrors `isSubdir(pkgPath, binPath)` from pnpm's
/// `is-subdir`. We deliberately do not canonicalize via the filesystem — the
/// guard runs before the bin file exists at its final location, and pnpm's
/// implementation is purely lexical too.
fn is_subdir(parent: &Path, child: &Path) -> bool {
    let parent_norm = lexical_normalize(parent);
    let child_norm = lexical_normalize(child);
    child_norm.starts_with(&parent_norm)
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
    use serde_json::json;

    #[test]
    fn bin_as_string_uses_package_name() {
        let manifest = json!({"name": "foo", "bin": "cli.js"});
        let commands = get_bins_from_package_manifest(&manifest, Path::new("/pkg/foo"));
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "foo");
        assert_eq!(commands[0].path, Path::new("/pkg/foo/cli.js"));
    }

    #[test]
    fn bin_as_string_strips_scope() {
        let manifest = json!({"name": "@scope/foo", "bin": "cli.js"});
        let commands = get_bins_from_package_manifest(&manifest, Path::new("/pkg/foo"));
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "foo");
    }

    #[test]
    fn bin_as_object_keeps_keys_and_strips_scope() {
        let manifest = json!({
            "name": "tool",
            "bin": {
                "tool": "bin/tool.js",
                "@scope/extra": "bin/extra.js",
            },
        });
        let mut commands = get_bins_from_package_manifest(&manifest, Path::new("/p"));
        commands.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].name, "extra");
        assert_eq!(commands[1].name, "tool");
    }

    #[test]
    fn rejects_unsafe_bin_names() {
        let manifest = json!({
            "name": "x",
            "bin": {
                "good-name": "ok.js",
                "../bad": "evil.js",
                "with space": "no.js",
                "$": "dollar.js",
            },
        });
        let mut names: Vec<_> = get_bins_from_package_manifest(&manifest, Path::new("/p"))
            .into_iter()
            .map(|c| c.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["$".to_string(), "good-name".to_string()]);
    }

    #[test]
    fn rejects_path_traversal_outside_package_root() {
        let manifest = json!({
            "name": "x",
            "bin": {"x": "../../../etc/passwd"},
        });
        let commands = get_bins_from_package_manifest(&manifest, Path::new("/pkg/x"));
        assert!(commands.is_empty(), "must reject `..`-escapes from pkg root");
    }

    #[test]
    fn no_bin_field_returns_empty() {
        let manifest = json!({"name": "x"});
        assert!(get_bins_from_package_manifest(&manifest, Path::new("/p")).is_empty());
    }

    #[test]
    fn pkg_owns_bin_default_rule() {
        assert!(pkg_owns_bin("foo", "foo"));
        assert!(!pkg_owns_bin("foo", "bar"));
    }

    #[test]
    fn pkg_owns_bin_overrides() {
        assert!(pkg_owns_bin("npx", "npm"));
        assert!(pkg_owns_bin("pnpx", "pnpm"));
        assert!(pkg_owns_bin("pnpx", "@pnpm/exe"));
        assert!(!pkg_owns_bin("npx", "anything-else"));
    }
}
