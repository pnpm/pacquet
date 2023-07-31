use std::process::Command;

use miette::{Diagnostic, Result};
use thiserror::Error;

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum ExecutorError {
    #[error(transparent)]
    #[diagnostic(code(pacquet_executor::io_error))]
    Io(#[from] std::io::Error),
}

pub fn execute_shell(command: &str) -> Result<(), ExecutorError> {
    let mut cmd = Command::new("sh").arg("-c").arg(command).spawn()?;

    cmd.wait()?;

    Ok(())
}
