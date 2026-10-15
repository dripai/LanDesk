use crate::platform::secrets::NativeSecretProtector;
use anyhow::{Context, Result, ensure};
#[cfg(windows)]
use landesk_core::domain::identity::CommitFailure;
use landesk_core::{
    domain::identity::{CommitResult, CommitStatus},
    domain::identity::{PASSWORD_PURPOSE, ServerIdentity},
    ports::IdentityRepository,
    security::secret::Secret,
};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;
pub struct IdentityFile(pub PathBuf);
impl IdentityRepository for IdentityFile {
    fn lock(&self) -> Result<Box<dyn std::any::Any>> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(self.0.with_extension("lock"))?;
        lock.try_lock().context("无法取得密码保存锁")?;
        Ok(Box::new(lock))
    }

    fn read(&self) -> Result<ServerIdentity> {
        read(&self.0)
    }
    fn commit(&self, config: &ServerIdentity) -> CommitResult {
        use std::io::Write;
        let path = &self.0;
        // Hash and ciphertext are committed together in this one atomic file.
        let parent = path.parent().context("连接配置路径无效")?;
        let mut stage = tempfile::NamedTempFile::new_in(parent)?;
        stage.write_all(&Zeroizing::new(serde_json::to_vec(config)?))?;
        stage.as_file().sync_all()?;
        #[cfg(not(windows))]
        stage
            .persist(path)
            .map_err(|error| error.error)
            .context("保存连接密码失败")?;
        #[cfg(windows)]
        {
            let original = Zeroizing::new(std::fs::read(path)?);
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING;
            use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
            use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
            use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
            let wide = |path: &Path| {
                path.as_os_str()
                    .encode_wide()
                    .chain(Some(0))
                    .collect::<Vec<_>>()
            };
            let backup = parent.join(format!(
                "password-backup-{}.json",
                stage.path().file_stem().unwrap().to_string_lossy()
            ));
            let (_, stage) = stage.keep().map_err(|error| error.error)?;
            let result = unsafe {
                ReplaceFileW(
                    wide(path).as_ptr(),
                    wide(&stage).as_ptr(),
                    wide(&backup).as_ptr(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if result == 0 {
                let error = std::io::Error::last_os_error();
                if backup.exists()
                    && unsafe {
                        MoveFileExW(
                            wide(&backup).as_ptr(),
                            wide(path).as_ptr(),
                            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                        )
                    } == 0
                {
                    return Err(CommitFailure::Unknown(anyhow::anyhow!(
                        "保存连接密码失败: {error}; 原配置恢复失败: {}，备份保留在 {}",
                        std::io::Error::last_os_error(),
                        backup.display()
                    )));
                }
                if std::fs::read(path).ok().as_deref() != Some(original.as_slice()) {
                    return Err(CommitFailure::Unknown(anyhow::anyhow!(
                        "保存连接密码失败: {error}；原配置无法核验"
                    )));
                }
                if stage.exists() {
                    std::fs::remove_file(&stage)?;
                }
                return Err(CommitFailure::BeforeCommit(
                    anyhow::Error::new(error).context("保存连接密码失败，原配置已保留"),
                ));
            }
            if let Err(error) = std::fs::remove_file(backup) {
                return Ok(CommitStatus::CommittedWithWarning(format!(
                    "连接密码已保存，但旧配置备份清理失败: {error}"
                )));
            }
        }
        #[cfg(unix)]
        if let Err(error) = std::fs::File::open(parent).and_then(|directory| directory.sync_all()) {
            return Ok(CommitStatus::CommittedWithWarning(format!(
                "密码已替换，但目录持久化失败: {error}"
            )));
        }
        Ok(CommitStatus::Committed)
    }
}
pub fn create(password: &str, port: u16, display_password: Secret) -> Result<ServerIdentity> {
    landesk_core::domain::password::validate(password)?;
    ensure!(port != 0, "SSH 端口无效");
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut()).map_err(|e| anyhow::anyhow!("随机数生成失败: {e}"))?;
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed).into(),
        "LanDesk",
    )?;
    getrandom::fill(seed.as_mut()).map_err(|e| anyhow::anyhow!("随机数生成失败: {e}"))?;
    ServerIdentity::new(
        password,
        port,
        display_password,
        key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?
            .to_string(),
        seed.iter().map(|b| format!("{b:02x}")).collect(),
    )
}
pub fn read(path: &Path) -> Result<ServerIdentity> {
    let text =
        Zeroizing::new(std::fs::read_to_string(path).context("缺少内置 SSH 配置，请运行安装程序")?);
    let config: ServerIdentity = serde_json::from_str(&text).context("内置 SSH 配置无效")?;
    config.validate()?;
    private_key(&config)?;
    Ok(config)
}
pub fn write_new(config: &ServerIdentity, path: &Path) -> Result<()> {
    use std::io::Write;
    let data = Zeroizing::new(serde_json::to_vec(config)?);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if let Err(error) = file.write_all(&data).and_then(|_| file.sync_all()) {
        drop(file);
        std::fs::remove_file(path).context("配置写入失败，且无法清理不完整配置")?;
        return Err(error.into());
    }
    Ok(())
}

pub fn create_stored(password: &str, port: u16) -> Result<ServerIdentity> {
    create(
        password,
        port,
        Secret::encrypt(
            password.as_bytes(),
            PASSWORD_PURPOSE,
            &NativeSecretProtector,
        )?,
    )
}
pub fn private_key(identity: &ServerIdentity) -> Result<russh::keys::PrivateKey> {
    Ok(russh::keys::PrivateKey::from_openssh(identity.host_key())?)
}
pub fn fingerprint(identity: &ServerIdentity) -> Result<String> {
    Ok(private_key(identity)?
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string())
}
#[cfg(test)]
pub fn change_password(path: &Path, password: &str, secret: Secret) -> Result<()> {
    let repo = IdentityFile(path.into());
    let mut identity = repo.read()?;
    identity.replace_credential(password, secret)?;
    repo.commit(&identity).map_err(anyhow::Error::new)?;
    Ok(())
}

pub fn change_stored_password(
    path: &Path,
    password: &str,
) -> landesk_core::application::password::PasswordOutcome {
    landesk_core::application::password::change(
        &IdentityFile(path.into()),
        &NativeSecretProtector,
        Zeroizing::new(password.to_owned()),
    )
}

#[cfg(test)]
mod repository_tests {
    use super::*;
    #[test]
    fn update_lock_survives_atomic_identity_replacement_and_releases_with_owner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.json");
        let identity = create("old", 2222, Secret::for_test(b"old", PASSWORD_PURPOSE)).unwrap();
        write_new(&identity, &path).unwrap();
        let first = IdentityFile(path.clone());
        let second = IdentityFile(path);
        let lease = first.lock().unwrap();
        assert!(second.lock().is_err());
        first.commit(&identity).unwrap();
        assert!(
            second.lock().is_err(),
            "locking the replaced identity inode would lose mutual exclusion"
        );
        drop(lease);
        assert!(second.lock().is_ok());
    }
}
