#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
fn main() {
    ui::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LanDeskClient 界面仅支持 Windows；其他平台可运行连接模块测试。");
}
