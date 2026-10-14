#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    landesk::platform::windows::virtual_display::run()
}
#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("LanDeskDisplay requires Windows x64")
}
