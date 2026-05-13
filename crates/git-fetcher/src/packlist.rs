//! Decide which files inside an extracted git-hosted package end up in
//! the CAS. MVP port of [`npm-packlist`](https://github.com/npm/npm-packlist)
//! / pnpm's
//! [`fs/packlist`](https://github.com/pnpm/pnpm/blob/94240bc046/fs/packlist/src/index.ts):
//!
//! - When `manifest.files` is a non-empty array, only its glob entries
//!   plus the always-included files (`package.json`, `README*`,
//!   `LICEN[SC]E*`, `CHANGES*`, `CHANGELOG*`, `HISTORY*`, `NOTICE*`)
//!   and any file referenced by `manifest.main` / `manifest.bin` make it
//!   into the result.
//! - When `manifest.files` is absent, every regular file under
//!   `pkg_dir` is included *except* the always-excluded set
//!   (`.git/`, `node_modules/`, lockfiles for other package managers,
//!   common cruft like `.DS_Store` / `npm-debug.log` / `*.orig`).
//!
//! Glob support is limited to `*`, `**`, and `?`. Full
//! `.npmignore` / `.gitignore` layering, `bundleDependencies` walking,
//! and exotic glob features (negation, character classes) are deferred
//! to a follow-up; the gap is logged at `tracing::warn!` when a
//! manifest carries `bundleDependencies` so the omission is visible
//! during install. Tracks pnpm's behavior at
//! [`fs/packlist/src/index.ts:24-29`](https://github.com/pnpm/pnpm/blob/94240bc046/fs/packlist/src/index.ts#L24-L29).

use crate::error::PacklistError;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

/// Filenames always included regardless of `manifest.files`, matching
/// `npm-packlist`'s `alwaysIncluded` plus pnpm's
/// [`packlist`](https://github.com/pnpm/pnpm/blob/94240bc046/fs/packlist/src/index.ts#L13)
/// pattern set. Case-insensitive prefix matches.
const ALWAYS_INCLUDED_PREFIXES: &[&str] =
    &["readme", "license", "licence", "changes", "changelog", "history", "notice"];

/// Files / directories always excluded from the published view. The
/// repo's own lockfiles must not bleed into the consumer's install —
/// only the dep's source/build artifacts should ship.
const ALWAYS_EXCLUDED_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    ".npmrc",
    "npm-debug.log",
    ".DS_Store",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    ".svn",
    "CVS",
    ".hg",
];

/// File-name suffixes always excluded (matched case-sensitively on the
/// basename, like `npm-packlist` does for the `*.orig` family).
const ALWAYS_EXCLUDED_SUFFIXES: &[&str] = &[".orig"];

