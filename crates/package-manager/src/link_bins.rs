use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, ErrorKind},
    path::{Component, Path, PathBuf},
};

use derive_more::{Display, Error};
use miette::Diagnostic;
use node_semver::Version;
use pacquet_lockfile::{
    Lockfile, PackageKey, PackageMetadata, PkgNameVerPeer, ProjectSnapshot, SnapshotEntry,
};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use serde_json::Value;
use walkdir::WalkDir;

// Maps a bin name to all packages that are legitimate owners of it beyond the
// default rule that a package named `X` owns the `X` bin.
// Mirrors pnpm's `BIN_OWNER_OVERRIDES` in `@pnpm/bins.resolver`.
//
// Upstream reference:
// <https://github.com/pnpm/pnpm/blob/3f37d17b23/bins/resolver/src/index.ts>
static BIN_OWNER_OVERRIDES: &[(&str, &[&str])] = &[
    ("npx", &["npm"]),
    ("pn", &["pnpm", "@pnpm/exe"]),
    ("pnpm", &["@pnpm/exe"]),
    ("pnpx", &["pnpm", "@pnpm/exe"]),
    ("pnx", &["pnpm", "@pnpm/exe"]),
];

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
    pkg_name: String,
    pkg_version: String,
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
    /// Snapshot keys skipped by platform check; their bins are not linked.
    pub skipped_snapshots: &'a HashSet<PackageKey>,
}

impl<'a> LinkBins<'a> {
    /// Execute the subroutine.
    pub fn run(self) -> Result<(), LinkBinsError> {
        let LinkBins {
            config,
            importers,
            packages,
            snapshots,
            dependency_groups,
            skipped_snapshots,
        } = self;

        let Some(snapshots) = snapshots else { return Ok(()) };
        let Some(packages) = packages else { return Ok(()) };

        let virtual_store_dir = &config.virtual_store_dir;
        let modules_dir = &config.modules_dir;

        // Pass 1: root node_modules/.bin/ for direct dependencies.
        if let Some(root_importer) = importers.get(Lockfile::ROOT_IMPORTER_KEY) {
            let bins_dir = modules_dir.join(".bin");

            let mut cmds: Vec<BinCmd> = Vec::new();
            for (dep_name, dep_spec) in
                root_importer.dependencies_by_groups(dependency_groups.iter().copied())
            {
                let pkg_key = PkgNameVerPeer::new(dep_name.clone(), dep_spec.version.clone());

                if skipped_snapshots.contains(&pkg_key) {
                    continue;
                }

                let metadata_key = pkg_key.without_peer();

                if packages.get(&metadata_key).and_then(|m| m.has_bin) != Some(true) {
                    continue;
                }

                let pkg_dir = virtual_store_dir
                    .join(pkg_key.to_virtual_store_name())
                    .join("node_modules")
                    .join(dep_name.to_string());

                cmds.extend(read_bin_cmds(&pkg_dir)?);
            }
            create_bin_symlinks(cmds, &bins_dir)?;
        }

        // Pass 2: per-package node_modules/.bin/ for each virtual-store slot.
        //
        // Each slot's .bin contains symlinks for its own dependencies' bins,
        // so that lifecycle scripts can invoke those executables. Mirrors
        // pnpm's `linkAllBins` in the deps-restorer.
        for (snapshot_key, snapshot) in snapshots {
            if skipped_snapshots.contains(snapshot_key) {
                continue;
            }

            let snapshot_node_modules =
                virtual_store_dir.join(snapshot_key.to_virtual_store_name()).join("node_modules");
            let bins_dir = snapshot_node_modules.join(".bin");

            let all_deps = snapshot
                .dependencies
                .iter()
                .flatten()
                .chain(snapshot.optional_dependencies.iter().flatten());

            let mut cmds_for_slot: Vec<BinCmd> = Vec::new();

            for (dep_alias, dep_ref) in all_deps {
                let resolved = dep_ref.resolve(dep_alias);

                if skipped_snapshots.contains(&resolved) {
                    continue;
                }

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

            create_bin_symlinks(cmds_for_slot, &bins_dir)?;
        }

        Ok(())
    }
}

/// Read the `bin` / `directories.bin` field from `{pkg_dir}/package.json` and
/// return resolved commands.
///
/// Returns an empty `Vec` if `package.json` is absent or has no bin field.
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
/// Supports the string form, object form, and `directories.bin`, mirroring
/// `getBinsFromPackageManifest` + `commandsFromBin` in `@pnpm/bins.resolver`.
///
/// Upstream reference:
/// <https://github.com/pnpm/pnpm/blob/3f37d17b23/bins/resolver/src/index.ts>
fn parse_bin_cmds(manifest: &Value, pkg_dir: &Path) -> Vec<BinCmd> {
    let pkg_name = manifest.get("name").and_then(Value::as_str).unwrap_or("");
    let pkg_version = manifest.get("version").and_then(Value::as_str).unwrap_or("");

    if let Some(bin) = manifest.get("bin") {
        return commands_from_bin(bin, pkg_name, pkg_version, pkg_dir);
    }

    // directories.bin: enumerate files in the named sub-directory.
    // Mirrors pnpm's `findFiles` + directory path in `getBinsFromPackageManifest`.
    if let Some(Value::String(rel_dir)) = manifest.get("directories").and_then(|d| d.get("bin")) {
        let bin_dir = pkg_dir.join(rel_dir);
        if !is_within(pkg_dir, &bin_dir) {
            return vec![];
        }
        return WalkDir::new(&bin_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if !is_bin_name_safe(&name) {
                    return None;
                }
                Some(BinCmd {
                    name,
                    path: e.path().to_path_buf(),
                    pkg_name: pkg_name.to_string(),
                    pkg_version: pkg_version.to_string(),
                })
            })
            .collect();
    }

