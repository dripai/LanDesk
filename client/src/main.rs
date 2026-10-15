#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod tray;
#[cfg(any(windows, target_os = "macos"))]
mod ui;

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    ui::run();
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("LanDeskClient 界面支持 Windows 和 macOS；其他平台可运行连接模块测试。");
}
