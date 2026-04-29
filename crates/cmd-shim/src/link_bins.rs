use crate::{
    bin_resolver::{Command, get_bins_from_package_manifest, pkg_owns_bin},
    shim::{
        RealFs, generate_cmd_shim, generate_pwsh_shim, generate_sh_shim, is_shim_pointing_at,
        search_script_runtime,
    },
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use rayon::prelude::*;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

/// One package known to be installed at `location`, with its parsed
/// `package.json`. Mirrors the per-package input shape of pnpm's
/// `linkBinsOfPackages`.
#[derive(Debug, Clone)]
pub struct PackageBinSource {
    pub location: PathBuf,
    pub manifest: Value,
}

/// Error type for [`link_bins_of_packages`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum LinkBinsError {
    #[display("Failed to create bin directory at {dir:?}: {error}")]
    #[diagnostic(code(pacquet_cmd_shim::create_bin_dir))]
    CreateBinDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to read package manifest at {path:?}: {error}")]
    #[diagnostic(code(pacquet_cmd_shim::read_manifest))]
    ReadManifest {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to parse package manifest at {path:?}: {error}")]
    #[diagnostic(code(pacquet_cmd_shim::parse_manifest))]
    ParseManifest {
        path: PathBuf,
        #[error(source)]
        error: serde_json::Error,
    },

    #[display("Failed to read shim source {path:?}: {error}")]
    #[diagnostic(code(pacquet_cmd_shim::probe_shim_source))]
    ProbeShimSource {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to write shim file at {path:?}: {error}")]
    #[diagnostic(code(pacquet_cmd_shim::write_shim))]
    WriteShim {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to chmod {path:?}: {error}")]
    #[diagnostic(code(pacquet_cmd_shim::chmod))]
    Chmod {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Read `<location>/package.json` for each entry under `modules_dir` and link
/// its bins into `bins_dir`. Mirrors pnpm v11's `linkBins(modulesDir, binsDir)`
/// at <https://github.com/pnpm/pnpm/blob/4750fd370c/bins/linker/src/index.ts>.
///
/// Skips:
/// - The `.bin` and `.pacquet` directories themselves (and any other
///   dot-prefixed entry, matching pnpm).
/// - Entries whose `package.json` cannot be read (legitimate when a directory
///   under `node_modules` happens to not be a package, e.g. an empty scope
///   directory).
///
/// Scoped packages are recursed: `node_modules/@scope/foo` becomes one
/// candidate. This mirrors `binNamesAndPaths` in upstream `linkBins`.
pub fn link_bins(modules_dir: &Path, bins_dir: &Path) -> Result<(), LinkBinsError> {
    let packages = collect_packages_in_modules_dir(modules_dir)?;
    link_bins_of_packages(&packages, bins_dir)
}

fn collect_packages_in_modules_dir(
    modules_dir: &Path,
) -> Result<Vec<PackageBinSource>, LinkBinsError> {
    let mut packages = Vec::new();

    let entries = match fs::read_dir(modules_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(packages),
        Err(error) => {
            return Err(LinkBinsError::CreateBinDir { dir: modules_dir.to_path_buf(), error });
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();

        if name_str.starts_with('@') {
            // Scoped: walk one level deeper.
            let Ok(scope_entries) = fs::read_dir(&path) else {
                continue;
            };
            for sub in scope_entries.flatten() {
                let sub_path = sub.path();
                if let Some(pkg) = read_package(&sub_path)? {
                    packages.push(pkg);
                }
            }
            continue;
        }

        if let Some(pkg) = read_package(&path)? {
            packages.push(pkg);
        }
    }

    Ok(packages)
}

fn read_package(location: &Path) -> Result<Option<PackageBinSource>, LinkBinsError> {
    let manifest_path = location.join("package.json");
    let bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(LinkBinsError::ReadManifest { path: manifest_path, error }),
    };
    let manifest: Value = serde_json::from_slice(&bytes)
        .map_err(|error| LinkBinsError::ParseManifest { path: manifest_path, error })?;
    Ok(Some(PackageBinSource { location: location.to_path_buf(), manifest }))
}

/// Link every bin declared by `packages` into `bins_dir`, applying the same
/// conflict resolution upstream uses.
///
/// Conflict resolution mirrors `resolveCommandConflicts`:
///
/// 1. Ownership wins. If exactly one package owns the bin name (via
///    [`pkg_owns_bin`]), it wins outright.
/// 2. Otherwise lexical comparison on the package name, lower wins. Stable
///    and deterministic regardless of the order packages were discovered.
///
/// Pacquet's first iteration does not resolve same-package multi-version
/// conflicts via semver (a feature upstream uses for hoisting), since the
/// virtual-store layout means each bin source is a unique
/// `(package, version)` slot already.
pub fn link_bins_of_packages(
    packages: &[PackageBinSource],
    bins_dir: &Path,
) -> Result<(), LinkBinsError> {
    let mut chosen: HashMap<String, (Command, &PackageBinSource)> = HashMap::new();

    for pkg in packages {
        let pkg_name = pkg.manifest.get("name").and_then(Value::as_str).unwrap_or("");
        let commands = get_bins_from_package_manifest(&pkg.manifest, &pkg.location);
        for command in commands {
            match chosen.get(&command.name) {
                None => {
                    chosen.insert(command.name.clone(), (command, pkg));
                }
                Some((_, existing)) => {
                    let existing_name =
                        existing.manifest.get("name").and_then(Value::as_str).unwrap_or("");
                    if pick_winner(&command.name, existing_name, pkg_name) {
                        chosen.insert(command.name.clone(), (command, pkg));
                    }
                }
            }
        }
    }

    if chosen.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(bins_dir)
        .map_err(|error| LinkBinsError::CreateBinDir { dir: bins_dir.to_path_buf(), error })?;

    // Each shim's read-shebang + write-file + chmod sequence is independent
    // across bin names — no shared state — so drive them on rayon. The hot
    // path is per-package-bin; without parallelism the per-shim file I/O
    // serialised across the whole `chosen` map.
    chosen.par_iter().try_for_each(|(bin_name, (command, _pkg))| {
        write_shim(&command.path, &bins_dir.join(bin_name))
    })?;

    Ok(())
}

/// Return `true` when `candidate` should replace `existing` for `bin_name`.
/// Matches the two-step ownership-then-lexical-compare in upstream's
/// `resolveCommandConflicts`.
fn pick_winner(bin_name: &str, existing: &str, candidate: &str) -> bool {
    let existing_owns = pkg_owns_bin(bin_name, existing);
    let candidate_owns = pkg_owns_bin(bin_name, candidate);
    match (existing_owns, candidate_owns) {
        (true, false) => false,
        (false, true) => true,
        // Both own (or neither): fall through to lexical compare. Picking the
        // smaller name keeps results deterministic across input orderings.
        _ => candidate < existing,
    }
}

/// Write the shell shim for `target_path` at `shim_path` and chmod it
/// executable. Idempotent on warm reinstalls via [`is_shim_pointing_at`].
///
/// On Unix this writes a single shell script and chmods both it and the
/// target binary to `0o755`, matching the `fixBin(cmd.path, 0o755)` and
/// `chmodShim` sequence in pnpm v11. Windows `.cmd` / `.ps1` are deferred.
/// The platform-specific behavior is gated behind `#[cfg(unix)]` so the
/// build still compiles on Windows.
fn write_shim(target_path: &Path, shim_path: &Path) -> Result<(), LinkBinsError> {
    let runtime = search_script_runtime::<RealFs>(target_path).map_err(|error| {
        LinkBinsError::ProbeShimSource { path: target_path.to_path_buf(), error }
    })?;

    // Idempotent skip when the existing `.sh` shim already points at
    // the right target. We only check the `.sh` flavor; if it's correct,
    // the `.cmd`/`.ps1` siblings were written together and are also
    // correct (and if they aren't, rewriting them is cheap).
    let already_correct = matches!(fs::read_to_string(shim_path), Ok(existing) if is_shim_pointing_at(&existing, target_path));

    let sh_body = generate_sh_shim(target_path, shim_path, runtime.as_ref());
    let cmd_path = with_extension_appended(shim_path, "cmd");
    let ps1_path = with_extension_appended(shim_path, "ps1");
    let cmd_body = generate_cmd_shim(target_path, &cmd_path, runtime.as_ref());
    let ps1_body = generate_pwsh_shim(target_path, &ps1_path, runtime.as_ref());

    if !already_correct {
        fs::write(shim_path, sh_body.as_bytes())
            .map_err(|error| LinkBinsError::WriteShim { path: shim_path.to_path_buf(), error })?;
        fs::write(&cmd_path, cmd_body.as_bytes())
            .map_err(|error| LinkBinsError::WriteShim { path: cmd_path.clone(), error })?;
        fs::write(&ps1_path, ps1_body.as_bytes())
            .map_err(|error| LinkBinsError::WriteShim { path: ps1_path.clone(), error })?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(shim_path, fs::Permissions::from_mode(0o755))
            .map_err(|error| LinkBinsError::Chmod { path: shim_path.to_path_buf(), error })?;
        // Make the underlying script executable too. pnpm calls
        // `fixBin(cmd.path, 0o755)` to do this; we apply the same minimum
        // mode without rewriting CRLF shebangs (a feature pnpm inherits from
        // npm's `bin-links/lib/fix-bin.js`). Targets shipped by npm already
        // use LF in practice, so the simpler chmod-only path is enough for
        // the install tests this PR ports.
        if let Ok(metadata) = fs::metadata(target_path) {
            let mode = metadata.permissions().mode() | 0o111;
            let _ = fs::set_permissions(target_path, fs::Permissions::from_mode(mode));
        }
    }

    Ok(())
}

/// Append `<ext>` to `path` as a *new* extension segment (`foo` →
/// `foo.cmd`), regardless of any existing extension. `Path::with_extension`
/// would *replace* the existing extension, which is wrong for our case —
/// the bin name `tsc` keeps its own `tsc` and gains a sibling `tsc.cmd`,
/// not turn into `tsc.cmd` losing the original `.sh` flavor.
fn with_extension_appended(path: &Path, ext: &str) -> std::path::PathBuf {
    let mut result = path.as_os_str().to_owned();
    result.push(".");
    result.push(ext);
    result.into()
}

#[cfg(test)]
mod tests;
