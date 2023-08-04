use std::{io, os, path::Path};

#[cfg(unix)]
pub fn symlink_dir<P: AsRef<Path>>(original: P, link: P) -> io::Result<()> {
    os::unix::fs::symlink(original, link)
}

#[cfg(windows)]
pub fn symlink_dir<P: AsRef<Path>>(original: P, link: P) -> io::Result<()> {
    os::windows::fs::symlink_dir(original, link)
}

#[cfg(test)]
pub fn get_filenames_in_folder(path: &Path) -> Vec<String> {
    let mut files = std::fs::read_dir(path)
        .unwrap()
        .into_iter()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();

    files.sort();
    files
}
