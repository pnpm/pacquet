#[cfg(unix)]
pub const SCRIPT_EXECUTOR: &str = "bash";
#[cfg(windows)]
pub const SCRIPT_EXECUTOR: &str = "powershell";

#[cfg(unix)]
pub const SCRIPT_NAME: &str = "install.bash";
#[cfg(windows)]
pub const SCRIPT_NAME: &str = "install.ps1";
