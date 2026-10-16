//! Shared local storage. No keychain, credential manager, DPAPI or user data directory.
use anyhow::{Context, Result, ensure};
use landesk_core::{
    ports::SecretProtector,
    security::encryption::{self, Key},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::Write,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

/// Anchor to the executable, never the launcher-dependent working directory.
/// A macOS bundle keeps mutable data outside its signed Contents directory.
pub fn data_directory(executable: &Path, role: &str) -> Result<PathBuf> {
    ensure!(matches!(role, "client" | "server"), "应用数据类型无效");
    let mut directory = executable.parent().context("无法定位程序目录")?;
    if directory.file_name().is_some_and(|name| name == "MacOS") {
        let contents = directory.parent().context("应用包结构无效")?;
        let bundle = contents.parent().context("应用包结构无效")?;
        if contents.file_name().is_some_and(|name| name == "Contents")
            && bundle
                .extension()
                .is_some_and(|extension| extension == "app")
        {
            directory = bundle.parent().context("无法定位应用包目录")?;
        }
    }
    Ok(directory.join("data").join(role))
}
pub fn application_data(role: &str) -> Result<PathBuf> {
    data_directory(&std::env::current_exe().context("无法定位当前程序")?, role)
}
pub fn validate_private(metadata: &Metadata) -> Result<()> {
    ensure!(!metadata.file_type().is_symlink(), "本地存储不能是符号链接");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "本地存储必须仅当前用户可访问"
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT: junctions must not redirect key storage.
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "本地存储不能是重解析点"
        );
    }
    Ok(())
}
pub fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).with_context(|| {
        format!(
            "无法创建数据目录 {}，请检查程序目录写入权限",
            path.display()
        )
    })?;
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(metadata.is_dir(), "数据目录不是普通目录");
    validate_private(&metadata)
}
pub fn prepare_private_file(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    validate_private(&file.metadata()?)
}
pub fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    // Windows files are flushed before the atomic rename and inherit the parent ACL.
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
pub struct LocalSecrets(PathBuf);
impl LocalSecrets {
    pub fn new(directory: PathBuf) -> Self {
        Self(directory)
    }
    pub fn beside(config: &Path) -> Self {
        Self(config.with_file_name("keys"))
    }
    fn path(&self, purpose: &str) -> PathBuf {
        self.0
            .join(format!("{:x}", Sha256::digest(purpose.as_bytes())))
    }
    fn read(&self, purpose: &str) -> Result<Key> {
        let path = self.path(purpose);
        let metadata = std::fs::symlink_metadata(&path).context("本地加密密钥不存在或无法访问")?;
        ensure!(
            metadata.is_file() && metadata.len() == 32,
            "本地加密密钥必须为 32 字节普通文件"
        );
        validate_private(&metadata)?;
        let bytes = Zeroizing::new(std::fs::read(path)?);
        ensure!(bytes.len() == 32, "本地加密密钥长度无效");
        let mut key = Zeroizing::new([0; 32]);
        key.copy_from_slice(&bytes);
        Ok(key)
    }
}
impl SecretProtector for LocalSecrets {
    fn key_for_write(&self, purpose: &str) -> Result<Key> {
        let path = self.path(purpose);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return self.read(purpose),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        create_private_directory(&self.0)?;
        let key = encryption::generate_key()?;
        let mut stage = tempfile::NamedTempFile::new_in(&self.0)?;
        prepare_private_file(stage.as_file())?;
        stage.write_all(key.as_ref())?;
        stage.as_file().sync_all()?;
        match stage.persist_noclobber(path) {
            Ok(_) => sync_directory(&self.0)?,
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.error.into()),
        }
        self.read(purpose)
    }
    fn key_for_read(&self, purpose: &str) -> Result<Key> {
        self.read(purpose)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use landesk_core::security::secret::Secret;
    #[test]
    fn data_is_beside_executable_and_outside_app_bundle() {
        assert_eq!(
            data_directory(Path::new("/portable/LanDeskServer.exe"), "server").unwrap(),
            Path::new("/portable/data/server")
        );
        assert_eq!(
            data_directory(
                Path::new("/portable/LanDesk.app/Contents/MacOS/LanDesk"),
                "server"
            )
            .unwrap(),
            Path::new("/portable/data/server")
        );
        assert_eq!(
            data_directory(
                Path::new("/portable/LanDeskClient.app/Contents/MacOS/LanDeskClient"),
                "client"
            )
            .unwrap(),
            Path::new("/portable/data/client")
        );
    }
    #[test]
    fn persisted_secrets_work_after_directory_move_and_reject_missing_or_damaged_keys() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original");
        let keys = LocalSecrets::new(original.clone());
        assert!(keys.key_for_read("test").is_err());
        assert!(!original.exists());
        let value = Secret::encrypt(b"test password", "test", &keys).unwrap();
        let encoded = serde_json::to_vec(&value).unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("test password"));
        let moved = directory.path().join("moved");
        std::fs::rename(original, &moved).unwrap();
        let keys = LocalSecrets::new(moved);
        let value: Secret = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(&*value.decrypt("test", &keys).unwrap(), b"test password");
        assert!(value.decrypt("wrong-purpose", &keys).is_err());
        let path = keys.path("test");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(value.decrypt("test", &keys).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(&path, b"damaged").unwrap();
        assert!(keys.key_for_write("test").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"damaged");
        std::fs::remove_file(path).unwrap();
        assert!(value.decrypt("test", &keys).is_err());
    }
}
