//! Local-only SSH administration. No HTTP/WebSocket route exposes this module.
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    process::Command,
    time::Duration,
};

pub fn parse_port(value: &str) -> Result<u16> {
    let port: u16 = value.trim().parse().context("SSH 端口必须为 1–65535")?;
    ensure!(
        port != 0 && port != crate::protocol::PORT,
        "SSH 端口不能为 0 或 LanDesk 使用的 17890"
    );
    Ok(port)
}

pub fn probe(port: u16) -> Result<()> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut socket =
        TcpStream::connect_timeout(&address, Duration::from_secs(2)).context("SSH 端口不可达")?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut banner = [0; 4];
    socket
        .read_exact(&mut banner)
        .context("未收到 SSH 服务响应")?;
    ensure!(&banner == b"SSH-", "目标端口不是 SSH 服务");
    Ok(())
}

pub fn current_port() -> Result<u16> {
    crate::platform::ssh::current_port()
}
pub fn request_change(port: u16) -> Result<()> {
    parse_port(&port.to_string())?;
    crate::platform::ssh::request_change(port)
}

pub fn run_admin_command() -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Ok(false);
    }
    ensure!(
        args.len() == 2 && args[0] == "--apply-ssh-port",
        "不支持的启动参数"
    );
    crate::platform::ssh::apply(parse_port(&args[1])?)?;
    Ok(true)
}

pub fn command(command: &mut Command) -> Result<String> {
    let output = command.output().context("系统命令无法启动")?;
    let stdout = String::from_utf8(output.stdout).context("系统命令输出不是 UTF-8")?;
    ensure!(
        output.status.success(),
        "系统命令失败：{} {}",
        stdout.trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(stdout)
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().context("配置目录无效")?)?;
    temporary
        .as_file()
        .set_permissions(std::fs::metadata(path)?.permissions())?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    #[cfg(not(target_os = "windows"))]
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("保存 SSH 配置失败")?;
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
        // ReplaceFile opens the replacement with no sharing. Close our writable
        // handle first, but keep TempPath ownership for cleanup on failure.
        let temporary = temporary.into_temp_path();
        let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        // ReplaceFile preserves the original file's ACL; do not replace it with
        // the temporary file's inherited permissions.
        ensure!(
            unsafe {
                ReplaceFileW(
                    target.as_ptr(),
                    source.as_ptr(),
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            } != 0,
            "保存 SSH 配置失败：{}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Keep an on-disk backup through restart and verification. Failed rollback is
/// reported with its path; it must never be mistaken for a successful save.
pub fn transaction(
    path: &Path,
    replacement: &[u8],
    mut restart: impl FnMut() -> Result<()>,
    mut verify: impl FnMut() -> Result<()>,
) -> Result<()> {
    let original = std::fs::read(path)?;
    let backup = path.with_extension("landesk-backup");
    let mut saved = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .context("SSH 备份已存在或无法创建；请先检查上次修改结果")?;
    saved.set_permissions(std::fs::metadata(path)?.permissions())?;
    saved.write_all(&original)?;
    saved.sync_all()?;
    drop(saved);
    let result = atomic_write(path, replacement)
        .and_then(|_| restart())
        .and_then(|_| verify());
    if let Err(error) = result {
        let rollback = atomic_write(path, &original).and_then(|_| restart());
        if let Err(rollback) = rollback {
            bail!(
                "修改失败：{error:#}；回滚未完成：{rollback:#}。原配置保存在 {}，请在本机恢复",
                backup.display()
            );
        }
        std::fs::remove_file(&backup)?;
        bail!("修改失败，已恢复原配置：{error:#}");
    }
    std::fs::remove_file(&backup).context("端口已应用，但无法清理备份")?;
    Ok(())
}

pub fn wait_for_ssh(port: u16) -> Result<()> {
    for _ in 0..10 {
        if probe(port).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    probe(port)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_write_replaces_existing_contents_and_cleans_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config");
        std::fs::write(&path, "original").unwrap();
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn rejects_invalid_or_conflicting_ports() {
        for value in ["0", "65536", "17890", "22;id", "-1"] {
            assert!(parse_port(value).is_err());
        }
        assert_eq!(parse_port(" 2222 ").unwrap(), 2222);
    }
    #[test]
    fn failed_restart_restores_file_and_failed_rollback_preserves_backup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config");
        std::fs::write(&path, "old").unwrap();
        let mut calls = 0;
        let error = transaction(
            &path,
            b"new",
            || {
                calls += 1;
                if calls == 1 {
                    bail!("restart failed");
                }
                Ok(())
            },
            || Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("已恢复原配置"), "{error:#}");
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert!(!path.with_extension("landesk-backup").exists());
        assert!(transaction(&path, b"new", || bail!("restart failed"), || Ok(())).is_err());
        assert_eq!(
            std::fs::read(path.with_extension("landesk-backup")).unwrap(),
            b"old"
        );
    }
}
