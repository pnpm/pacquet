use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{LoadLockfileError, Lockfile};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manager::ResolvedPackages;
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
    /// In-memory cache for packages that have started resolving dependencies.
    pub resolved_packages: ResolvedPackages,
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
    ///
    /// `require_lockfile` is `true` when the caller has committed to the
    /// frozen-lockfile install path (via `--frozen-lockfile`) and needs
    /// the lockfile loaded even when `config.lockfile` is `false`.
    /// Matches pnpm's CLI: `--frozen-lockfile` is the strongest signal,
    /// it must not be silently dropped because `lockfile` is disabled
    /// (or unset) in config.
    pub fn init(
        manifest_path: PathBuf,
        config: &'static Npmrc,
        require_lockfile: bool,
    ) -> Result<Self, InitStateError> {
        let should_load = config.lockfile || require_lockfile;
        Ok(State {
            config,
            manifest: manifest_path
                .pipe(PackageManifest::create_if_needed)
                .map_err(InitStateError::LoadManifest)?,
            lockfile: call_load_lockfile(should_load, Lockfile::load_from_current_dir)
                .map_err(InitStateError::LoadLockfile)?,
            http_client: ThrottledClient::new_from_cpu_count(),
            tarball_mem_cache: MemCache::new(),
            resolved_packages: ResolvedPackages::new(),
        })
    }
}

/// Load the lockfile from the current directory when `should_load` is
/// `true`. Callers compose `should_load` from `config.lockfile ||
/// --frozen-lockfile` so the CLI flag is always honoured.
///
/// This function was extracted to be tested independently.
fn call_load_lockfile<LoadLockfile, Lockfile, Error>(
    should_load: bool,
    load_lockfile: LoadLockfile,
) -> Result<Option<Lockfile>, Error>
where
    LoadLockfile: FnOnce() -> Result<Option<Lockfile>, Error>,
{
    should_load.then(load_lockfile).transpose().map(Option::flatten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_call_load_lockfile() {
        macro_rules! case {
            ($should_load:expr, $load_lockfile:expr => $output:expr) => {{
                let should_load = $should_load;
                let load_lockfile = $load_lockfile;
                let output: Result<Option<&str>, &str> = $output;
                eprintln!(
                    "CASE: {should_load:?}, {load_lockfile} => {output:?}",
                    load_lockfile = stringify!($load_lockfile),
                );
                assert_eq!(call_load_lockfile(should_load, load_lockfile), output);
            }};
        }

        case!(false, || unreachable!() => Ok(None));
        case!(true, || Err("error") => Err("error"));
        case!(true, || Ok(None) => Ok(None));
        case!(true, || Ok(Some("value")) => Ok(Some("value")));
    }
}
