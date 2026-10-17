use crate::{domain::password, security::secret::Secret};
use anyhow::{Result, ensure};
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use serde::{Deserialize, Serialize};
pub const PASSWORD_PURPOSE: &str = "LanDesk.Server.ConnectionPassword.v1";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerIdentity {
    pub port: u16,
    password_hash: String,
    display_password: Secret,
    host_key: String,
    pub backend_key: String,
}
impl ServerIdentity {
    pub fn new(
        password: &str,
        port: u16,
        display_password: Secret,
        host_key: String,
        backend_key: String,
    ) -> Result<Self> {
        password::validate(password)?;
        let value = Self {
            port,
            password_hash: Argon2::default()
                .hash_password(password.as_bytes())?
                .to_string(),
            display_password,
            host_key,
            backend_key,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.port != 0, "SSH 端口无效");
        ensure!(
            PasswordHash::new(&self.password_hash)?.algorithm.as_str() == "argon2id",
            "密码散列算法无效"
        );
        ensure!(!self.host_key.is_empty(), "主机身份缺失");
        ensure!(
            self.backend_key.len() == 64 && self.backend_key.bytes().all(|b| b.is_ascii_hexdigit()),
            "内部连接密钥无效"
        );
        Ok(())
    }
    pub fn host_key(&self) -> &str {
        &self.host_key
    }
    pub fn password_secret(&self) -> Secret {
        self.display_password.clone()
    }
    pub fn same_host(&self, other: &Self) -> bool {
        self.port == other.port
            && self.host_key == other.host_key
            && self.backend_key == other.backend_key
    }
    pub fn verify_password(&self, password: &str) -> Result<bool> {
        Ok(Argon2::default()
            .verify_password(
                password.as_bytes(),
                &PasswordHash::new(&self.password_hash)?,
            )
            .is_ok())
    }
    pub fn replace_credential(&mut self, password: &str, secret: Secret) -> Result<()> {
        password::validate(password)?;
        self.password_hash = Argon2::default()
            .hash_password(password.as_bytes())?
            .to_string();
        self.display_password = secret;
        Ok(())
    }
}

#[derive(Debug)]
pub enum CommitStatus {
    Committed,
    CommittedWithWarning(String),
}
#[derive(Debug)]
pub enum CommitFailure {
    BeforeCommit(anyhow::Error),
    Unknown(anyhow::Error),
}
impl std::fmt::Display for CommitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforeCommit(e) => write!(f, "{e:#}"),
            Self::Unknown(e) => write!(f, "保存结果无法确认，请重新读取配置：{e:#}"),
        }
    }
}
impl std::error::Error for CommitFailure {}
impl From<anyhow::Error> for CommitFailure {
    fn from(e: anyhow::Error) -> Self {
        Self::BeforeCommit(e)
    }
}
impl From<std::io::Error> for CommitFailure {
    fn from(e: std::io::Error) -> Self {
        Self::BeforeCommit(e.into())
    }
}
impl From<serde_json::Error> for CommitFailure {
    fn from(e: serde_json::Error) -> Self {
        Self::BeforeCommit(e.into())
    }
}
pub type CommitResult = std::result::Result<CommitStatus, CommitFailure>;
