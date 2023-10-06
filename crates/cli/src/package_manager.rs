use std::{io, path::PathBuf};

use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
};
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::Npmrc;
use pacquet_package_json::PackageJson;
use pacquet_package_manager::{AutoImportError, CreateVirtdirError};
use pacquet_tarball::Cache;

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum PackageManagerError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Tarball(#[from] pacquet_tarball::TarballError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    PackageJson(#[from] pacquet_package_json::PackageJsonError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    LoadLockfileError(#[from] pacquet_lockfile::LoadLockfileError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Registry(#[from] pacquet_registry::RegistryError),

    #[error(transparent)]
    #[diagnostic(code(pacquet_package_manager::io_error))]
    Io(#[from] io::Error),

    #[error(transparent)]
    #[diagnostic(transparent)]
    AutoImport(#[from] AutoImportError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    CreateVirtdir(#[from] CreateVirtdirError),
}

pub struct PackageManager {
    pub config: &'static Npmrc,
    pub package_json: PackageJson,
    pub lockfile: Option<Lockfile>,
    pub http_client: reqwest::Client,
    pub(crate) tarball_cache: Cache,
}

impl PackageManager {
    pub fn new<P: Into<PathBuf>>(
        package_json_path: P,
        config: &'static Npmrc,
    ) -> Result<Self, PackageManagerError> {
        Ok(PackageManager {
            config,
            package_json: PackageJson::create_if_needed(package_json_path.into())?,
            lockfile: call_load_lockfile(config.lockfile, Lockfile::load_from_current_dir)?,
            http_client: reqwest::Client::new(),
            tarball_cache: Cache::new(),
        })
    }
}

/// Private function to load lockfile from current directory should `config.lockfile` is `true`.
///
/// This function was extracted to be tested independently.
fn call_load_lockfile<LoadLockfile, Lockfile, Error>(
    config_lockfile: bool,
    load_lockfile: LoadLockfile,
) -> Result<Option<Lockfile>, Error>
where
    LoadLockfile: FnOnce() -> Result<Option<Lockfile>, Error>,
{
    config_lockfile.then(load_lockfile).transpose().map(Option::flatten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_call_load_lockfile() {
        macro_rules! case {
            ($config_lockfile:expr, $load_lockfile:expr => $output:expr) => {{
                let config_lockfile = $config_lockfile;
                let load_lockfile = $load_lockfile;
                let output: Result<Option<&str>, &str> = $output;
                eprintln!(
                    "CASE: {config_lockfile:?}, {load_lockfile} => {output:?}",
                    load_lockfile = stringify!($load_lockfile),
                );
                assert_eq!(call_load_lockfile(config_lockfile, load_lockfile), output);
            }};
        }

        case!(false, || unreachable!() => Ok(None));
        case!(true, || Err("error") => Err("error"));
        case!(true, || Ok(None) => Ok(None));
        case!(true, || Ok(Some("value")) => Ok(Some("value")));
    }
}
