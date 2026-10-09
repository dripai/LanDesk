use crate::ssh_config::{self, command};
use anyhow::{Context, Result, ensure};
use std::{net::TcpListener, path::Path, process::Command};

const SERVICES: &str = "/etc/services";
fn services_port(text: &str) -> Result<u16> {
    let values: Vec<_> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some("ssh"))
                .then(|| fields.next())
                .flatten()
                .filter(|field| field.ends_with("/tcp"))
        })
        .collect();
    ensure!(
        values.len() == 1,
        "系统 ssh 服务端口配置不唯一，无法自动修改"
    );
    ssh_config::parse_port(values[0].trim_end_matches("/tcp"))
}
pub fn current_port() -> Result<u16> {
    services_port(&std::fs::read_to_string(SERVICES)?)
}
fn replace_port(text: &str, port: u16) -> Result<String> {
    services_port(text)?;
    Ok(text
        .split_inclusive('\n')
        .map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.first() == Some(&"ssh")
                && fields
                    .get(1)
                    .is_some_and(|value| value.ends_with("/tcp") || value.ends_with("/udp"))
            {
                let protocol = fields[1].split_once('/').unwrap().1;
                line.replacen(fields[1], &format!("{port}/{protocol}"), 1)
            } else {
                line.to_owned()
            }
        })
        .collect())
}

pub fn request_change(port: u16) -> Result<()> {
    let exe = std::env::current_exe()?;
    let quoted = format!(
        "'{}' --apply-ssh-port {port}",
        exe.to_str()
            .context("程序路径不是 UTF-8")?
            .replace('\'', "'\\''")
    );
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        quoted.replace('\\', "\\\\").replace('"', "\\\"")
    );
    command(Command::new("/usr/bin/osascript").args(["-e", &script]))?;
    ensure!(current_port()? == port, "系统配置未保存");
    ssh_config::probe(port)
}

pub fn apply(port: u16) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "修改 SSH 端口需要管理员授权"
    );
    // The Apple launchd job resolves the 'ssh' service through /etc/services.
    // Refuse a different layout instead of guessing at an administrator's setup.
    let plist = command(Command::new("/usr/libexec/PlistBuddy").args([
        "-c",
        "Print :Sockets:Listeners:SockServiceName",
        "/System/Library/LaunchDaemons/ssh.plist",
    ]))?;
    ensure!(
        plist.trim() == "ssh",
        "系统 SSH 监听配置不是默认结构，需由管理员手动配置"
    );
    let status = command(
        Command::new("/usr/sbin/systemsetup")
            .env("LC_ALL", "C")
            .arg("-getremotelogin"),
    )?;
    ensure!(
        status.trim() == "Remote Login: On",
        "请先在系统设置中启用远程登录；此操作不自动开启 SSH"
    );
    if current_port()? == port {
        return ssh_config::probe(port);
    }
    let listener = TcpListener::bind(("0.0.0.0", port)).context("新端口已被占用")?;
    drop(listener);
    let original = std::fs::read_to_string(SERVICES)?;
    let replacement = replace_port(&original, port)?;
    ssh_config::transaction(
        Path::new(SERVICES),
        replacement.as_bytes(),
        || {
            // systemsetup explicitly requires Full Disk Access on current macOS.
            // Errors are surfaced and trigger rollback; no TCC changes are made here.
            command(Command::new("/usr/sbin/systemsetup").args(["-setremotelogin", "-f", "off"]))?;
            command(Command::new("/usr/sbin/systemsetup").args(["-setremotelogin", "on"]))?;
            ssh_config::wait_for_ssh(current_port()?)
        },
        || ssh_config::wait_for_ssh(port),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modifies_only_ssh_service_and_preserves_other_ports() {
        let old = "# services\nssh 22/tcp # SSH\nssh 22/udp\nhttp 80/tcp\n";
        let new = replace_port(old, 2222).unwrap();
        assert_eq!(services_port(&new).unwrap(), 2222);
        assert!(new.contains("http 80/tcp"));
        assert!(new.contains("ssh 2222/udp"));
        assert!(services_port("ssh 22/tcp\nssh 23/tcp").is_err());
    }
}
