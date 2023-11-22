use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{LoadLockfileError, Lockfile};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{PackageManifest, PackageManifestError};
use pacquet_tarball::MemCache;
use pipe_trait::Pipe;
use std::path::PathBuf;

/// Application state when running `pacquet run` or `pacquet install`.
pub struct State {
    /// Shared cache that store downloaded tarballs.
    pub tarball_mem_cache: MemCache,
    /// HTTP client to make HTTP requests.
    pub http_client: ThrottledClient,
    /// Configuration read from `.npmrc`
    pub config: &'static Npmrc,
    /// Data from the `package.json` file.
    pub manifest: PackageManifest,
    /// Data from the `pnpm-lock.yaml` file.
    pub lockfile: Option<Lockfile>,
}

/// Error type of [`State::init`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum InitStateError {
    #[diagnostic(transparent)]
    LoadManifest(#[error(source)] PackageManifestError),

    #[diagnostic(transparent)]
    LoadLockfile(#[error(source)] LoadLockfileError),
}

impl State {
    /// Initialize the application state.
    pub fn init(manifest_path: PathBuf, config: &'static Npmrc) -> Result<Self, InitStateError> {
        Ok(State {
            config,
            manifest: manifest_path
                .pipe(PackageManifest::create_if_needed)
                .map_err(InitStateError::LoadManifest)?,
            lockfile: call_load_lockfile(config.lockfile, Lockfile::load_from_current_dir)
                .map_err(InitStateError::LoadLockfile)?,
            http_client: ThrottledClient::new_from_cpu_count(),
            tarball_mem_cache: MemCache::new(),
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
