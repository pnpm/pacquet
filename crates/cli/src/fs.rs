use std::{io, path::Path};

#[cfg(windows)]
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

#[cfg(unix)]
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

#[cfg(windows)]
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    match std::os::windows::fs::symlink_dir(original, link) {
        Ok(_) => Ok(()),
        Err(e) => {
            match e.raw_os_error() {
                // If we don't have the privilege to create a symlink, we can try to create a junction instead.
                Some(ERROR_PRIVILEGE_NOT_HELD) => junction::create(original, link),
                _ => Err(e),
            }
        }
    }
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
pub fn get_all_folders(root: &std::path::Path) -> Vec<String> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        let entry_path = entry.path();
        if entry.file_type().is_dir() || entry.file_type().is_symlink() {
            // We need this mutation to ensure that both Unix and Windows paths resolves the same.
            // TODO: Find a better way to do this?
            let simple_path = entry_path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_str().expect("invalid UTF-8"))
                .collect::<Vec<_>>()
                .join("/");

            if !simple_path.is_empty() {
                files.push(simple_path);
            }
        }
    }
    files.sort();
    files
}
