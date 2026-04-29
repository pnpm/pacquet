use std::{
    collections::HashMap,
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{Lockfile, PackageKey, PackageMetadata, PkgNameVerPeer, ProjectSnapshot, SnapshotEntry};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use serde_json::Value;

/// Error type for [`LinkBins`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum LinkBinsError {
    #[display("Failed to read package.json at {path:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::link_bins_read_manifest))]
    ReadManifest {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to parse package.json at {path:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::link_bins_parse_manifest))]
    ParseManifest {
        path: PathBuf,
        #[error(source)]
        error: serde_json::Error,
    },

    #[display("Failed to create .bin directory at {dir:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::link_bins_create_dir))]
    CreateBinDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display("Failed to create bin symlink at {link:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::link_bins_create_symlink))]
    CreateSymlink {
        link: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// A resolved binary command from a package's `package.json`.
struct BinCmd {
    name: String,
    path: PathBuf,
}

/// Create `.bin` symlinks for installed packages.
///
/// Performs two passes that mirror pnpm's `linkBinsOfPackages` +
/// `linkAllBins` steps in the headless restorer:
///
/// 1. **Root `node_modules/.bin/`** — bins exposed by direct dependencies
///    of the root project (filtered by `dependency_groups`).
/// 2. **Per-package `node_modules/.bin/`** inside each virtual-store slot —
///    bins of that slot's own dependencies, so that lifecycle scripts can
///    call their deps' executables.
///
/// Upstream reference:
/// <https://github.com/pnpm/pnpm/blob/3f37d17b23/installing/deps-restorer/src/index.ts>
#[must_use]
pub struct LinkBins<'a> {
    pub config: &'static Npmrc,
    pub importers: &'a HashMap<String, ProjectSnapshot>,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    pub dependency_groups: &'a [DependencyGroup],
}

impl<'a> LinkBins<'a> {
    /// Execute the subroutine.
    pub fn run(self) -> Result<(), LinkBinsError> {
        let LinkBins { config, importers, packages, snapshots, dependency_groups } = self;

        let Some(snapshots) = snapshots else { return Ok(()) };
        let Some(packages) = packages else { return Ok(()) };

        let virtual_store_dir = &config.virtual_store_dir;
        let modules_dir = &config.modules_dir;

        // Pass 1: root node_modules/.bin/ for direct dependencies.
        if let Some(root_importer) = importers.get(Lockfile::ROOT_IMPORTER_KEY) {
            let bins_dir = modules_dir.join(".bin");

            for (dep_name, dep_spec) in root_importer.dependencies_by_groups(dependency_groups.iter().copied()) {
                let pkg_key = PkgNameVerPeer::new(dep_name.clone(), dep_spec.version.clone());
                let metadata_key = pkg_key.without_peer();

                if packages.get(&metadata_key).and_then(|m| m.has_bin) != Some(true) {
                    continue;
                }

                let pkg_dir = virtual_store_dir
                    .join(pkg_key.to_virtual_store_name())
                    .join("node_modules")
                    .join(dep_name.to_string());

                let cmds = read_bin_cmds(&pkg_dir)?;
                create_bin_symlinks(&cmds, &bins_dir)?;
            }
        }

        // Pass 2: per-package node_modules/.bin/ for each virtual-store slot.
        //
        // Each slot's .bin contains symlinks for its own dependencies' bins,
        // so that lifecycle scripts can invoke those executables. Mirrors
        // pnpm's `linkAllBins` in the deps-restorer.
        for (snapshot_key, snapshot) in snapshots {
            let snapshot_node_modules = virtual_store_dir
                .join(snapshot_key.to_virtual_store_name())
                .join("node_modules");
            let bins_dir = snapshot_node_modules.join(".bin");

            let all_deps = snapshot
                .dependencies
                .iter()
                .flatten()
                .chain(snapshot.optional_dependencies.iter().flatten());

            let mut cmds_for_slot: Vec<BinCmd> = Vec::new();

            for (dep_alias, dep_ref) in all_deps {
                let resolved = dep_ref.resolve(dep_alias);
                let dep_metadata_key = resolved.without_peer();

                if packages.get(&dep_metadata_key).and_then(|m| m.has_bin) != Some(true) {
                    continue;
                }

                let dep_pkg_dir = virtual_store_dir
                    .join(resolved.to_virtual_store_name())
                    .join("node_modules")
                    .join(resolved.name.to_string());

                cmds_for_slot.extend(read_bin_cmds(&dep_pkg_dir)?);
            }

            if !cmds_for_slot.is_empty() {
                create_bin_symlinks(&cmds_for_slot, &bins_dir)?;
            }
        }

        Ok(())
    }
}

/// Read the `bin` field from `{pkg_dir}/package.json` and return resolved commands.
///
/// Returns an empty `Vec` if `package.json` is absent or has no `bin` field.
fn read_bin_cmds(pkg_dir: &Path) -> Result<Vec<BinCmd>, LinkBinsError> {
    let manifest_path = pkg_dir.join("package.json");
    let content = match fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(LinkBinsError::ReadManifest { path: manifest_path, error }),
    };
    let manifest: Value = serde_json::from_str(&content)
        .map_err(|error| LinkBinsError::ParseManifest { path: manifest_path, error })?;

    Ok(parse_bin_cmds(&manifest, pkg_dir))
}

