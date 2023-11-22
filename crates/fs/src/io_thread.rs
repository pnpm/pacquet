use pacquet_task_queue::{Task, TaskQueue};
use std::io;

/// Dedicated thread for I/O operations.
pub type IoThread = TaskQueue<IoTask>;

/// Operation to run on [`IoThread`].
#[derive(Debug)]
#[non_exhaustive]
pub enum IoTask {}

impl Task for IoTask {
    type Output = io::Result<()>;
    fn run(self) -> Self::Output {
        match self {}
    }
}
