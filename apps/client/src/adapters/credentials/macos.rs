use super::*;
use security_framework::passwords::delete_generic_password;
use security_framework::passwords::get_generic_password;
use security_framework::passwords::set_generic_password;

const SERVICE: &str = "local.aiwork.LanDeskClient.SSH";
const NOT_FOUND: i32 = -25300; // errSecItemNotFound; all other errors remain fatal.

pub(super) fn read_at(target: &str) -> Result<Option<SavedPassword>> {
    let bytes = match get_generic_password(SERVICE, target) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(error) if error.code() == NOT_FOUND => return Ok(None),
        Err(error) => return Err(error).context("无法读取 macOS 钥匙串"),
    };
    Ok(Some(
        serde_json::from_slice(&bytes).context("保存的 SSH 凭据损坏")?,
    ))
}

pub(super) fn write_at(target: &str, saved: Option<&SavedPassword>) -> Result<()> {
    match saved {
        Some(saved) => {
            let bytes = Zeroizing::new(serde_json::to_vec(saved)?);
            set_generic_password(SERVICE, target, &bytes)
                .context("无法保存 SSH 凭据到 macOS 钥匙串")?;
            ensure!(
                read_at(target)?.as_ref() == Some(saved),
                "SSH 凭据保存后状态核验失败"
            );
        }
        None => {
            match delete_generic_password(SERVICE, target) {
                Ok(()) => {}
                Err(error) if error.code() == NOT_FOUND => {}
                Err(error) => return Err(error).context("无法删除钥匙串中的 SSH 凭据"),
            }
            ensure!(read_at(target)?.is_none(), "SSH 凭据删除后状态核验失败");
        }
    }
    Ok(())
}
