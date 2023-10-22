#[cfg(unix)]
pub const SCRIPT_EXECUTOR: &str = "bash";
#[cfg(windows)]
pub const SCRIPT_EXECUTOR: &str = "powershell";
