use anyhow::{Context, Result, ensure};
use landesk_core::domain::connection::ConnectionProfile;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
pub fn directory() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("无法找到当前用户配置目录")?
        .join("LanDeskClient"))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Connections {
    pub version: u32,
    pub profiles: Vec<ConnectionProfile>,
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
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).context("无法读取连接列表"),
        };
        let config: Self = serde_json::from_slice(&bytes).context("连接列表格式错误")?;
        config.validate()?;
        Ok(config)
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
    pub fn replace(&mut self, old: Option<&str>, mut profile: ConnectionProfile) -> Result<String> {
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
    fn profile(host: &str) -> ConnectionProfile {
        ConnectionProfile {
            host: host.into(),
            user: "dev".into(),
            ..ConnectionProfile::default()
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
        b.ssh_port = 22;
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
        assert_eq!(Connections::load(&path).unwrap(), config.clone());
        config.profiles.remove(0);
        config.save(&path).unwrap();
        assert_eq!(Connections::load(&path).unwrap().profiles.len(), 1);
    }
    #[test]
    fn unknown_or_single_profile_format_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for bytes in [
            serde_json::to_vec(&profile("192.0.2.1")).unwrap(),
            b"{}".to_vec(),
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(Connections::load(&path).is_err());
        }
    }
}