/// Parse binary commands from a manifest value.
///
/// Supports both the string form (`"bin": "./cli.js"`) and the object form
/// (`"bin": { "tsc": "./bin/tsc" }`), mirroring `commandsFromBin` in
/// `@pnpm/bins.resolver`.
///
/// `directories.bin` is not yet implemented (rare in practice).
///
/// Upstream reference:
/// <https://github.com/pnpm/pnpm/blob/3f37d17b23/bins/resolver/src/index.ts>
fn parse_bin_cmds(manifest: &Value, pkg_dir: &Path) -> Vec<BinCmd> {
    let pkg_name = manifest.get("name").and_then(Value::as_str).unwrap_or("");

    match manifest.get("bin") {
        Some(Value::String(rel_path)) => {
            let cmd_name = strip_scope(pkg_name).to_string();
            if cmd_name.is_empty() || !is_bin_name_safe(&cmd_name) {
                return vec![];
            }
            let bin_path = pkg_dir.join(rel_path);
            if !is_within(pkg_dir, &bin_path) {
                return vec![];
            }
            vec![BinCmd { name: cmd_name, path: bin_path }]
        }
        Some(Value::Object(map)) => map
            .iter()
            .filter_map(|(name, path_val)| {
                let bin_name = if name.starts_with('@') {
                    name.split_once('/')?.1.to_string()
                } else {
                    name.clone()
                };
                if !is_bin_name_safe(&bin_name) {
                    return None;
                }
                let rel_path = path_val.as_str()?;
                let bin_path = pkg_dir.join(rel_path);
                if !is_within(pkg_dir, &bin_path) {
                    return None;
                }
                Some(BinCmd { name: bin_name, path: bin_path })
            })
            .collect(),
        _ => vec![],
    }
}

/// Create symlinks in `bins_dir` for each command.
///
/// Uses absolute targets (consistent with the rest of the pacquet codebase;
/// pnpm uses relative targets — tracked as a TODO).
///
/// Skips creation if the symlink already points at the correct target
/// (idempotent on warm installs).
fn create_bin_symlinks(cmds: &[BinCmd], bins_dir: &Path) -> Result<(), LinkBinsError> {
    if cmds.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(bins_dir).map_err(|error| LinkBinsError::CreateBinDir {
        dir: bins_dir.to_path_buf(),
        error,
    })?;

    for cmd in cmds {
        let link_path = bins_dir.join(&cmd.name);

        // Skip if already correct.
        if matches!(fs::read_link(&link_path), Ok(t) if t == cmd.path) {
            continue;
        }

        // Remove stale link or file.
        let _ = fs::remove_file(&link_path);

        std::os::unix::fs::symlink(&cmd.path, &link_path)
            .map_err(|error| LinkBinsError::CreateSymlink { link: link_path.clone(), error })?;

        // Ensure the target has the executable bit set.
        if let Ok(meta) = fs::metadata(&cmd.path) {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = meta.permissions();
            let mode = perms.mode();
            if mode & 0o111 == 0 {
                perms.set_mode(mode | 0o111);
                let _ = fs::set_permissions(&cmd.path, perms);
            }
        }
    }

    Ok(())
}

/// Strip the `@scope/` prefix from a package name.
///
/// Used when a `"bin"` value is a string: the command name is derived from
/// the package name, minus any scope.
fn strip_scope(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix('@') {
        rest.split_once('/').map(|x| x.1).unwrap_or(name)
    } else {
        name
    }
}

