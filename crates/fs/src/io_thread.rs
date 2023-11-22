use crate::{ensure_file, EnsureFileError};
use derive_more::{Display, Error};
use pacquet_task_queue::{Task, TaskQueue};
use std::path::PathBuf;

/// Dedicated thread for I/O operations.
pub type IoThread = TaskQueue<IoTask>;

/// Operation to run on [`IoThread`].
#[derive(Debug)]
#[non_exhaustive]
pub enum IoTask {
    EnsureFile { file_path: PathBuf, content: Vec<u8>, mode: Option<u32> },
}

#[derive(Debug, Display, Error)]
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
