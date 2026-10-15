#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    landesk::bootstrap::macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("Windows 服务端请使用 LanDeskServer.exe")
}
