use std::{io, path::Path};
use walkdir::WalkDir;

pub fn get_filenames_in_folder(path: &Path) -> Vec<String> {
    let mut files = std::fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();

    files.sort();
    files
}

fn normalized_suffix(path: &Path, prefix: &Path) -> String {
    path.strip_prefix(prefix)
        .expect("strip prefix from path")
        .to_str()
        .expect("convert suffix to UTF-8")
        .replace('\\', "/")
}

pub fn get_all_folders(root: &Path) -> Vec<String> {
    WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.expect("access entry"))
        .filter(|entry| entry.file_type().is_dir() || entry.file_type().is_symlink())
        .map(|entry| normalized_suffix(entry.path(), root))
        .filter(|suffix| !suffix.is_empty())
        .collect()
}

pub fn get_all_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry.unwrap();
        let entry_path = entry.path();

        if entry.file_type().is_dir() {
            continue;
        }

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
    files.sort();
    files
}

// Helper function to check if a path is a symlink or junction
pub fn is_symlink_or_junction(path: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    return junction::exists(path);

    #[cfg(not(windows))]
    return Ok(path.is_symlink());
}
