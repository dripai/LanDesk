mod bootstrap;
pub(super) mod broker;
mod dxgi;
mod install;
pub(super) mod service;
mod sys;
mod virtual_display;
pub(super) mod wire;
pub(super) mod worker;

/// Dispatch before settings/UI initialization so SYSTEM processes never open user files.
pub fn startup() -> anyhow::Result<bool> {
    match std::env::args().nth(1).as_deref() {
        Some("--desktop-service") => {
            service::run()?;
            Ok(true)
        }
        Some("--desktop-worker") => {
            worker::run()?;
            Ok(true)
        }
        Some("--install-desktop-service") => {
            install::install()?;
            Ok(true)
        }
        Some("--uninstall-desktop-service") => {
            install::uninstall()?;
            Ok(true)
        }
        None => install::launch(),
        Some(argument) => anyhow::bail!("未知启动参数：{argument}"),
    }
}
pub fn background() -> bool {
    matches!(
        std::env::args().nth(1).as_deref(),
        Some("--desktop-service" | "--desktop-worker")
    )
}

mod worker_io;
