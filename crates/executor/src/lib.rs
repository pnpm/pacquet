use derive_more::{Display, Error};
use miette::Diagnostic;
use std::process::Command;

#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum ExecutorError {
    #[display("Failed to spawn command: {_0}")]
    #[diagnostic(code(pacquet_executor::spawn_command))]
    SpawnCommand(#[error(source)] std::io::Error),

    #[display("Process exits with an error: {_0}")]
    #[diagnostic(code(pacquet_executor::wait_process))]
    WaitProcess(#[error(source)] std::io::Error),
}

pub fn execute_shell(command: &str) -> Result<(), ExecutorError> {
    let mut cmd =
        Command::new("sh").arg("-c").arg(command).spawn().map_err(ExecutorError::SpawnCommand)?;

    cmd.wait().map_err(ExecutorError::WaitProcess)?;

    Ok(())
}
