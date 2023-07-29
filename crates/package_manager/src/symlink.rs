use std::{io, os, path::Path};

#[cfg(unix)]
pub fn symlink_dir<P: AsRef<Path>>(original: P, link: P) -> io::Result<()> {
    os::unix::fs::symlink(original, link)
}

#[cfg(windows)]
pub fn symlink_dir<P: AsRef<Path>>(original: P, link: P) -> io::Result<()> {
    os::windows::fs::symlink_dir(original, link)
}
