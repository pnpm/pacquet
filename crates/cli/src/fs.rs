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
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();

    files.sort();
    files
}

#[cfg(test)]
pub fn get_all_folders(root: &std::path::PathBuf) -> Vec<String> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        let entry_path = entry.path().to_path_buf();
        if entry.file_type().is_dir() || entry.file_type().is_symlink() {
            // We need this mutation to ensure that both Unix and Windows paths resolves the same.
            // TODO: Find a better way to do this?
            let simple_path = entry_path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/");

            if !simple_path.is_empty() {
                files.push(simple_path.as_os_string());
            }
        }
    }
    files.sort();
    files
}


