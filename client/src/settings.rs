use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub host: String,
    pub user: String,
    pub ssh_port: u16,
    pub open_browser: bool,
    pub minimize_to_tray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: String::new(),
            user: String::new(),
            ssh_port: 22,
            open_browser: true,
            minimize_to_tray: true,
        }
    }
}

impl Settings {
    pub fn directory() -> Result<PathBuf> {
        Ok(dirs::config_dir()
            .context("无法找到当前用户配置目录")?
            .join("LanDeskClient"))
    }
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(data) => {
                let settings: Self = serde_json::from_slice(&data)
                    .context("客户端设置文件损坏，请修复或移走 settings.json")?;
                settings.validate()?;
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("无法读取客户端设置"),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.host.is_empty() && self.host.len() <= 253,
            "请填写 Mac 的 IP 或主机名"
        );
        ensure!(
            self.host.parse::<std::net::IpAddr>().is_ok()
                || self
                    .host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c)),
            "Mac 地址应为 IP 或主机名，不含协议、端口或空格"
        );
        ensure!(
            !self.user.is_empty()
                && self.user.len() <= 256
                && !self
                    .user
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace()),
            "请填写有效的 Mac 用户名"
        );
        ensure!(self.ssh_port != 0, "SSH 端口必须为 1–65535");
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path.parent().context("设置路径无效")?;
        std::fs::create_dir_all(parent).context("无法创建设置目录")?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file().sync_all()?;
        file.persist(path).context("无法原子保存客户端设置")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Settings {
        Settings {
            host: "192.0.2.10".into(),
            user: "developer".into(),
            ..Settings::default()
        }
    }
    #[test]
    fn settings_round_trip_and_replacement_do_not_include_password() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let mut value = sample();
        value.save(&path).unwrap();
        value.ssh_port = 2222;
        value.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), value);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("password"));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
    #[test]
    fn invalid_settings_preserve_existing_file_and_corruption_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let mut value = sample();
        value.save(&path).unwrap();
        value.host = "https://mac:22".into();
        assert!(value.save(&path).is_err());
        assert_eq!(Settings::load(&path).unwrap(), sample());
        std::fs::write(&path, b"{}").unwrap();
        assert!(Settings::load(&path).is_err());
    }
}
