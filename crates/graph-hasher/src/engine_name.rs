/// Compute pnpm's `ENGINE_NAME` string — the same value pnpm uses
/// as the side-effects cache key prefix.
///
/// Ports
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/core/constants/src/index.ts#L7>:
/// ```js
/// `${process.platform};${process.arch};node${process.version.split('.')[0].substring(1)}`
/// ```
///
/// Example outputs:
/// - `"darwin;arm64;node20"`
/// - `"linux;x64;node22"`
/// - `"win32;x64;node24"`
///
/// `node_major` is the Node major version (e.g. `20`, `22`, `24`).
/// Callers pass it as a number because the discovery side (spawning
/// `node --version` or reading `npm_node_execpath`) is policy and
/// doesn't belong in this hasher crate.
///
/// `platform` and `arch` default to the running host via the
/// static `std::env::consts` constants mapped through Node's
/// naming scheme. Production callers can pass `None` to get the
/// host values; tests can pin both for cache-key round-trip.
pub fn engine_name(node_major: u32, platform: Option<&str>, arch: Option<&str>) -> String {
    let platform = platform.unwrap_or_else(|| host_platform());
    let arch = arch.unwrap_or_else(|| host_arch());
    format!("{platform};{arch};node{node_major}")
}

/// Map `std::env::consts::OS` to Node's `process.platform` naming.
/// Node uses `darwin` / `linux` / `win32` / `freebsd` / `openbsd` /
/// `sunos` / `aix` / `android`. Rust uses `macos` / `linux` /
/// `windows` / `freebsd` / `openbsd` / `solaris` / `aix` /
/// `android`. Only `macos`, `windows`, and `solaris` differ.
fn host_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        "solaris" => "sunos",
        other => other,
    }
}

/// Map `std::env::consts::ARCH` to Node's `process.arch` naming.
/// Node uses `x64` / `arm64` / `ia32` / `arm` / `s390x` / `ppc64`
/// / `ppc64` (LE, same string) / `loong64` / `riscv64`. Rust uses
/// `x86_64` / `aarch64` / `x86` / `arm` / `s390x` / `powerpc64` /
/// `powerpc64le` / `loongarch64` / `riscv64`. Mappings below mirror
/// what Node itself emits on each target — anything left as
/// passthrough (e.g. `arm`, `s390x`, `riscv64`) already matches
/// between the two naming schemes.
fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        // Node calls big-endian and little-endian POWER both
        // `ppc64`; only big-endian gets `endianness === 'BE'` to
        // distinguish them. Rust's two arch values both map here.
        "powerpc64" | "powerpc64le" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::engine_name;
    use pretty_assertions::assert_eq;

    /// Format matches pnpm's `${platform};${arch};node${major}`
    /// — required for the side-effects cache to interop.
    #[test]
    fn engine_name_matches_pnpm_format() {
        assert_eq!(engine_name(20, Some("darwin"), Some("arm64")), "darwin;arm64;node20");
        assert_eq!(engine_name(22, Some("linux"), Some("x64")), "linux;x64;node22");
        assert_eq!(engine_name(24, Some("win32"), Some("x64")), "win32;x64;node24");
    }

    /// Defaults route through the host mapping. Just assert the
    /// shape (three semicolon-separated parts ending in
    /// `node<digits>`) — the exact OS/arch depends on where the
    /// test is run.
    #[test]
    fn engine_name_host_default_has_expected_shape() {
        let name = engine_name(20, None, None);
        let parts: Vec<&str> = name.split(';').collect();
        assert_eq!(parts.len(), 3, "expected three parts, got {name:?}");
        assert!(parts[2].starts_with("node"), "third part must start with `node`: {name:?}");
        assert!(parts[2][4..].parse::<u32>().is_ok(), "node version must be numeric: {name:?}");
    }
}
