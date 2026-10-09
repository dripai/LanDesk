use crate::ssh_config::{self, command};
use anyhow::{Context, Result, ensure};
use std::{path::PathBuf, process::Command, ptr};
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
    Ok(ports.first().copied().unwrap_or(22)) // OpenSSH's documented default.
}
pub fn current_port() -> Result<u16> {
    configured_port(
        &std::fs::read_to_string(config_path()?)
            .context("未找到 Windows OpenSSH Server 配置，请先安装并启用该服务")?,
    )
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
    ssh_config::probe(port)
}
pub fn apply(port: u16) -> Result<()> {
    let path = config_path()?;
    let original = std::fs::read_to_string(&path)?;
    if configured_port(&original)? == port {
        return ssh_config::probe(port);
    }
    drop(std::net::TcpListener::bind(("0.0.0.0", port)).context("新端口已被占用")?);
    let new = replacement(&original, port)?;
    ssh_config::transaction(
        &path,
        new.as_bytes(),
        || {
            command(
                Command::new(
                    known_folder(&FOLDERID_System)?.join("WindowsPowerShell/v1.0/powershell.exe"),
                )
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "$ErrorActionPreference='Stop'; Restart-Service -Name sshd -ErrorAction Stop",
                ]),
            )?;
            ssh_config::wait_for_ssh(current_port()?)
        },
        || ssh_config::wait_for_ssh(port),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
