use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMode {
    #[default]
    Desktop,
    Terminal,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Authentication {
    #[default]
    Password,
    PrivateKey {
        path: PathBuf,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConnectionProfile {
    #[serde(default)]
    pub name: String,
    pub host: String,
    pub user: String,
    pub ssh_port: u16,
    pub open_browser: bool,
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub mode: ConnectionMode,
    #[serde(default)]
    pub authentication: Authentication,
}

impl Default for ConnectionProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            user: String::new(),
            ssh_port: 2222,
            open_browser: true,
            minimize_to_tray: true,
            mode: ConnectionMode::Desktop,
            authentication: Authentication::Password,
        }
    }
}

impl ConnectionProfile {
    pub fn ssh_user(&self) -> &str {
        match self.mode {
            ConnectionMode::Desktop => "landesk",
            ConnectionMode::Terminal => &self.user,
        }
    }
    pub fn normalized_host(&self) -> String {
        let host = self.host.trim();
        host.parse::<std::net::IpAddr>()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|_| host.trim_end_matches('.').to_ascii_lowercase())
    }
    pub fn id(&self) -> String {
        // Structured input makes IPv6/port boundaries unambiguous.
        let key = serde_json::to_vec(&(self.normalized_host(), self.ssh_port))
            .expect("string serialization");
        format!("{:x}", Sha256::digest(key))
    }
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.host
        } else {
            &self.name
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.name.len() <= 120 && !self.name.chars().any(char::is_control),
            "连接名称过长或含控制字符"
        );
        ensure!(
            !self.host.is_empty() && self.host.len() <= 253,
            "请填写服务器的 IP 或主机名"
        );
        ensure!(
            self.host.parse::<std::net::IpAddr>().is_ok()
                || self
                    .host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c)),
            "服务器地址应为 IP 或主机名，不含协议、端口或空格"
        );
        ensure!(
            self.mode == ConnectionMode::Desktop
                || (!self.user.is_empty()
                    && self.user.len() <= 256
                    && !self
                        .user
                        .chars()
                        .any(|c| c.is_control() || c.is_whitespace())),
            "请填写有效的 SSH 用户名"
        );
        ensure!(self.ssh_port != 0, "SSH 端口必须为 1–65535");
        if let Authentication::PrivateKey { path } = &self.authentication {
            ensure!(
                path.is_absolute(),
                "请选择私钥文件的绝对路径，不支持 ~ 缩写"
            );
        }
        Ok(())
    }
}

pub enum Event {
    Status(String),
    Ready,
    Stopped(anyhow::Result<StopReason>),
}
#[derive(Debug, PartialEq, Eq)]
pub enum StopReason {
    Disconnected,
    IdleTimeout,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_identity_and_terminal_account_have_distinct_rules() {
        let mut profile = ConnectionProfile {
            host: "example.test".into(),
            ..Default::default()
        };
        profile.validate().unwrap();
        assert_eq!(profile.ssh_user(), "landesk");
        profile.mode = ConnectionMode::Terminal;
        assert!(profile.validate().is_err());
        profile.user = "developer".into();
        profile.validate().unwrap();
        assert_eq!(profile.ssh_user(), "developer");
    }
    #[test]
    fn malformed_endpoint_and_relative_private_key_are_rejected() {
        let mut profile = ConnectionProfile {
            host: "example.test".into(),
            ..Default::default()
        };
        for host in ["", "ssh://example.test", "example.test:2222", "has space"] {
            profile.host = host.into();
            assert!(profile.validate().is_err());
        }
        profile.host = "::1".into();
        profile.validate().unwrap();
        profile.ssh_port = 0;
        assert!(profile.validate().is_err());
        profile.ssh_port = 2222;
        profile.authentication = Authentication::PrivateKey {
            path: "relative-key".into(),
        };
        assert!(profile.validate().is_err());
    }
}
