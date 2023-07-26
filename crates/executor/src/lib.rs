use std::process::Command;

use thiserror::Error;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ExecutorError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn execute_shell(command: &str) -> Result<(), ExecutorError> {
    let mut cmd = Command::new(command).spawn()?;

    cmd.wait()?;

    Ok(())
}
