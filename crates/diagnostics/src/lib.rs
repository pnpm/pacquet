mod local_tracing;

pub use miette;
pub use thiserror;
pub use tracing;

pub use local_tracing::enable_tracing_by_env;

pub type Error = miette::Error;
pub type Severity = miette::Severity;
pub type Report = miette::Report;
pub type Result<T> = miette::Result<T>;
