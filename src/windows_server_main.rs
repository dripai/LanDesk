#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    if std::env::current_exe()?.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .eq_ignore_ascii_case("LanDeskDisplay.exe")
    }) {
        return landesk::platform::windows::virtual_display::run();
    }
    landesk::platform::windows::run()
}
#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("LanDeskServer requires Windows x64")
}
