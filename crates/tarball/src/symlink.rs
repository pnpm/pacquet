use std::{io, os, path::PathBuf};

#[cfg(unix)]
pub fn symlink_dir(original: &PathBuf, link: &PathBuf) -> io::Result<()> {
    os::unix::fs::symlink(original, link)
}

#[cfg(windows)]
pub fn symlink_dir(original: &PathBuf, link: &PathBuf) -> io::Result<()> {
    os::windows::fs::symlink_dir(original, link);
}
