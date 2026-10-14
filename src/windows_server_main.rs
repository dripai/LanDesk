#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    landesk::platform::windows::run()
}
#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("LanDeskServer requires Windows x64")
}
