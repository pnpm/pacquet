use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{IoSendError, IoSendValue, IoTask, IoThread};
use pipe_trait::Pipe;
use std::{
    iter,
    path::{Path, PathBuf},
};

/// Value type of [`link_file`].
type LinkFileValue = Box<dyn Iterator<Item = IoSendValue>>;

/// Error type for [`link_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum LinkFileError {
    #[display("Fail to send command to create directory at {dirname:?}: {error}")]
    SendCreateDir {
        dirname: PathBuf,
        #[error(source)]
        error: IoSendError,
    },
    #[display("Fail to send command to create a link from {from:?} to {to:?}: {error}")]
    SendCreateLink {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: IoSendError,
    },
}

/// Reflink or copy a single file.
///
/// * If `target_link` already exists, do nothing.
/// * If parent dir of `target_link` doesn't exist, it will be created.
pub fn link_file(
    io_thread: &IoThread,
    source_file: &Path,
    target_link: &Path,
) -> Result<LinkFileValue, LinkFileError> {
    if target_link.exists() {
        return iter::empty().pipe(|x| Box::new(x) as LinkFileValue).pipe(Ok);
    }

    let create_dir_receiver = target_link
        .parent()
        .map(|parent_dir| {
            io_thread
                .send_and_listen(IoTask::CreateDirAll { dir_path: parent_dir.to_path_buf() })
                .map_err(|error| LinkFileError::SendCreateDir {
                    dirname: parent_dir.to_path_buf(),
                    error,
                })
        })
        .transpose()?;

    // TODO: add hardlink (https://github.com/pnpm/pacquet/issues/174)
    // NOTE: do not hardlink packages with postinstall

    let create_link_receiver = io_thread
        .send_and_listen(IoTask::ReflinkOrCopy {
            source_file: source_file.to_path_buf(),
            target_link: target_link.to_path_buf(),
        })
        .map_err(|error| LinkFileError::SendCreateLink {
            from: source_file.to_path_buf(),
            to: target_link.to_path_buf(),
            error,
        })?;

    create_dir_receiver
        .into_iter()
        .chain(iter::once(create_link_receiver))
        .pipe(|x| Box::new(x) as LinkFileValue)
        .pipe(Ok)
}
