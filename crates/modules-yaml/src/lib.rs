//! Read and write pnpm's `node_modules/.modules.yaml` manifest.
//!
//! Mirrors pnpm v11's `installing/modules-yaml` package. See upstream
//! <https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts>.
//!
//! The manifest is stored at `<modules_dir>/.modules.yaml`, where
//! `modules_dir` is the path of a `node_modules` directory. The on-disk
//! format is JSON (which YAML accepts), so reads use a YAML parser and
//! writes emit [`serde_json::to_string_pretty`] output to match pnpm exactly.

use derive_more::{Display, Error};
use pacquet_diagnostics::miette::{self, Diagnostic};
use serde_json::{Map, Value};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};
use strum::IntoStaticStr;

/// Filename of the modules manifest inside `node_modules/`.
///
/// The leading dot is required because `npm shrinkwrap` would otherwise
/// treat the file as an extraneous package. See upstream comment at
/// <https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L15-L17>.
pub const MODULES_FILENAME: &str = ".modules.yaml";

/// Default value for the `virtualStoreDirMaxLength` field.
///
/// Matches pnpm's fallback at
/// <https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L101-L103>.
pub const DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH: u64 = 120;

/// Capability trait: read a file's contents into a [`String`].
///
/// One trait per filesystem capability so each function declares only what
/// it actually uses, and so test fakes only implement the methods that
/// will be exercised. Pattern follows the per-capability typeclass style
/// rather than `parallel-disk-usage`'s lumped `FsApi` at
/// <https://github.com/KSXGitHub/parallel-disk-usage/blob/2aa39917f9/src/app/hdd.rs#L29-L35>.
pub trait FsReadToString {
    fn read_to_string(path: &Path) -> io::Result<String>;
}

/// Capability trait: create a directory and any missing parents.
pub trait FsCreateDirAll {
    fn create_dir_all(path: &Path) -> io::Result<()>;
}

/// Capability trait: write bytes to a file, replacing existing contents.
pub trait FsWrite {
    fn write(path: &Path, contents: &[u8]) -> io::Result<()>;
}

/// Production implementation, backed by [`std::fs`]. One impl block per
/// capability trait. Production uses the full set; tests pick the methods
/// they need.
pub struct RealApi;

impl FsReadToString for RealApi {
    #[inline]
    fn read_to_string(path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }
}

impl FsCreateDirAll for RealApi {
    #[inline]
    fn create_dir_all(path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }
}

impl FsWrite for RealApi {
    #[inline]
    fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
        fs::write(path, contents)
    }
}

/// Free-form representation of a `.modules.yaml` manifest.
///
/// pnpm carries a strongly-typed `Modules` interface upstream. Pacquet keeps
/// the manifest as a [`serde_json::Value`] while the surrounding install
/// pipeline is being ported; the on-disk format is JSON regardless.
pub type ModulesManifest = Value;

/// Error returned by [`read_modules_manifest`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum ReadModulesManifestError {
    #[display("Failed to read {path:?}: {source}")]
    #[diagnostic(code(pacquet_modules_yaml::read_io))]
    ReadFile { path: PathBuf, source: io::Error },

    #[display("Failed to parse {path:?}: {source}")]
    #[diagnostic(code(pacquet_modules_yaml::parse_yaml))]
    ParseYaml { path: PathBuf, source: Box<serde_saphyr::Error> },
}

/// Error returned by [`write_modules_manifest`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum WriteModulesManifestError {
    #[display("Failed to create directory {path:?}: {source}")]
    #[diagnostic(code(pacquet_modules_yaml::create_dir))]
    CreateDir { path: PathBuf, source: io::Error },

    #[display("Failed to serialize manifest: {_0}")]
    #[diagnostic(code(pacquet_modules_yaml::serialize_json))]
    SerializeJson(serde_json::Error),

    #[display("Failed to write {path:?}: {source}")]
    #[diagnostic(code(pacquet_modules_yaml::write_io))]
    WriteFile { path: PathBuf, source: io::Error },
}

