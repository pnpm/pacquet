use crate::{ensure_file, EnsureFileError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_task_queue::{SendError, SendResult, SendValue, Task, TaskQueue};
use std::path::PathBuf;

/// Dedicated thread for I/O operations.
pub type IoThread = TaskQueue<IoTask>;

/// Value to receive when it succeeds in sending an I/O task.
pub type IoSendValue = SendValue<IoTask>;

/// Value to receive when it fails to send an I/O task.
pub type IoSendError = SendError<IoTask>;

/// Result to receive when it attempts to send an I/O task.
pub type IoSendResult = SendResult<IoTask>;

/// Operation to run on [`IoThread`].
#[derive(Debug)]
#[non_exhaustive]
pub enum IoTask {
    EnsureFile { file_path: PathBuf, content: Vec<u8>, mode: Option<u32> },
}

/// Error type of [`IoTask`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum IoTaskError {
    EnsureFile(EnsureFileError),
}

impl Task for IoTask {
    type Output = Result<(), IoTaskError>;
    fn run(self) -> Self::Output {
        match self {
            IoTask::EnsureFile { file_path, content, mode } => {
                ensure_file(&file_path, &content, mode).map_err(IoTaskError::EnsureFile)
            }
        }
    }
}
