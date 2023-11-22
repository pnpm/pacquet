use derive_more::{Display, Error};
use pacquet_task_queue::{Task, TaskQueue};

/// Dedicated thread for I/O operations.
pub type IoThread = TaskQueue<IoTask>;

/// Operation to run on [`IoThread`].
#[derive(Debug)]
#[non_exhaustive]
pub enum IoTask {}

#[derive(Debug, Display, Error)]
#[non_exhaustive]
pub enum IoTaskError {}

impl Task for IoTask {
    type Output = Result<(), IoTaskError>;
    fn run(self) -> Self::Output {
        match self {}
    }
}
