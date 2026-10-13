use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

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
pub struct Settings {
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

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            user: String::new(),
            ssh_port: 22,
            open_browser: true,
            minimize_to_tray: true,
            mode: ConnectionMode::Desktop,
            authentication: Authentication::Password,
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
            !self.user.is_empty()
                && self.user.len() <= 256
                && !self
                    .user
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace()),
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
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(json.get("password").is_none());
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
    #[test]
    fn legacy_defaults_and_terminal_key_settings_are_explicit() {
        let legacy: Settings = serde_json::from_str(
            r#"{"host":"example.test","user":"alice","ssh_port":22,"open_browser":true,"minimize_to_tray":true}"#,
        ).unwrap();
        assert_eq!(legacy.mode, ConnectionMode::Desktop);
        assert_eq!(legacy.authentication, Authentication::Password);
        let mut terminal = legacy;
        terminal.mode = ConnectionMode::Terminal;
        terminal.authentication = Authentication::PrivateKey {
            path: PathBuf::from("~/.ssh/id_ed25519"),
        };
        assert!(terminal.validate().is_err());
        let dir = tempfile::tempdir().unwrap();
        terminal.authentication = Authentication::PrivateKey {
            path: dir.path().join("id_ed25519"),
        };
        let path = dir.path().join("settings.json");
        terminal.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), terminal);
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
            version: 1,
            profiles: Vec::new(),
            minimize_to_tray: true,
        }
    }
}
impl Connections {
    // Read the previous single-profile format once; the caller commits its credential migration.
    pub fn load(path: &Path) -> Result<(Self, bool)> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), false));
            }
            Err(error) => return Err(error).context("无法读取连接列表"),
        };
        let json: serde_json::Value = serde_json::from_slice(&bytes).context("连接设置损坏")?;
        if json.get("version").is_some() {
            let config: Self = serde_json::from_value(json).context("连接列表格式错误")?;
            config.validate()?;
            Ok((config, false))
        } else {
            let mut old: Settings = serde_json::from_value(json).context("旧连接设置损坏")?;
            old.validate()?;
            old.name = old.host.clone();
            let config = Self {
                minimize_to_tray: old.minimize_to_tray,
                profiles: vec![old],
                ..Self::default()
            };
            Ok((config, true))
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "不支持的连接列表版本");
        let mut ids = std::collections::HashSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            ensure!(
                ids.insert(profile.id()),
                "同一个 IP/主机名和 SSH 端口只能保存一条连接"
            );
        }
        Ok(())
    }
    pub fn replace(&mut self, old: Option<&str>, mut profile: Settings) -> Result<String> {
        // Normalize the identity, but preserve the SSH host spelling so editing an
        // imported profile does not silently bypass its existing known_hosts entry.
        profile.host = profile.host.trim().to_owned();
        profile.validate()?;
        let id = profile.id();
        ensure!(
            !self
                .profiles
                .iter()
                .any(|p| p.id() == id && Some(p.id().as_str()) != old),
            "该 IP/主机名和 SSH 端口已经存在"
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
            user: "dev".into(),
            ..Settings::default()
        }
    }
    #[test]
    fn identity_normalizes_addresses_and_excludes_user_and_label() {
        let a = profile("2001:0db8:0:0:0:0:0:1");
        let mut b = profile("2001:db8::1");
        b.user = "another".into();
        b.name = "别名".into();
        assert_eq!(a.id(), b.id());
        assert_eq!(a.id().len(), 64);
        assert_eq!(profile("Mac.LOCAL.").id(), profile("mac.local").id());
        let mut config = Connections::default();
        config.replace(None, profile("Mac.LOCAL.")).unwrap();
        assert_eq!(config.profiles[0].host, "Mac.LOCAL.");
        assert!(config.replace(None, profile("mac.local")).is_err());
        b.ssh_port = 2222;
        assert_ne!(a.id(), b.id());
    }
    #[test]
    fn add_edit_delete_reject_duplicates_and_persist_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut config = Connections::default();
        let a = config.replace(None, profile("192.0.2.1")).unwrap();
        config.replace(None, profile("192.0.2.2")).unwrap();
        let before = config.clone();
        assert!(config.replace(Some(&a), profile("192.0.2.2")).is_err());
        assert_eq!(config, before);
        config.replace(Some(&a), profile("192.0.2.3")).unwrap();
        config.save(&path).unwrap();
        assert_eq!(Connections::load(&path).unwrap(), (config.clone(), false));
        config.profiles.remove(0);
        config.save(&path).unwrap();
        assert_eq!(Connections::load(&path).unwrap().0.profiles.len(), 1);
    }
    #[test]
    fn legacy_single_profile_is_explicitly_marked_for_one_time_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        profile("192.0.2.1").save(&path).unwrap();
        let (config, migration) = Connections::load(&path).unwrap();
        assert!(migration);
        assert_eq!(config.profiles.len(), 1);
        config.save(&path).unwrap();
        assert!(!Connections::load(&path).unwrap().1);
        std::fs::write(&path, b"{}").unwrap();
        assert!(Connections::load(&path).is_err());
    }
}
