use crate::ssh_config::{self, command};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    process::Command,
    ptr,
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::{
        Com::CoTaskMemFree,
        Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
    },
    UI::Shell::{
        FOLDERID_ProgramData, FOLDERID_System, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        SHGetKnownFolderPath, ShellExecuteExW,
    },
};

pub const DEFAULT_PORT: u16 = 22;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn known_folder(id: &windows_sys::core::GUID) -> Result<PathBuf> {
    let mut value = ptr::null_mut();
    ensure!(
        unsafe { SHGetKnownFolderPath(id, 0, ptr::null_mut(), &mut value) } >= 0,
        "无法定位系统 SSH 配置目录"
    );
    let path = unsafe {
        let mut len = 0;
        while *value.add(len) != 0 {
            len += 1;
        }
        let path = String::from_utf16(std::slice::from_raw_parts(value, len));
        CoTaskMemFree(value.cast());
        path?
    };
    Ok(PathBuf::from(path))
}
fn config_path() -> Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramData)?.join("ssh/sshd_config"))
}
fn read_configuration(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "未找到 SSH 配置：{}。请先安装 OpenSSH Server 并启动 sshd 服务；默认端口为 {DEFAULT_PORT}。",
                path.display()
            )
        } else {
            format!("无法读取 SSH 配置：{}，请检查文件权限和 UTF-8 编码。", path.display())
        };
        anyhow::Error::new(error).context(message)
    })
}

fn powershell_path() -> Result<PathBuf> {
    let path = known_folder(&FOLDERID_System)?.join("WindowsPowerShell/v1.0/powershell.exe");
    let metadata = std::fs::metadata(&path).with_context(|| {
        format!(
            "未找到 Windows PowerShell：{}，无法重启 SSH 服务",
            path.display()
        )
    })?;
    ensure!(
        metadata.is_file(),
        "Windows PowerShell 路径不是文件：{}",
        path.display()
    );
    Ok(path)
}

fn verify_listener(port: u16) -> Result<()> {
    ssh_config::probe(port).with_context(|| {
        format!("SSH 配置端口为 {port}，但服务未响应；请确认 OpenSSH Server 的 sshd 服务已启动")
    })
}
fn configured_port(text: &str) -> Result<u16> {
    let mut ports = Vec::new();
    for line in text.lines() {
        let words: Vec<_> = line.split('#').next().unwrap().split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        ensure!(
            !words[0].eq_ignore_ascii_case("Include"),
            "SSH 配置使用 Include，请由管理员修改实际配置文件"
        );
        if words[0].eq_ignore_ascii_case("Match") {
            break;
        }
        if words[0].eq_ignore_ascii_case("Port") {
            ensure!(words.len() == 2, "SSH Port 配置无效");
            ports.push(ssh_config::parse_port(words[1])?);
        }
    }
    ensure!(ports.len() <= 1, "SSH 同时监听多个端口，请由管理员明确配置");
    Ok(ports.first().copied().unwrap_or(DEFAULT_PORT)) // OpenSSH's documented default.
}
pub fn current_port() -> Result<u16> {
    configured_port(&read_configuration(&config_path()?)?)
}
fn replacement(text: &str, port: u16) -> Result<String> {
    configured_port(text)?;
    let mut out = format!("Port {port}\r\n");
    let mut in_match = false;
    for line in text.split_inclusive('\n') {
        let key = line.split_whitespace().next().unwrap_or("");
        if key.eq_ignore_ascii_case("Match") {
            in_match = true;
        }
        if !in_match && key.eq_ignore_ascii_case("Port") {
            continue;
        }
        out.push_str(line);
    }
    Ok(out)
}
pub fn request_change(port: u16) -> Result<()> {
    // Report missing/invalid configuration in the existing window, before UAC.
    // The displayed default is only an editable value, not proof SSH is running.
    if current_port()? == port {
        return verify_listener(port);
    }
    powershell_path()?;
    let exe = wide(
        std::env::current_exe()?
            .to_str()
            .context("程序路径不是 UTF-8")?,
    );
    let verb = wide("runas");
    let args = wide(&format!("--apply-ssh-port {port}"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: exe.as_ptr(),
        lpParameters: args.as_ptr(),
        nShow: 0,
        ..Default::default()
    };
    unsafe {
        ensure!(
            ShellExecuteExW(&mut info) != 0,
            "管理员授权取消或启动失败：{}",
            std::io::Error::last_os_error()
        );
        ensure!(!info.hProcess.is_null(), "未获得管理员进程句柄");
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut exit = 1;
        let ok = GetExitCodeProcess(info.hProcess, &mut exit);
        CloseHandle(info.hProcess);
        ensure!(
            ok != 0 && exit == 0,
            "修改 SSH 端口失败，请检查 OpenSSH 服务、配置和管理员权限"
        );
    }
    ensure!(current_port()? == port, "端口配置未保存");
    verify_listener(port)
}
pub fn apply(port: u16) -> Result<()> {
    let path = config_path()?;
    let original = read_configuration(&path)?;
    if configured_port(&original)? == port {
        return verify_listener(port);
    }
    // Resolve the executable before modifying any system configuration.
    let powershell = powershell_path()?;
    drop(std::net::TcpListener::bind(("0.0.0.0", port)).context("新端口已被占用")?);
    let new = replacement(&original, port)?;
    ssh_config::transaction(
        &path,
        new.as_bytes(),
        || {
            command(Command::new(&powershell).args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$ErrorActionPreference='Stop'; Restart-Service -Name sshd -ErrorAction Stop",
            ]))?;
            ssh_config::wait_for_ssh(current_port()?)
        },
        || ssh_config::wait_for_ssh(port),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_configuration_reports_path_without_creating_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ssh/sshd_config");
        let error = read_configuration(&path).unwrap_err().to_string();
        assert!(error.contains("OpenSSH Server"), "{error}");
        assert!(error.contains(path.to_str().unwrap()), "{error}");
        assert!(!path.exists());
        assert!(!root.path().join("ssh").exists());
    }
    #[test]
    fn existing_configuration_uses_default_or_preserves_custom_port() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sshd_config");
        std::fs::write(&path, "#Port 22\r\n").unwrap();
        assert_eq!(
            configured_port(&read_configuration(&path).unwrap()).unwrap(),
            DEFAULT_PORT
        );
        std::fs::write(&path, "Port 2222\r\n").unwrap();
        assert_eq!(
            configured_port(&read_configuration(&path).unwrap()).unwrap(),
            2222
        );
        std::fs::write(&path, "Port wrong\r\n").unwrap();
        assert!(configured_port(&read_configuration(&path).unwrap()).is_err());
    }
    #[test]
    fn ssh_port_change_preserves_authentication_and_match_blocks() {
        let old = "#Port 22\r\nPasswordAuthentication yes\r\nMatch Group administrators\r\n AuthorizedKeysFile __PROGRAMDATA__/ssh/admin\r\n";
        let new = replacement(old, 2222).unwrap();
        assert_eq!(configured_port(&new).unwrap(), 2222);
        assert!(new.contains("PasswordAuthentication yes"));
        assert!(new.contains("Match Group administrators"));
        assert!(configured_port("Port 22\nPort 2222\n").is_err());
        assert!(configured_port("Include other.conf\n").is_err());
    }
}
