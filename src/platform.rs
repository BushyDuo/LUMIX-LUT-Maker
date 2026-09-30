use std::path::Path;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::ffi::OsString;

#[cfg(target_os = "macos")]
pub const FILE_MANAGER_LABEL: &str = "在 Finder 中显示";

#[cfg(target_os = "windows")]
pub const FILE_MANAGER_LABEL: &str = "在资源管理器中显示";

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const FILE_MANAGER_LABEL: &str = "打开所在文件夹";

#[cfg(target_os = "macos")]
pub fn reveal_file(path: &Path) -> std::io::Result<()> {
    Command::new("open").arg("-R").arg(path).spawn().map(drop)
}

#[cfg(target_os = "windows")]
pub fn reveal_file(path: &Path) -> std::io::Result<()> {
    let mut select = OsString::from("/select,");
    select.push(path.as_os_str());
    Command::new("explorer.exe").arg(select).spawn().map(drop)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn reveal_file(path: &Path) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or(path);
    Command::new("xdg-open").arg(directory).spawn().map(drop)
}
