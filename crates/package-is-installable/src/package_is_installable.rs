//! Port of `packageIsInstallable` from
//! <https://github.com/pnpm/pnpm/blob/94240bc046/config/package-is-installable/src/index.ts>.

use derive_more::{Display, Error};
use miette::Diagnostic;
use serde::Serialize;

use crate::check_engine::{
    Engine, InvalidNodeVersionError, UnsupportedEngineError, WantedEngine, check_engine,
};
use crate::check_platform::{
    SupportedArchitectures, UnsupportedPlatformError, WantedPlatform, check_platform,
};

/// Inputs from a package manifest (or lockfile metadata row) that
/// drive the installability check.
#[derive(Debug, Clone, Default)]
pub struct PackageInstallabilityManifest {
    pub engines: Option<WantedEngine>,
    pub cpu: Option<Vec<String>>,
    pub os: Option<Vec<String>>,
    pub libc: Option<Vec<String>>,
}

/// Discriminator on `pnpm:skipped-optional-dependency` payloads.
/// Matches upstream's `'unsupported_engine' | 'unsupported_platform'`
/// pair at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/config/package-is-installable/src/index.ts#L57>.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    UnsupportedEngine,
    UnsupportedPlatform,
}

/// Union of the two non-strict error shapes [`check_package`] can
/// return. Mirrors upstream's `UnsupportedEngineError |
/// UnsupportedPlatformError` from `index.ts:81`.
#[derive(Debug, Display, Error, Diagnostic, Clone, PartialEq, Eq)]
pub enum InstallabilityError {
    #[display("{_0}")]
    #[diagnostic(transparent)]
    Engine(UnsupportedEngineError),
    #[display("{_0}")]
    #[diagnostic(transparent)]
    Platform(UnsupportedPlatformError),
}

impl InstallabilityError {
    /// Map the wrapped error variant to its `pnpm:skipped-optional-dependency`
    /// reason, matching `index.ts:57`'s `'unsupported_engine' |
    /// 'unsupported_platform'` ternary.
    pub fn skip_reason(&self) -> SkipReason {
        match self {
            Self::Engine(_) => SkipReason::UnsupportedEngine,
            Self::Platform(_) => SkipReason::UnsupportedPlatform,
        }
    }
}

/// Tri-state verdict mirroring upstream's `boolean | null` return at
/// `index.ts:38`. Returned by [`package_is_installable`].
///
/// - [`InstallabilityVerdict::Installable`]: maps to upstream `true`.
///   No warning, no skip, just install.
/// - [`InstallabilityVerdict::SkipOptional`]: maps to upstream `false`.
///   The package is incompatible and was declared optional; caller
///   should emit `pnpm:skipped-optional-dependency` and exclude the
///   package from the install set.
/// - [`InstallabilityVerdict::ProceedWithWarning`]: maps to upstream
///   `null`. The package is incompatible, not optional, and
///   `engineStrict` is off; caller emits `pnpm:install-check` warn
///   (or a tracing-level warning) and proceeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallabilityVerdict {
    Installable,
    SkipOptional {
        reason: SkipReason,
        /// Details string the caller copies into the
        /// `pnpm:skipped-optional-dependency` payload's `details`
        /// field. Matches upstream `warn.toString()` at `index.ts:50`.
        details: String,
    },
    ProceedWithWarning {
        /// Message body for the `pnpm:install-check` warn. Matches
        /// upstream `warn.message` at `index.ts:44`.
        message: String,
    },
}

/// Options threaded into [`package_is_installable`] / [`check_package`].
///
/// `node_version` mirrors pnpm's `nodeVersion` config setting: if
/// present and parseable, it's used as the current node version; if
/// absent, the caller passes `current_node` from the actual runtime.
/// `pnpm_version` is normally `None` for pacquet (pacquet isn't pnpm);
/// upstream sets this from `getSystemPnpmVersion()` or a config
/// override. `engine_strict` defaults to false (pnpm's default at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/config/reader/src/index.ts>),
/// and `supported_architectures` is read from `pnpm-workspace.yaml`
/// when present.
#[derive(Debug, Clone, Default)]
pub struct InstallabilityOptions<'a> {
    pub engine_strict: bool,
    pub optional: bool,
    pub current_node_version: String,
    pub pnpm_version: Option<String>,
    pub current_os: String,
    pub current_cpu: String,
    pub current_libc: String,
    pub supported_architectures: Option<&'a SupportedArchitectures>,
}

