// Platform compatibility check for optional packages.
//
// Mirrors pnpm's `checkPlatform` from `@pnpm/config.package-is-installable`:
// https://github.com/pnpm/pnpm/blob/3f37d17b23/config/package-is-installable/src/checkPlatform.ts
//
// Only the current-platform path is implemented (no `supportedArchitectures`
// expansion — that is a separate `pnpm-workspace.yaml` feature).

/// Return the OS string as pnpm sees it.
///
/// Maps Rust `target_os` values to the Node.js `process.platform` strings
/// that appear in `package.json` `os` fields.
pub fn current_os() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else if cfg!(target_os = "openbsd") {
        "openbsd"
    } else {
        "unknown"
    }
}

/// Return the CPU architecture string as pnpm sees it.
///
/// Maps Rust `target_arch` values to the Node.js `process.arch` strings
/// that appear in `package.json` `cpu` fields.
pub fn current_cpu() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "ia32"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else if cfg!(target_arch = "s390x") {
        "s390x"
    } else if cfg!(target_arch = "powerpc64") {
        "ppc64"
    } else {
        "unknown"
    }
}

/// Return the C library identifier as pnpm sees it.
///
/// Only meaningful on Linux: returns `"glibc"` for GNU/glibc targets and
/// `"musl"` for musl targets. Returns `"unknown"` on all other platforms,
/// which causes the libc check to be skipped (mirrors pnpm's behaviour when
/// `detect-libc` returns `null`).
pub fn current_libc() -> &'static str {
    if cfg!(all(target_os = "linux", target_env = "musl")) {
        "musl"
    } else if cfg!(target_os = "linux") {
        "glibc"
    } else {
        "unknown"
    }
}

/// Return `true` if a package with the given platform constraints is
/// installable on the current system.
///
/// `None` means "no constraint" (always allowed). An empty slice defaults to
/// `["any"]` to match pnpm's runtime fallback.
///
/// Upstream reference:
/// <https://github.com/pnpm/pnpm/blob/3f37d17b23/config/package-is-installable/src/checkPlatform.ts>
pub fn is_platform_supported(
    os: Option<&[String]>,
    cpu: Option<&[String]>,
    libc: Option<&[String]>,
) -> bool {
    if let Some(os_list) = os
        && !check_list(current_os(), os_list)
    {
        return false;
    }
    if let Some(cpu_list) = cpu
        && !check_list(current_cpu(), cpu_list)
    {
        return false;
    }
    if let Some(libc_list) = libc {
        let c = current_libc();
        // Skip the libc check when we cannot determine the current libc
        // (mirrors pnpm: `if (wantedPlatform.libc && currentLibc !== 'unknown')`).
        if c != "unknown" && !check_list(c, libc_list) {
            return false;
        }
    }
    true
}

/// Check whether `value` satisfies `list`.
///
/// Rules (mirrors pnpm's `checkList`):
/// * `["any"]` → always true.
/// * Entries starting with `!` are exclusions: if `value` matches, return false.
/// * Positive entries: if any matches `value`, the check passes.
/// * If every entry is an exclusion and none matched `value`, the check passes.
fn check_list(value: &str, list: &[String]) -> bool {
    if list.len() == 1 && list[0] == "any" {
        return true;
    }
    let mut any_match = false;
    let mut blacklist_count: usize = 0;
    for entry in list {
        if let Some(excluded) = entry.strip_prefix('!') {
            if excluded == value {
                return false;
            }
            blacklist_count += 1;
        } else if entry == value {
            any_match = true;
        }
    }
    any_match || blacklist_count == list.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sl(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn any_always_passes() {
        assert!(check_list("linux", &sl(&["any"])));
        assert!(check_list("darwin", &sl(&["any"])));
    }

    #[test]
    fn positive_match() {
        assert!(check_list("linux", &sl(&["linux"])));
        assert!(!check_list("darwin", &sl(&["linux"])));
    }

    #[test]
    fn multiple_positive() {
        assert!(check_list("linux", &sl(&["linux", "darwin"])));
        assert!(check_list("darwin", &sl(&["linux", "darwin"])));
        assert!(!check_list("win32", &sl(&["linux", "darwin"])));
    }

    #[test]
    fn negation_excludes() {
        // !darwin means "not darwin" — linux passes
        assert!(check_list("linux", &sl(&["!darwin"])));
        assert!(!check_list("darwin", &sl(&["!darwin"])));
    }

    #[test]
    fn all_negations_with_no_match_passes() {
        // ["!darwin", "!win32"] — linux passes (all entries are exclusions, none matched)
        assert!(check_list("linux", &sl(&["!darwin", "!win32"])));
        assert!(!check_list("darwin", &sl(&["!darwin", "!win32"])));
    }

    #[test]
    fn is_platform_supported_no_constraints() {
        assert!(is_platform_supported(None, None, None));
    }

    #[test]
    fn is_platform_supported_os_constraint() {
        // Package that claims to support only darwin should fail on linux
        #[cfg(target_os = "linux")]
        assert!(!is_platform_supported(Some(&sl(&["darwin"])), None, None));

        #[cfg(target_os = "macos")]
        assert!(!is_platform_supported(Some(&sl(&["linux"])), None, None));
    }

    #[test]
    fn is_platform_supported_all_os_allowed() {
        assert!(is_platform_supported(Some(&sl(&["any"])), None, None));
    }
}