/// Walk `pkg_dir` and return forward-slash relative paths for every
/// file the published tarball should contain. Mirrors the return shape
/// of `packlist()` at
/// [`fs/packlist/src/index.ts:24-29`](https://github.com/pnpm/pnpm/blob/94240bc046/fs/packlist/src/index.ts#L24-L29)
/// (paths relative to `pkgDir`, no leading `./`).
pub fn packlist(pkg_dir: &Path, manifest: &Value) -> Result<Vec<String>, PacklistError> {
    if let Some(bundle) =
        manifest.get("bundleDependencies").or_else(|| manifest.get("bundledDependencies"))
        && bundle.as_array().is_some_and(|a| !a.is_empty())
    {
        tracing::warn!(
            target: "pacquet::git_fetcher::packlist",
            pkg_dir = %pkg_dir.display(),
            "manifest declares bundleDependencies, but the MVP packlist port does not yet recurse into bundled deps — they will be missing from the cached snapshot",
        );
    }

    let files_field = manifest.get("files").and_then(Value::as_array);
    let files_globs: Option<Vec<&str>> = files_field
        .map(|arr| arr.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .filter(|v| !v.is_empty());

    let main_path = manifest.get("main").and_then(Value::as_str);
    let bin_paths: Vec<&str> = manifest
        .get("bin")
        .map(|bin| match bin {
            Value::String(s) => vec![s.as_str()],
            Value::Object(map) => map.values().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default();

    let mut out: BTreeSet<String> = BTreeSet::new();
    for entry in WalkDir::new(pkg_dir).into_iter().filter_entry(|e| {
        // Hard-exclude `.git` / `node_modules` etc. before descent so
        // we never spend time walking them. Note: this also filters
        // the root once `e.depth() == 0`, hence the `is_root` guard.
        let is_root = e.depth() == 0;
        is_root || !is_always_excluded_dir(e.file_name())
    }) {
        let entry = entry
            .map_err(|err| io_error(pkg_dir, err.into_io_error().unwrap_or_else(other_io_error)))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = relative_forward_slash(pkg_dir, entry.path());
        if should_always_exclude_file(&rel) {
            continue;
        }
        if should_include(&rel, &files_globs, main_path, &bin_paths) {
            out.insert(rel);
        }
    }
    Ok(out.into_iter().collect())
}

fn should_include(
    rel: &str,
    files_globs: &Option<Vec<&str>>,
    main: Option<&str>,
    bins: &[&str],
) -> bool {
    if is_always_included_basename(rel) {
        return true;
    }
    if let Some(globs) = files_globs {
        if let Some(main) = main
            && normalize_field_path(main) == rel
        {
            return true;
        }
        if bins.iter().any(|bin| normalize_field_path(bin) == rel) {
            return true;
        }
        return globs.iter().any(|glob| glob_match(normalize_field_path(glob).as_str(), rel));
    }
    // No `files` field — everything that wasn't excluded above ships.
    true
}

fn is_always_included_basename(rel: &str) -> bool {
    let basename = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    if basename == "package.json" {
        return true;
    }
    ALWAYS_INCLUDED_PREFIXES.iter().any(|prefix| basename.starts_with(prefix))
}

fn is_always_excluded_dir(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else { return false };
    ALWAYS_EXCLUDED_NAMES.contains(&name)
}

fn should_always_exclude_file(rel: &str) -> bool {
    let basename = rel.rsplit('/').next().unwrap_or(rel);
    if ALWAYS_EXCLUDED_NAMES.contains(&basename) {
        return true;
    }
    ALWAYS_EXCLUDED_SUFFIXES.iter().any(|s| basename.ends_with(s))
}

fn relative_forward_slash(root: &Path, full: &Path) -> String {
    let rel = full.strip_prefix(root).unwrap_or(full);
    let mut buf = PathBuf::from(rel).into_os_string().to_string_lossy().into_owned();
    if std::path::MAIN_SEPARATOR != '/' {
        buf = buf.replace(std::path::MAIN_SEPARATOR, "/");
    }
    buf
}

/// Strip a leading `./` and any leading slashes from `path` so manifest
/// `files` entries match the forward-slash relative form `packlist`
/// produces. Mirrors `npm-packlist`'s normalisation step.
fn normalize_field_path(path: &str) -> String {
    let trimmed = path.trim_start_matches("./");
    trimmed.trim_start_matches('/').to_string()
}

/// Minimal glob matcher supporting `*` (any non-slash run), `**` (any
/// sequence including slashes), and `?` (single non-slash char). Good
/// enough for the common `files` patterns: `dist/**`, `lib/*.js`,
/// `bin/cli`, etc.
fn glob_match(pattern: &str, candidate: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), candidate.as_bytes())
}

fn glob_match_inner(pattern: &[u8], candidate: &[u8]) -> bool {
    // Two-pointer walk with one backtrack point. The asymptotic worst
    // case is O(n*m), but real `files` patterns have only one or two
    // wildcards so the inner loop terminates fast.
    let mut p = 0;
    let mut c = 0;
    let mut star_p: Option<usize> = None;
    let mut star_c = 0;
    while c < candidate.len() {
        if p < pattern.len()
            && pattern[p] == b'*'
            && p + 1 < pattern.len()
            && pattern[p + 1] == b'*'
        {
            // `**` — match any sequence (including slashes).
            star_p = Some(p);
            p += 2;
            // Skip a single following `/` so `dist/**` matches `dist/x`.
            if p < pattern.len() && pattern[p] == b'/' {
                p += 1;
            }
            star_c = c;
            continue;
        }
        if p < pattern.len() && pattern[p] == b'*' {
            // single `*` — match any non-slash run.
            star_p = Some(p);
            p += 1;
            star_c = c;
            continue;
        }
        // `?` matches any single non-slash byte; otherwise the byte
        // must match exactly. (Without the explicit `candidate[c] !=
        // b'/'` gate, `a?b` would incorrectly match `a/b`.)
        if p < pattern.len() {
            let pc = pattern[p];
            let matches = (pc == b'?' && candidate[c] != b'/') || pc == candidate[c];
            if matches {
                p += 1;
                c += 1;
                continue;
            }
        }
        if let Some(sp) = star_p {
            // Backtrack: the `*` swallows one more candidate byte.
            // For single `*`, refuse to swallow a `/`.
            if pattern[sp] == b'*'
                && (sp + 1 >= pattern.len() || pattern[sp + 1] != b'*')
                && candidate[star_c] == b'/'
            {
                return false;
            }
            star_c += 1;
            c = star_c;
            p = sp
                + if pattern[sp] == b'*' && sp + 1 < pattern.len() && pattern[sp + 1] == b'*' {
                    2
                } else {
                    1
                };
            continue;
        }
        return false;
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn io_error(pkg_dir: &Path, source: std::io::Error) -> PacklistError {
    PacklistError::Io { pkg_dir: pkg_dir.display().to_string(), source }
}

fn other_io_error() -> std::io::Error {
    std::io::Error::other("walkdir produced an entry with no underlying io::Error")
}

#[cfg(test)]
mod tests;