/// Pure compose of [`check_platform`] and [`check_engine`]. Returns
/// the first error a manifest produces, or `None` if compatible.
/// Mirrors upstream `checkPackage` at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/config/package-is-installable/src/index.ts#L68-L94>.
///
/// Platform is checked first (so an unsupported OS surfaces as a
/// `Platform` error even if the engine range would also reject).
pub fn check_package(
    package_id: &str,
    manifest: &PackageInstallabilityManifest,
    options: &InstallabilityOptions<'_>,
) -> Result<Option<InstallabilityError>, InvalidNodeVersionError> {
    let wanted_platform = WantedPlatform {
        os: manifest.os.clone().or_else(|| Some(vec!["any".to_string()])),
        cpu: manifest.cpu.clone().or_else(|| Some(vec!["any".to_string()])),
        libc: manifest.libc.clone().or_else(|| Some(vec!["any".to_string()])),
    };

    if let Some(platform_err) = check_platform(
        package_id,
        &wanted_platform,
        options.supported_architectures,
        &options.current_os,
        &options.current_cpu,
        &options.current_libc,
    ) {
        return Ok(Some(InstallabilityError::Platform(platform_err)));
    }

    let Some(wanted_engines) = manifest.engines.as_ref() else {
        return Ok(None);
    };

    let current =
        Engine { node: options.current_node_version.clone(), pnpm: options.pnpm_version.clone() };
    match check_engine(package_id, wanted_engines, &current)? {
        Some(engine_err) => Ok(Some(InstallabilityError::Engine(engine_err))),
        None => Ok(None),
    }
}

/// Tri-state installability verdict, mirroring upstream
/// `packageIsInstallable` at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/config/package-is-installable/src/index.ts#L20-L66>.
///
/// Side effects (the `pnpm:install-check` warn and
/// `pnpm:skipped-optional-dependency` emit) are *not* performed here
/// — the caller composes them so log payloads can carry pacquet-
/// specific context (`prefix`, `requester`, etc.).
///
/// `InstallabilityError` is large (200+ bytes) because it carries the
/// full wanted/current platform or engine state for diagnostic
/// rendering. Boxing the `Err` arm keeps `Result<_, _>` small enough
/// for clippy's `result-large-err` lint on installs where the error
/// path is rare.
pub fn package_is_installable(
    package_id: &str,
    manifest: &PackageInstallabilityManifest,
    options: &InstallabilityOptions<'_>,
) -> Result<InstallabilityVerdict, Box<InstallabilityError>> {
    let warn = match check_package(package_id, manifest, options) {
        Ok(maybe) => maybe,
        Err(invalid_node) => {
            // Upstream `checkEngine` `throw`s on invalid node version
            // regardless of `engineStrict`. Map to a synthetic Engine
            // error so callers don't have to widen the error type.
            // (Not expected on the install hot path: pacquet passes a
            // parseable `current_node_version` from a detection step
            // that itself falls back to a known good default.)
            return Err(Box::new(InstallabilityError::Engine(
                UnsupportedEngineError::synthetic_for_invalid_node(package_id, &invalid_node),
            )));
        }
    };
    let Some(warn) = warn else { return Ok(InstallabilityVerdict::Installable) };

    if options.optional {
        return Ok(InstallabilityVerdict::SkipOptional {
            reason: warn.skip_reason(),
            details: warn.to_string(),
        });
    }

    if options.engine_strict {
        return Err(Box::new(warn));
    }

    Ok(InstallabilityVerdict::ProceedWithWarning { message: warn.to_string() })
}