/// Return `true` if `candidate` is inside `base` (no path traversal).
///
/// Lexically normalises `candidate` by resolving `..` components before
/// calling `starts_with`, so a path like `pkg/../../../etc/passwd` does not
/// falsely pass the prefix check. Mirrors the `isSubdir` check in
/// `@pnpm/bins.resolver`.
fn is_within(base: &Path, candidate: &Path) -> bool {
    normalize_path(candidate).starts_with(base)
}

/// Resolve `..` and `.` components lexically (without hitting the filesystem).
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components: Vec<Component<'_>> = Vec::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                // Only pop a real component; ignore `..` at the root.
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                }
            }
            Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Return `true` if `name` is a safe bin name.
///
/// Mirrors pnpm's check: valid if every character is unchanged by
/// `encodeURIComponent`, or the name is exactly `$`.
///
/// `encodeURIComponent` leaves unreserved characters unencoded:
/// `A-Z a-z 0-9 - _ . ! ~ * ' ( )`
///
/// Upstream reference:
/// <https://github.com/pnpm/pnpm/blob/3f37d17b23/bins/resolver/src/index.ts>
fn is_bin_name_safe(name: &str) -> bool {
    if name == "$" {
        return true;
    }
    name.chars().all(|c| {
        matches!(c,
            'A'..='Z' | 'a'..='z' | '0'..='9'
            | '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn touch(dir: &Path, rel: &str) -> PathBuf {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, "#!/usr/bin/env node\n").unwrap();
        p
    }

    #[test]
    fn strip_scope_unscoped() {
        assert_eq!(strip_scope("typescript"), "typescript");
    }

    #[test]
    fn strip_scope_scoped() {
        assert_eq!(strip_scope("@babel/cli"), "cli");
    }

    #[test]
    fn is_bin_name_safe_valid() {
        assert!(is_bin_name_safe("tsc"));
        assert!(is_bin_name_safe("jest"));
        assert!(is_bin_name_safe("create-react-app"));
        assert!(is_bin_name_safe("$"));
    }

    #[test]
    fn is_bin_name_safe_invalid() {
        assert!(!is_bin_name_safe("../evil"));
        assert!(!is_bin_name_safe("cmd with space"));
        assert!(!is_bin_name_safe("cmd;injection"));
    }

    #[test]
    fn parse_bin_string_form() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "bin/tsc");
        let manifest = json!({ "name": "typescript", "bin": "bin/tsc" });
        let cmds = parse_bin_cmds(&manifest, tmp.path());
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "typescript");
        assert_eq!(cmds[0].path, tmp.path().join("bin/tsc"));
    }

    #[test]
    fn parse_bin_string_form_scoped() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "cli.js");
        let manifest = json!({ "name": "@babel/cli", "bin": "cli.js" });
        let cmds = parse_bin_cmds(&manifest, tmp.path());
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "cli");
    }

    #[test]
    fn parse_bin_object_form() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "bin/tsc");
        touch(tmp.path(), "bin/tsserver");
        let manifest = json!({
            "name": "typescript",
            "bin": { "tsc": "bin/tsc", "tsserver": "bin/tsserver" }
        });
        let mut cmds = parse_bin_cmds(&manifest, tmp.path());
        cmds.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].name, "tsc");
        assert_eq!(cmds[1].name, "tsserver");
    }

    #[test]
    fn parse_bin_path_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let manifest = json!({ "name": "evil", "bin": "../../../etc/passwd" });
        let cmds = parse_bin_cmds(&manifest, tmp.path());
        assert!(cmds.is_empty());
    }

    #[test]
    fn create_bin_symlinks_creates_link() {
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("pkg");
        let bin_target = touch(&pkg_dir, "bin/cli.js");
        let bins_dir = tmp.path().join("node_modules/.bin");

        let cmds = vec![BinCmd { name: "mycli".to_string(), path: bin_target.clone() }];
        create_bin_symlinks(&cmds, &bins_dir).unwrap();

        let link = bins_dir.join("mycli");
        assert!(link.exists() || link.symlink_metadata().is_ok());
        assert_eq!(fs::read_link(&link).unwrap(), bin_target);
    }

    #[test]
    fn create_bin_symlinks_idempotent() {
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("pkg");
        let bin_target = touch(&pkg_dir, "bin/cli.js");
        let bins_dir = tmp.path().join("node_modules/.bin");

        let cmds = vec![BinCmd { name: "mycli".to_string(), path: bin_target.clone() }];
        create_bin_symlinks(&cmds, &bins_dir).unwrap();
        create_bin_symlinks(&cmds, &bins_dir).unwrap(); // second call must not error
    }
}
