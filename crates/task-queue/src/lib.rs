use std::{fmt::Debug, future::IntoFuture};
use tokio::{
    sync::{
        mpsc::{self, error::SendError as MpscSendError},
        oneshot,
    },
    task::JoinHandle,
    task::{spawn, JoinError},
};

/// Value to sent in the command channel.
type Command<Task> = (Task, oneshot::Sender<<Task as self::Task>::Output>);

/// Handle of a blocking task queue.
#[derive(Debug)]
pub struct TaskQueue<Task: self::Task> {
    handle: JoinHandle<()>,
    command_sender: mpsc::UnboundedSender<Command<Task>>,
}

impl<Task> TaskQueue<Task>
where
    Task: self::Task + Send + 'static,
    Task::Output: Debug + Send + 'static,
{
    /// Spawn a new task queue.
    pub fn spawn() -> Self {
        let (command_sender, mut command_receiver) = mpsc::unbounded_channel::<Command<Task>>();
        let handle = spawn(async move {
            while let Some((task, response_sender)) = command_receiver.recv().await {
                response_sender.send(task.run()).expect("send value to oneshot channel");
            }
        });
        TaskQueue { handle, command_sender }
    }

    /// Send a task to the task queue, get a oneshot receiver that listens to the return value of the sent task.
    pub fn send_and_listen(
        &self,
        task: Task,
    ) -> Result<oneshot::Receiver<Task::Output>, MpscSendError<Command<Task>>> {
        let (response_sender, response_receiver) = oneshot::channel::<Task::Output>();
        self.command_sender.send((task, response_sender))?;
        Ok(response_receiver)
    }
}

/// Wait for the task queue to finish.
impl<Task: self::Task> IntoFuture for TaskQueue<Task> {
    type IntoFuture = JoinHandle<()>;
    type Output = Result<(), JoinError>;
    fn into_future(self) -> Self::IntoFuture {
        self.handle
    }
}

/// Task to be sent to the [`TaskQueue`].
pub trait Task {
    type Output;
    fn run(self) -> Self::Output;
}
