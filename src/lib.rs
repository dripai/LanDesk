#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("LanDesk 服务端支持 macOS 和 Windows；Linux 后端尚未实现。");

pub mod access;
mod clipboard_image;
pub mod desktop;
pub mod file_worker;
mod files;
pub mod platform;
pub mod protocol;
pub mod server;
pub mod transport;
