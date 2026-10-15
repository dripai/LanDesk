//! Operational logs only: never pass credentials, input text or raw peer packets here.
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;

const LIMIT: u64 = 2_000_000;
static OUTPUT: OnceLock<Mutex<std::path::PathBuf>> = OnceLock::new();
static WRITE: Mutex<()> = Mutex::new(());

pub fn initialize(path: std::path::PathBuf) -> anyhow::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let output = OUTPUT.get_or_init(|| Mutex::new(path.clone()));
    *output.lock().unwrap_or_else(|p| p.into_inner()) = path;
    Ok(())
}

fn append(path: &Path, line: &str, limit: u64) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if file.metadata()?.len() + line.len() as u64 > limit {
        // Copy into an existing ACL-protected file: LocalService cannot rename in the parent.
        let old = std::fs::read(path)?;
        std::fs::write(path.with_extension("previous.log"), old)?;
        drop(file);
        // FILE_APPEND_DATA on Windows does not grant the right to truncate the file.
        std::fs::write(path, [])?;
    }
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(line.as_bytes())
}

pub fn log(message: &str) {
    let Some(path) = OUTPUT.get() else {
        eprintln!("{message}");
        return;
    };
    let path = path.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let _guard = WRITE.lock().unwrap_or_else(|p| p.into_inner());
    let message = &message[..message.floor_char_boundary(64 * 1024)];
    let line = format!(
        "{} pid={} version={} {}\n",
        timestamp(),
        std::process::id(),
        env!("CARGO_PKG_VERSION"),
        message.replace(['\r', '\n'], " ")
    );
    if let Err(error) = append(&path, &line, LIMIT) {
        eprintln!("Cannot write log {}: {error}", path.display());
    }
}

#[cfg(windows)]
fn timestamp() -> String {
    let mut time = windows_sys::Win32::Foundation::SYSTEMTIME::default();
    unsafe { windows_sys::Win32::System::SystemInformation::GetSystemTime(&mut time) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds
    )
}
#[cfg(not(windows))]
fn timestamp() -> String {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", time.as_secs(), time.subsec_millis())
}

#[cfg(test)]
mod tests {
    #[test]
    fn rotation_retains_recent_errors_and_previous_permissions() {
        let directory =
            std::env::temp_dir().join(format!("landesk-log-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("server.log");
        super::append(&path, "first\n", 12).unwrap();
        super::append(&path, "second\n", 12).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        assert_eq!(
            std::fs::read_to_string(path.with_extension("previous.log")).unwrap(),
            "first\n"
        );
        super::append(&path, "third\n", 12).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "third\n");
        assert_eq!(
            std::fs::read_to_string(path.with_extension("previous.log")).unwrap(),
            "second\n"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
