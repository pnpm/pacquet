use derive_more::{Display, Error, From};
use miette::Diagnostic;
use std::process::Command;

#[derive(Debug, Display, Error, Diagnostic, From)]
#[non_exhaustive]
pub enum ExecutorError {
    #[diagnostic(code(pacquet_executor::io_error))]
    Io(#[error(source)] std::io::Error),
}

pub fn execute_shell(command: &str) -> Result<(), ExecutorError> {
    let mut cmd = Command::new("sh").arg("-c").arg(command).spawn()?;

    cmd.wait()?;

    Ok(())
}
