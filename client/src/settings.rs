use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub name: String,
    pub host: String,
    pub port: u16,
    pub open_browser: bool,
    pub minimize_to_tray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: 17891,
            open_browser: true,
            minimize_to_tray: true,
        }
    }
}

impl Settings {
    pub fn normalized_host(&self) -> String {
        let host = self.host.trim();
        host.parse::<std::net::IpAddr>()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|_| host.trim_end_matches('.').to_ascii_lowercase())
    }
    pub fn id(&self) -> String {
        // Structured input makes IPv6/port boundaries unambiguous.
        let key =
            serde_json::to_vec(&(self.normalized_host(), self.port)).expect("string serialization");
        format!("{:x}", Sha256::digest(key))
    }
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.host
        } else {
            &self.name
        }
    }
    pub fn viewer_url(&self) -> String {
        let name = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", self.label())
            .finish();
        format!("http://127.0.0.1:17890/s/{}/#{name}", self.id())
    }
    pub fn directory() -> Result<PathBuf> {
        Ok(dirs::config_dir()
            .context("无法找到当前用户配置目录")?
            .join("LanDeskClient"))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.name.len() <= 120 && !self.name.chars().any(char::is_control),
            "连接名称过长或含控制字符"
        );
        ensure!(
            !self.host.is_empty() && self.host.len() <= 253,
            "请填写远程电脑的 IP 或主机名"
        );
        ensure!(
            self.host.parse::<std::net::IpAddr>().is_ok()
                || self
                    .host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c)),
            "服务器地址应为 IP 或主机名，不含协议、端口或空格"
        );
        ensure!(self.port != 0, "连接端口必须为 1–65535");
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Connections {
    pub version: u32,
    pub profiles: Vec<Settings>,
    pub minimize_to_tray: bool,
}
impl Default for Connections {
    fn default() -> Self {
        Self {
            version: 2,
            profiles: Vec::new(),
            minimize_to_tray: true,
        }
    }
}
impl Connections {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let config: Self = serde_json::from_slice(&bytes)
                    .context("连接列表格式错误，请检查 connections.json")?;
                config.validate()?;
                Ok(config)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).context("无法读取连接列表"),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 2, "不支持的连接列表版本");
        let mut ids = std::collections::HashSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            ensure!(
                ids.insert(profile.id()),
                "同一个 IP/主机名和连接端口只能保存一条连接"
            );
        }
        Ok(())
    }
    pub fn replace(&mut self, old: Option<&str>, mut profile: Settings) -> Result<String> {
        profile.host = profile.normalized_host();
        profile.validate()?;
        let id = profile.id();
        ensure!(
            !self
                .profiles
                .iter()
                .any(|p| p.id() == id && Some(p.id().as_str()) != old),
            "该 IP/主机名和连接端口已经存在"
        );
        if let Some(old) = old {
            let index = self
                .profiles
                .iter()
                .position(|p| p.id() == old)
                .context("连接已不存在")?;
            self.profiles[index] = profile;
        } else {
            self.profiles.push(profile);
        }
        Ok(id)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path.parent().context("设置路径无效")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file().sync_all()?;
        file.persist(path).context("无法原子保存连接列表")?;
        Ok(())
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    fn profile(host: &str) -> Settings {
        Settings {
            host: host.into(),
            ..Settings::default()
        }
    }
    #[test]
    fn identity_normalizes_addresses_and_excludes_label() {
        let a = profile("2001:0db8:0:0:0:0:0:1");
        let mut b = profile("2001:db8::1");
        b.name = "别名".into();
        assert_eq!(a.id(), b.id());
        assert_eq!(a.id().len(), 64);
        assert_eq!(profile("Mac.LOCAL.").id(), profile("mac.local").id());
        let mut config = Connections::default();
        config.replace(None, profile("Mac.LOCAL.")).unwrap();
        assert_eq!(config.profiles[0].host, "mac.local");
        assert!(config.replace(None, profile("mac.local")).is_err());
        b.port = 2222;
        assert_ne!(a.id(), b.id());
    }
    #[test]
    fn add_edit_delete_reject_duplicates_and_persist_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let mut config = Connections::default();
        let a = config.replace(None, profile("192.0.2.1")).unwrap();
        config.replace(None, profile("192.0.2.2")).unwrap();
        let before = config.clone();
        assert!(config.replace(Some(&a), profile("192.0.2.2")).is_err());
        assert_eq!(config, before);
        config.replace(Some(&a), profile("192.0.2.3")).unwrap();
        config.save(&path).unwrap();
        assert_eq!(Connections::load(&path).unwrap(), config.clone());
        config.profiles.remove(0);
        config.save(&path).unwrap();
        assert_eq!(Connections::load(&path).unwrap().profiles.len(), 1);
    }
    #[test]
    fn unsupported_or_corrupt_config_is_reported_and_invalid_edit_preserves_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let mut config = Connections::default();
        config.replace(None, profile("192.0.2.1")).unwrap();
        config.save(&path).unwrap();
        let saved = std::fs::read(&path).unwrap();
        config.profiles[0].host = "https://bad:22".into();
        assert!(config.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        assert!(!String::from_utf8(saved).unwrap().contains("password"));
        std::fs::write(
            &path,
            br#"{"version":1,"profiles":[],"minimize_to_tray":true}"#,
        )
        .unwrap();
        assert!(Connections::load(&path).is_err());
        std::fs::write(&path, b"{}").unwrap();
        assert!(Connections::load(&path).is_err());
    }
}
