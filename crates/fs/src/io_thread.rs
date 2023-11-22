use pacquet_task_queue::{Task, TaskQueue};
use std::io;

/// Dedicated thread for I/O operations.
pub type IoThread = TaskQueue<Operation>;

/// Operation to run on [`IoThread`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Operation {}

impl Task for Operation {
    type Output = io::Result<()>;
    fn run(self) -> Self::Output {
        match self {}
    }
}