/// Read `<modules_dir>/.modules.yaml` and return the normalized manifest.
///
/// Returns `Ok(None)` when the file does not exist or is empty, matching
/// upstream `readModulesManifest` at
/// <https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L50-L105>.
///
/// Production callers turbofish [`RealApi`]: `read_modules_manifest::<RealApi>(dir)`.
/// The bound is the minimal capability ([`FsReadToString`]) so test fakes
/// only need to implement the method that is actually called.
pub fn read_modules_manifest<Api: FsReadToString>(
    modules_dir: &Path,
) -> Result<Option<ModulesManifest>, ReadModulesManifestError> {
    let manifest_path = modules_dir.join(MODULES_FILENAME);
    let content = match Api::read_to_string(&manifest_path) {
        Ok(content) => content,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ReadModulesManifestError::ReadFile { path: manifest_path, source });
        }
    };
    let mut manifest: Value =
        serde_saphyr::from_str(&content).map_err(|source| ReadModulesManifestError::ParseYaml {
            path: manifest_path.clone(),
            source: Box::new(source),
        })?;
    if manifest.is_null() {
        return Ok(None);
    }
    if let Value::Object(fields) = &mut manifest {
        normalize_after_read(modules_dir, fields);
    }
    Ok(Some(manifest))
}

/// Write `manifest` to `<modules_dir>/.modules.yaml`, creating `modules_dir`
/// if it does not already exist.
///
/// Mirrors upstream `writeModulesManifest` at
/// <https://github.com/pnpm/pnpm/blob/1819226b51/installing/modules-yaml/src/index.ts#L111-L138>.
///
/// Production callers turbofish [`RealApi`]: `write_modules_manifest::<RealApi>(dir, &m)`.
/// Bounds are minimal: only [`FsCreateDirAll`] and [`FsWrite`] are required.
pub fn write_modules_manifest<Api: FsCreateDirAll + FsWrite>(
    modules_dir: &Path,
    manifest: &ModulesManifest,
) -> Result<(), WriteModulesManifestError> {
    let mut to_save = manifest.clone();
    if let Value::Object(fields) = &mut to_save {
        normalize_before_write(modules_dir, fields);
    }
    let serialized =
        serde_json::to_string_pretty(&to_save).map_err(WriteModulesManifestError::SerializeJson)?;
    Api::create_dir_all(modules_dir).map_err(|source| WriteModulesManifestError::CreateDir {
        path: modules_dir.to_path_buf(),
        source,
    })?;
    let manifest_path = modules_dir.join(MODULES_FILENAME);
    Api::write(&manifest_path, serialized.as_bytes())
        .map_err(|source| WriteModulesManifestError::WriteFile { path: manifest_path, source })
}

/// Apply the post-read transforms pnpm performs at upstream L62-L104.
fn normalize_after_read(modules_dir: &Path, fields: &mut Map<String, Value>) {
    resolve_virtual_store_dir(modules_dir, fields);
    apply_legacy_shamefully_hoist(fields);
    if !is_present_string(fields.get("prunedAt")) {
        fields.insert("prunedAt".to_string(), Value::String(http_date_now()));
    }
    if !is_present_number(fields.get("virtualStoreDirMaxLength")) {
        fields.insert(
            "virtualStoreDirMaxLength".to_string(),
            Value::Number(DEFAULT_VIRTUAL_STORE_DIR_MAX_LENGTH.into()),
        );
    }
}

/// Apply the pre-write transforms pnpm performs at upstream L116-L135.
fn normalize_before_write(modules_dir: &Path, fields: &mut Map<String, Value>) {
    sort_skipped(fields);
    drop_empty_hoist_fields(fields);
    // Junctions on Windows break when the project moves, so the absolute
    // path is intentionally preserved there. See upstream L129-L135.
    if !cfg!(windows) {
        rewrite_virtual_store_dir_relative(modules_dir, fields);
    }
}

/// Match pnpm's L66-L70. When `virtualStoreDir` is missing, default to
/// `modules_dir/.pnpm`. When it is relative, resolve it against
/// `modules_dir`.
fn resolve_virtual_store_dir(modules_dir: &Path, fields: &mut Map<String, Value>) {
    let resolved = match fields.get("virtualStoreDir").and_then(Value::as_str) {
        None | Some("") => modules_dir.join(".pnpm"),
        Some(stored) => {
            let stored_path = Path::new(stored);
            if stored_path.is_absolute() {
                stored_path.to_path_buf()
            } else {
                modules_dir.join(stored_path)
            }
        }
    };
    fields.insert("virtualStoreDir".to_string(), path_to_value(&resolved));
}

