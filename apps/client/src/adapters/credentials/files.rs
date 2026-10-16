use super::*;
use landesk_core::security::secret::Secret;
use landesk_storage as protection;
use std::io::Write;
use std::path::PathBuf;

fn path(target: &str) -> Result<PathBuf> {
    let name: String = target.bytes().map(|byte| format!("{byte:02x}")).collect();
    Ok(crate::adapters::settings::directory()?
        .join("credentials")
        .join(name))
}
fn read_file(path: &Path, target: &str) -> Result<Option<SavedPassword>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("无法读取本地凭据文件"),
    };
    ensure!(
        metadata.is_file() && metadata.len() <= 65536,
        "凭据文件无效或过大"
    );
    protection::validate_private(&metadata)?;
    let bytes = Zeroizing::new(std::fs::read(path)?);
    let secret: Secret = serde_json::from_slice(&bytes).context("本地凭据密文无效")?;
    let bytes = secret.decrypt(target, &protection::LocalSecrets::beside(path))?;
    Ok(Some(
        serde_json::from_slice(&bytes).context("保存的 SSH 凭据损坏")?,
    ))
}
pub(super) fn read_at(target: &str) -> Result<Option<SavedPassword>> {
    read_file(&path(target)?, target)
}
pub(super) fn write_at(target: &str, saved: Option<&SavedPassword>) -> Result<()> {
    write_file(&path(target)?, target, saved)
}
fn write_file(path: &Path, target: &str, saved: Option<&SavedPassword>) -> Result<()> {
    let directory = path.parent().context("凭据目录无效")?;
    match saved {
        Some(saved) => {
            protection::create_private_directory(directory)?;
            let plaintext = Zeroizing::new(serde_json::to_vec(saved)?);
            let secret =
                Secret::encrypt(&plaintext, target, &protection::LocalSecrets::beside(path))?;
            let bytes = serde_json::to_vec(&secret)?;
            let mut file = tempfile::NamedTempFile::new_in(directory)?;
            protection::prepare_private_file(file.as_file())?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist(path).context("无法保存本地凭据")?;
            protection::sync_directory(directory)?;
            ensure!(
                read_file(path, target)?.as_ref() == Some(saved),
                "凭据保存后状态核验失败"
            );
        }
        None => match std::fs::remove_file(path) {
            Ok(()) => protection::sync_directory(directory)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("无法删除本地凭据"),
        },
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn rejects_shared_permissions_symlinks_and_corrupt_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credential");
        assert!(read_file(&file, "test").unwrap().is_none());
        std::fs::write(&file, b"{}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            read_file(&file, "test")
                .err()
                .unwrap()
                .to_string()
                .contains("仅当前用户")
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_file(&file, "test").is_err());
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read_file(&link, "test").is_err());
    }
}
