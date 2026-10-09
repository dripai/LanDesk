//! Application credentials, independent of operating-system accounts and sshd.
use anyhow::{Context, Result, ensure};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use russh::keys::{Algorithm, PrivateKey};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use zeroize::Zeroize;

pub const DEFAULT_PORT: u16 = 17891;
pub const USER: &str = "landesk";
// This is a protocol channel identifier, never a TCP forwarding destination.
pub const CHANNEL_PORT: u32 = 17890;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    version: u32,
    pub port: u16,
    pub password_hash: Option<String>,
    private_key: String,
}
impl Drop for Settings {
    fn drop(&mut self) {
        self.private_key.zeroize();
    }
}
pub fn path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("无法定位当前用户配置目录")?
        .join("LanDeskServer/server.json"))
}
pub fn parse_port(value: &str) -> Result<u16> {
    let port: u16 = value.trim().parse().context("连接端口必须为 1024–65535")?;
    ensure!(
        port >= 1024 && port != 17890,
        "连接端口须为 1024–65535，且不能占用客户端的 17890"
    );
    Ok(port)
}
impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let config: Self = serde_json::from_slice(&bytes)
                    .context("LanDeskServer 配置损坏，请检查 server.json")?;
                ensure!(config.version == 1, "不支持的服务端配置版本");
                parse_port(&config.port.to_string())?;
                config.key()?;
                if let Some(hash) = &config.password_hash {
                    let hash = PasswordHash::new(hash)
                        .map_err(|e| anyhow::anyhow!("访问密码校验值损坏：{e}"))?;
                    ensure!(
                        hash.algorithm.as_str() == "argon2id",
                        "访问密码校验算法无效"
                    );
                }
                Ok(config)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                version: 1,
                port: DEFAULT_PORT,
                password_hash: None,
                private_key: PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?
                    .to_openssh(russh::keys::ssh_key::LineEnding::LF)?
                    .to_string(),
            }),
            Err(error) => Err(error).context("无法读取 LanDeskServer 配置"),
        }
    }
    pub fn key(&self) -> Result<PrivateKey> {
        let key = PrivateKey::from_openssh(&self.private_key).context("服务端设备密钥损坏")?;
        ensure!(key.algorithm() == Algorithm::Ed25519, "设备密钥类型无效");
        Ok(key)
    }
    pub fn update(&self, port: u16, password: &str) -> Result<Self> {
        parse_port(&port.to_string())?;
        let mut next = self.clone();
        next.port = port;
        if !password.is_empty() {
            ensure!(
                password.chars().count() >= 10 && password.len() <= 256 && !password.contains('\0'),
                "访问密码至少 10 个字符，最多 256 字节"
            );
            let mut salt = [0u8; 16];
            getrandom::fill(&mut salt).map_err(|e| anyhow::anyhow!("生成密码盐失败：{e}"))?;
            let salt = SaltString::encode_b64(&salt)
                .map_err(|e| anyhow::anyhow!("密码盐编码失败：{e}"))?;
            next.password_hash = Some(
                Argon2::default()
                    .hash_password(password.as_bytes(), &salt)
                    .map_err(|e| anyhow::anyhow!("保存访问密码失败：{e}"))?
                    .to_string(),
            );
        }
        ensure!(
            next.password_hash.is_some(),
            "首次启动请设置 LanDesk 访问密码（至少 10 个字符）"
        );
        Ok(next)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("服务端配置路径无效")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec_pretty(self)?);
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(path).context("无法保存服务端连接设置")?;
        Ok(())
    }
}
pub fn verify(hash: &str, password: &str) -> bool {
    if password.len() > 256 {
        return false;
    }
    PasswordHash::new(hash).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_require_setup_hash_password_and_keep_identity_on_rotation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("server.json");
        let initial = Settings::load(&path).unwrap();
        assert!(initial.update(DEFAULT_PORT, "").is_err());
        assert!(initial.update(DEFAULT_PORT, "888888").is_err());
        let configured = initial.update(DEFAULT_PORT, "test-password-1").unwrap();
        configured.save(&path).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert!(verify(
            loaded.password_hash.as_ref().unwrap(),
            "test-password-1"
        ));
        assert!(!verify(loaded.password_hash.as_ref().unwrap(), "wrong"));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("test-password-1")
        );
        let changed = loaded.update(DEFAULT_PORT + 1, "test-password-2").unwrap();
        assert_eq!(
            loaded.key().unwrap().public_key(),
            changed.key().unwrap().public_key()
        );
        assert!(!verify(
            changed.password_hash.as_ref().unwrap(),
            "test-password-1"
        ));
        assert!(verify(
            changed.password_hash.as_ref().unwrap(),
            "test-password-2"
        ));
        assert_eq!(
            loaded.update(DEFAULT_PORT + 1, "").unwrap().password_hash,
            loaded.password_hash
        );
        std::fs::write(&path, b"{}").unwrap();
        assert!(Settings::load(&path).is_err());
    }
}