/// Match pnpm's L132-L135 by storing `virtualStoreDir` relative to
/// `modules_dir`. Falls back to the original value when stripping the
/// prefix fails.
fn rewrite_virtual_store_dir_relative(modules_dir: &Path, fields: &mut Map<String, Value>) {
    let Some(stored) = fields.get("virtualStoreDir").and_then(Value::as_str) else {
        return;
    };
    let stored_path = Path::new(stored);
    let relative = stored_path.strip_prefix(modules_dir).unwrap_or(stored_path);
    fields.insert("virtualStoreDir".to_string(), path_to_value(relative));
}

/// Per-alias visibility selected by the legacy `shamefullyHoist` flag. The
/// derive emits `"public"` and `"private"` so the value lands as the
/// expected JSON string when written into a derived `hoistedDependencies`
/// entry.
#[derive(Debug, Clone, Copy, IntoStaticStr)]
enum HoistKind {
    #[strum(serialize = "public")]
    Public,
    #[strum(serialize = "private")]
    Private,
}

/// Translate the legacy `shamefullyHoist` and `hoistedAliases` fields into
/// the modern `publicHoistPattern` and `hoistedDependencies` shapes. Mirrors
/// upstream L71-L97.
fn apply_legacy_shamefully_hoist(fields: &mut Map<String, Value>) {
    let Some(shamefully_hoist) = fields.get("shamefullyHoist").and_then(Value::as_bool) else {
        return;
    };
    let kind = if shamefully_hoist { HoistKind::Public } else { HoistKind::Private };
    if !fields.contains_key("publicHoistPattern") {
        let default_pattern = if shamefully_hoist {
            Value::Array(vec![Value::String("*".to_string())])
        } else {
            Value::Array(Vec::new())
        };
        fields.insert("publicHoistPattern".to_string(), default_pattern);
    }
    if fields.contains_key("hoistedAliases") && !fields.contains_key("hoistedDependencies") {
        let hoisted_aliases = fields.get("hoistedAliases").cloned().unwrap_or(Value::Null);
        fields.insert(
            "hoistedDependencies".to_string(),
            derive_hoisted_dependencies(&hoisted_aliases, kind),
        );
    }
}

fn derive_hoisted_dependencies(hoisted_aliases: &Value, kind: HoistKind) -> Value {
    let Value::Object(aliases_by_path) = hoisted_aliases else {
        return Value::Object(Map::new());
    };
    let kind_str: &'static str = kind.into();
    let mut derived = Map::with_capacity(aliases_by_path.len());
    for (dep_path, alias_list) in aliases_by_path {
        let mut entry = Map::new();
        if let Value::Array(alias_values) = alias_list {
            for alias_value in alias_values {
                if let Value::String(alias_name) = alias_value {
                    entry.insert(alias_name.clone(), Value::String(kind_str.to_string()));
                }
            }
        }
        derived.insert(dep_path.clone(), Value::Object(entry));
    }
    Value::Object(derived)
}

fn sort_skipped(fields: &mut Map<String, Value>) {
    let Some(Value::Array(skipped)) = fields.get_mut("skipped") else {
        return;
    };
    skipped.sort_by(|left, right| match (left, right) {
        (Value::String(left), Value::String(right)) => left.cmp(right),
        // Non-string entries compare equal to everything else, so stable
        // sort keeps them in input order. pnpm only ever writes `skipped`
        // as `string[]`, so this fallback exists for corrupt manifests
        // rather than any real upstream shape.
        _ => std::cmp::Ordering::Equal,
    });
}

fn drop_empty_hoist_fields(fields: &mut Map<String, Value>) {
    if is_empty_or_null(fields.get("hoistPattern")) {
        fields.shift_remove("hoistPattern");
    }
    if is_null_or_missing(fields.get("publicHoistPattern")) {
        fields.shift_remove("publicHoistPattern");
    }
    let drop_hoisted_aliases = match fields.get("hoistedAliases") {
        None | Some(Value::Null) => true,
        _ => !fields.contains_key("hoistPattern") && !fields.contains_key("publicHoistPattern"),
    };
    if drop_hoisted_aliases {
        fields.shift_remove("hoistedAliases");
    }
}

fn is_present_string(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(string)) if !string.is_empty())
}

fn is_present_number(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Number(_)))
}

fn is_null_or_missing(value: Option<&Value>) -> bool {
    matches!(value, None | Some(Value::Null))
}

fn is_empty_or_null(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(string)) => string.is_empty(),
        _ => false,
    }
}

fn path_to_value(path: &Path) -> Value {
    Value::String(path.to_string_lossy().into_owned())
}

fn http_date_now() -> String {
    httpdate::fmt_http_date(SystemTime::now())
}

#[cfg(test)]
mod edge_cases;