    vec![]
}

/// Build commands from a `bin` field value (string or object form).
///
/// When `bin` is a string the command name is derived from `pkg_name` (with
/// any `@scope/` prefix stripped), matching pnpm's `commandsFromBin`.
fn commands_from_bin(
    bin: &Value,
    pkg_name: &str,
    pkg_version: &str,
    pkg_dir: &Path,
) -> Vec<BinCmd> {
    let pairs: Vec<(&str, &str)> = match bin {
        Value::String(rel_path) => vec![(pkg_name, rel_path.as_str())],
        Value::Object(map) => {
            map.iter().filter_map(|(k, v)| v.as_str().map(|p| (k.as_str(), p))).collect()
        }
        _ => return vec![],
    };

    pairs
        .into_iter()
        .filter_map(|(cmd_name, rel_path)| {
            let bin_name =
                if cmd_name.starts_with('@') { cmd_name.split_once('/')?.1 } else { cmd_name };
            if bin_name.is_empty() || !is_bin_name_safe(bin_name) {
                return None;
            }
            let bin_path = pkg_dir.join(rel_path);
            if !is_within(pkg_dir, &bin_path) {
                return None;
            }
            Some(BinCmd {
                name: bin_name.to_string(),
                path: bin_path,
                pkg_name: pkg_name.to_string(),
                pkg_version: pkg_version.to_string(),
            })
        })
        .collect()
}

