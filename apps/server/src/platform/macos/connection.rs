use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::path::PathBuf;

pub fn path() -> Result<PathBuf> {
    Ok(landesk_storage::application_data("server")?.join("config/config.json"))
}

pub fn prepare() -> Result<landesk_core::domain::identity::ServerIdentity> {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::PermissionsExt;
    let path = path()?;
    if !path.exists() {
        let password = landesk_core::domain::password::generate()?;
        let directory = path.parent().context("连接配置目录无效")?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        crate::adapters::identity::write_new(
            &crate::adapters::identity::create_stored(
                &password,
                crate::adapters::ssh::DEFAULT_PORT,
                &path,
            )?,
            &path,
        )?;
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "连接配置必须为私有普通文件"
    );
    crate::adapters::identity::read(&path)
}
