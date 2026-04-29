use crate::{
    bin_resolver::{Command, get_bins_from_package_manifest, pkg_owns_bin},
    shim::{generate_sh_shim, is_shim_pointing_at, search_script_runtime},
};
use derive_more::{Display, Error};
use miette::Diagnostic;
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

    for (bin_name, (command, _pkg)) in &chosen {
        write_shim(&command.path, &bins_dir.join(bin_name))?;
    }

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
    let runtime = search_script_runtime(target_path).map_err(|error| {
        LinkBinsError::ProbeShimSource { path: target_path.to_path_buf(), error }
    })?;

    let body = generate_sh_shim(target_path, shim_path, runtime.as_ref());

    if let Ok(existing) = fs::read_to_string(shim_path)
        && is_shim_pointing_at(&existing, target_path)
    {
        return Ok(());
    }

    fs::write(shim_path, body.as_bytes())
        .map_err(|error| LinkBinsError::WriteShim { path: shim_path.to_path_buf(), error })?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    /// End-to-end exercise: a package with a `bin` field has a shim written
    /// into the bins dir, the shim references the correct relative path,
    /// and (on Unix) both the shim and the target are executable.
    #[test]
    fn writes_shim_for_bin_string() {
        let tmp = tempdir().unwrap();
        let pkg_dir = tmp.path().join("node_modules/foo");
        fs::create_dir_all(pkg_dir.join("bin")).unwrap();
        fs::write(
            pkg_dir.join("package.json"),
            json!({"name": "foo", "version": "1.0.0", "bin": "bin/cli.js"}).to_string(),
        )
        .unwrap();
        fs::write(pkg_dir.join("bin/cli.js"), "#!/usr/bin/env node\n").unwrap();

        let bins_dir = tmp.path().join("node_modules/.bin");
        let manifest_value: Value =
            serde_json::from_slice(&fs::read(pkg_dir.join("package.json")).unwrap()).unwrap();
        link_bins_of_packages(
            &[PackageBinSource { location: pkg_dir.clone(), manifest: manifest_value }],
            &bins_dir,
        )
        .unwrap();

        let shim_path = bins_dir.join("foo");
        assert!(shim_path.exists(), "shim should be created");

        let body = fs::read_to_string(&shim_path).unwrap();
        assert!(body.contains("\"$basedir/../foo/bin/cli.js\""), "shim body: {body}");
        assert!(is_shim_pointing_at(&body, &pkg_dir.join("bin/cli.js")));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&shim_path).unwrap().permissions().mode() & 0o777,
                0o755,
                "shim must be 0o755",
            );
            assert!(
                fs::metadata(pkg_dir.join("bin/cli.js")).unwrap().permissions().mode() & 0o111 != 0,
                "target must have at least one executable bit",
            );
        }
    }

    /// `link_bins(modulesDir, binsDir)` walks every package and its scoped
    /// children. Both regular and `@scope/...` packages must contribute their
    /// bins.
    #[test]
    fn link_bins_walks_modules_and_scopes() {
        let tmp = tempdir().unwrap();
        let modules = tmp.path().join("node_modules");
        // Regular package
        fs::create_dir_all(modules.join("foo")).unwrap();
        fs::write(
            modules.join("foo/package.json"),
            json!({"name": "foo", "bin": "f.js"}).to_string(),
        )
        .unwrap();
        fs::write(modules.join("foo/f.js"), "#!/usr/bin/env node\n").unwrap();
        // Scoped package
        fs::create_dir_all(modules.join("@s/bar")).unwrap();
        fs::write(
            modules.join("@s/bar/package.json"),
            json!({"name": "@s/bar", "bin": "b.js"}).to_string(),
        )
        .unwrap();
        fs::write(modules.join("@s/bar/b.js"), "#!/usr/bin/env node\n").unwrap();
        // Non-package directory (no package.json) — must be ignored, not error.
        fs::create_dir_all(modules.join("not-a-package")).unwrap();

        let bins = modules.join(".bin");
        link_bins(&modules, &bins).unwrap();

        assert!(bins.join("foo").exists(), "foo shim must exist");
        assert!(bins.join("bar").exists(), "scoped @s/bar shim must use bare name `bar`");
    }

    /// Conflict resolution: when two packages declare the same bin name, the
    /// owning package wins.
    #[test]
    fn ownership_breaks_bin_conflicts() {
        let tmp = tempdir().unwrap();
        let npm = tmp.path().join("npm");
        let other = tmp.path().join("other");
        for d in [&npm, &other] {
            fs::create_dir_all(d).unwrap();
            fs::write(d.join("npx"), "#!/usr/bin/env node\n").unwrap();
        }
        fs::write(
            npm.join("package.json"),
            json!({"name": "npm", "bin": {"npx": "npx"}}).to_string(),
        )
        .unwrap();
        fs::write(
            other.join("package.json"),
            json!({"name": "other", "bin": {"npx": "npx"}}).to_string(),
        )
        .unwrap();

        let manifest_npm: Value =
            serde_json::from_slice(&fs::read(npm.join("package.json")).unwrap()).unwrap();
        let manifest_other: Value =
            serde_json::from_slice(&fs::read(other.join("package.json")).unwrap()).unwrap();

        let bins = tmp.path().join(".bin");
        link_bins_of_packages(
            &[
                PackageBinSource { location: other.clone(), manifest: manifest_other },
                PackageBinSource { location: npm.clone(), manifest: manifest_npm },
            ],
            &bins,
        )
        .unwrap();

        let body = fs::read_to_string(bins.join("npx")).unwrap();
        // npm's `npx` lives at `<npm>/npx`; the shim must reference that path.
        assert!(
            body.contains("/npm/npx") || is_shim_pointing_at(&body, &npm.join("npx")),
            "ownership-aware resolution should pick npm's npx, body:\n{body}",
        );
    }
}