/// Create symlinks in `bins_dir` for each command.
///
/// Deduplicates commands by name before creating symlinks, mirroring pnpm's
/// `deduplicateCommands` call in `_linkBins`. Uses relative symlink targets
/// (matching pnpm's behaviour via `symlink-dir`).
///
/// Idempotent: skips creation when the symlink already points at the correct
/// relative target.
fn create_bin_symlinks(cmds: Vec<BinCmd>, bins_dir: &Path) -> Result<(), LinkBinsError> {
    if cmds.is_empty() {
        return Ok(());
    }

    let cmds = deduplicate_commands(cmds, bins_dir);

    fs::create_dir_all(bins_dir)
        .map_err(|error| LinkBinsError::CreateBinDir { dir: bins_dir.to_path_buf(), error })?;

    for cmd in &cmds {
        let link_path = bins_dir.join(&cmd.name);
        let rel_target = relative_path(bins_dir, &cmd.path);

        // Skip if already correct.
        if matches!(fs::read_link(&link_path), Ok(t) if t == rel_target) {
            continue;
        }

        // Remove stale link or file.
        let _ = fs::remove_file(&link_path);

        std::os::unix::fs::symlink(&rel_target, &link_path)
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

/// Deduplicate commands by name, keeping one winner per name.
///
/// Mirrors pnpm's `deduplicateCommands` + `resolveCommandConflicts` in
/// `@pnpm/bins.linker`.
///
/// Upstream reference:
/// <https://github.com/pnpm/pnpm/blob/3f37d17b23/bins/linker/src/index.ts>
fn deduplicate_commands(cmds: Vec<BinCmd>, bins_dir: &Path) -> Vec<BinCmd> {
    let mut groups: HashMap<String, Vec<BinCmd>> = HashMap::new();
    for cmd in cmds {
        groups.entry(cmd.name.clone()).or_default().push(cmd);
    }
    groups.into_values().map(|group| resolve_command_conflicts(group, bins_dir)).collect()
}

fn resolve_command_conflicts(group: Vec<BinCmd>, bins_dir: &Path) -> BinCmd {
    group
        .into_iter()
        .reduce(|a, b| {
            if compare_commands(&a, &b).is_ge() {
                tracing::debug!(
                    target: "pacquet::install",
                    binary_name = %b.name,
                    bins_dir = %bins_dir.display(),
                    linked_pkg = %a.pkg_name,
                    linked_pkg_version = %a.pkg_version,
                    skipped_pkg = %b.pkg_name,
                    skipped_pkg_version = %b.pkg_version,
                    "bin conflict resolved",
                );
                a
            } else {
                tracing::debug!(
                    target: "pacquet::install",
                    binary_name = %a.name,
                    bins_dir = %bins_dir.display(),
                    linked_pkg = %b.pkg_name,
                    linked_pkg_version = %b.pkg_version,
                    skipped_pkg = %a.pkg_name,
                    skipped_pkg_version = %a.pkg_version,
                    "bin conflict resolved",
                );
                b
            }
        })
        .unwrap() // group is non-empty by construction
}

/// Compare two commands competing for the same bin name.
///
/// Priority (highest to lowest):
/// 1. Package that "owns" the bin name (name matches or in `BIN_OWNER_OVERRIDES`).
/// 2. Alphabetically later package name (arbitrary but deterministic tiebreaker,
///    matching pnpm's `localeCompare` — see upstream).
/// 3. Higher semver version (same package, different versions).
///
/// Returns `Greater` when `a` should be chosen over `b`.
fn compare_commands(a: &BinCmd, b: &BinCmd) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let a_owns = pkg_owns_bin(&a.name, &a.pkg_name);
    let b_owns = pkg_owns_bin(&b.name, &b.pkg_name);
    if a_owns && !b_owns {
        return Ordering::Greater;
    }
    if !a_owns && b_owns {
        return Ordering::Less;
    }
    if a.pkg_name != b.pkg_name {
        return a.pkg_name.cmp(&b.pkg_name);
    }
    match (a.pkg_version.parse::<Version>(), b.pkg_version.parse::<Version>()) {
        (Ok(av), Ok(bv)) => av.cmp(&bv),
        _ => Ordering::Equal,
    }
}

/// Return `true` if `pkg_name` is the legitimate owner of `bin_name`.
///
/// Mirrors pnpm's `pkgOwnsBin` in `@pnpm/bins.resolver`.
fn pkg_owns_bin(bin_name: &str, pkg_name: &str) -> bool {
    if bin_name == pkg_name {
        return true;
    }
    BIN_OWNER_OVERRIDES.iter().any(|(b, owners)| *b == bin_name && owners.contains(&pkg_name))
}

/// Compute a relative path from `from_dir` to `to` (both absolute).
///
/// Equivalent to `path.relative(from_dir, to)` in Node.js, used so that
/// bin symlinks are portable (matching pnpm's `symlink-dir` behaviour).
fn relative_path(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component<'_>> = from_dir.components().collect();
    let to: Vec<Component<'_>> = to.components().collect();
    let common = from.iter().zip(to.iter()).take_while(|(a, b)| a == b).count();
    let mut rel = PathBuf::new();
    for _ in 0..(from.len() - common) {
        rel.push("..");
    }
    for c in &to[common..] {
        rel.push(c.as_os_str());
    }
    rel
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

    fn make_bin(name: &str, path: PathBuf) -> BinCmd {
        BinCmd {
            name: name.to_string(),
            path,
            pkg_name: name.to_string(),
            pkg_version: "1.0.0".to_string(),
        }
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
        let manifest = json!({ "name": "typescript", "version": "5.0.0", "bin": "bin/tsc" });
        let cmds = parse_bin_cmds(&manifest, tmp.path());
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "typescript");
        assert_eq!(cmds[0].path, tmp.path().join("bin/tsc"));
        assert_eq!(cmds[0].pkg_name, "typescript");
        assert_eq!(cmds[0].pkg_version, "5.0.0");
    }

    #[test]
    fn parse_bin_string_form_scoped() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "cli.js");
        let manifest = json!({ "name": "@babel/cli", "version": "7.0.0", "bin": "cli.js" });
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
            "version": "5.0.0",
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
    fn parse_directories_bin() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "scripts/foo");
        touch(tmp.path(), "scripts/bar");
        let manifest = json!({
            "name": "mypkg",
            "version": "1.0.0",
            "directories": { "bin": "scripts" }
        });
        let mut cmds = parse_bin_cmds(&manifest, tmp.path());
        cmds.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].name, "bar");
        assert_eq!(cmds[1].name, "foo");
    }

    #[test]
    fn parse_directories_bin_path_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let manifest = json!({
            "name": "evil",
            "directories": { "bin": "../../etc" }
        });
        let cmds = parse_bin_cmds(&manifest, tmp.path());
        assert!(cmds.is_empty());
    }

    #[test]
    fn create_bin_symlinks_creates_relative_link() {
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("pkg");
        let bin_target = touch(&pkg_dir, "bin/cli.js");
        let bins_dir = tmp.path().join("node_modules/.bin");

        let cmds = vec![make_bin("mycli", bin_target.clone())];
        create_bin_symlinks(cmds, &bins_dir).unwrap();

        let link = bins_dir.join("mycli");
        let link_target = fs::read_link(&link).unwrap();
        // Target must be relative, not absolute.
        assert!(
            link_target.is_relative(),
            "symlink target should be relative, got {link_target:?}"
        );
        // Resolving from bins_dir must reach the actual file.
        assert_eq!(
            bins_dir.join(&link_target).canonicalize().unwrap(),
            bin_target.canonicalize().unwrap()
        );
    }

    #[test]
    fn create_bin_symlinks_idempotent() {
        let tmp = TempDir::new().unwrap();
        let pkg_dir = tmp.path().join("pkg");
        let bin_target = touch(&pkg_dir, "bin/cli.js");
        let bins_dir = tmp.path().join("node_modules/.bin");

        let cmds = || vec![make_bin("mycli", bin_target.clone())];
        create_bin_symlinks(cmds(), &bins_dir).unwrap();
        create_bin_symlinks(cmds(), &bins_dir).unwrap();
    }

    #[test]
    fn deduplicate_keeps_owner() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "a.js");
        touch(tmp.path(), "b.js");
        // "tsc" is not owned by either package per BIN_OWNER_OVERRIDES,
        // so alphabetically later pkg name wins.
        let cmds = vec![
            BinCmd {
                name: "tsc".to_string(),
                path: tmp.path().join("a.js"),
                pkg_name: "aaa".to_string(),
                pkg_version: "1.0.0".to_string(),
            },
            BinCmd {
                name: "tsc".to_string(),
                path: tmp.path().join("b.js"),
                pkg_name: "zzz".to_string(),
                pkg_version: "1.0.0".to_string(),
            },
        ];
        let bins_dir = tmp.path().join(".bin");
        let result = deduplicate_commands(cmds, &bins_dir);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].pkg_name, "zzz");
    }

    #[test]
    fn deduplicate_owner_override_wins() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "a.js");
        touch(tmp.path(), "b.js");
        // "pnpm" bin: "@pnpm/exe" is in BIN_OWNER_OVERRIDES, so it wins over "zzz".
        let cmds = vec![
            BinCmd {
                name: "pnpm".to_string(),
                path: tmp.path().join("a.js"),
                pkg_name: "@pnpm/exe".to_string(),
                pkg_version: "9.0.0".to_string(),
            },
            BinCmd {
                name: "pnpm".to_string(),
                path: tmp.path().join("b.js"),
                pkg_name: "zzz".to_string(),
                pkg_version: "9.0.0".to_string(),
            },
        ];
        let bins_dir = tmp.path().join(".bin");
        let result = deduplicate_commands(cmds, &bins_dir);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].pkg_name, "@pnpm/exe");
    }

    #[test]
    fn relative_path_sibling_dirs() {
        let rel = relative_path(Path::new("/a/b/.bin"), Path::new("/a/b/pkg/bin/cli.js"));
        assert_eq!(rel, PathBuf::from("../pkg/bin/cli.js"));
    }

    #[test]
    fn relative_path_up_and_across() {
        let rel = relative_path(Path::new("/a/b/c"), Path::new("/a/d/e.js"));
        assert_eq!(rel, PathBuf::from("../../d/e.js"));
    }
}
