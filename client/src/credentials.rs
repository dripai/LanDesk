use crate::settings::Settings;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;
use windows_sys::Win32::{Foundation::ERROR_NOT_FOUND, Security::Credentials::*};
use zeroize::{Zeroize, Zeroizing};

const TARGET: &str = "LanDeskClient/SSH";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPassword {
    host: String,
    user: String,
    port: u16,
    password: String,
}
impl Drop for SavedPassword {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}
impl SavedPassword {
    pub fn new(settings: &Settings, password: &str) -> Self {
        Self {
            host: settings.host.clone(),
            user: settings.user.clone(),
            port: settings.ssh_port,
            password: password.into(),
        }
    }
    pub fn matches(&self, settings: &Settings) -> bool {
        self.host == settings.host && self.user == settings.user && self.port == settings.ssh_port
    }
    pub fn password(&self) -> Zeroizing<String> {
        Zeroizing::new(self.password.clone())
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

struct Credential(*mut CREDENTIALW);
impl Drop for Credential {
    fn drop(&mut self) {
        unsafe {
            let record = &mut *self.0;
            if record.CredentialBlobSize != 0 && !record.CredentialBlob.is_null() {
                std::slice::from_raw_parts_mut(
                    record.CredentialBlob,
                    record.CredentialBlobSize as usize,
                )
                .zeroize();
            }
            CredFree(self.0.cast());
        }
    }
}

fn read_at(target: &str) -> Result<Option<SavedPassword>> {
    let target = wide(target);
    let mut pointer = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(None);
        }
        return Err(error).context("无法读取 Windows 凭据管理器");
    }
    ensure!(!pointer.is_null(), "Windows 返回了空凭据");
    let credential = Credential(pointer);
    let record = unsafe { &*credential.0 };
    ensure!(
        record.CredentialBlobSize != 0 && !record.CredentialBlob.is_null(),
        "保存的 SSH 凭据为空"
    );
    let bytes = unsafe {
        std::slice::from_raw_parts(record.CredentialBlob, record.CredentialBlobSize as usize)
    };
    Ok(Some(
        serde_json::from_slice(bytes).context("保存的 SSH 凭据损坏")?,
    ))
}

fn write_at(target: &str, saved: Option<&SavedPassword>) -> Result<()> {
    let mut target = wide(target);
    let Some(saved) = saved else {
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
                return Err(error).context("无法删除保存的 SSH 密码");
            }
        }
        return Ok(());
    };
    let mut bytes = Zeroizing::new(serde_json::to_vec(saved)?);
    ensure!(
        bytes.len() <= CRED_MAX_CREDENTIAL_BLOB_SIZE as usize,
        "SSH 凭据超过 Windows 保存上限"
    );
    let mut username = wide(&saved.user);
    let record = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: bytes.len() as u32,
        CredentialBlob: bytes.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username.as_mut_ptr(),
        ..Default::default()
    };
    ensure!(
        unsafe { CredWriteW(&record, 0) } != 0,
        "无法保存 Windows SSH 凭据：{}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

pub fn read() -> Result<Option<SavedPassword>> {
    read_at(TARGET)
}

pub fn save(settings: &Settings, path: &Path, password: Option<&SavedPassword>) -> Result<()> {
    save_at(TARGET, settings, path, password)
}

fn save_at(
    target: &str,
    settings: &Settings,
    path: &Path,
    password: Option<&SavedPassword>,
) -> Result<()> {
    settings.validate()?;
    let previous = read_at(target)?;
    write_at(target, password)?;
    if let Err(error) = settings.save(path) {
        if let Err(rollback) = write_at(target, previous.as_ref()) {
            bail!(
                "设置保存失败：{error:#}；凭据回滚也失败：{rollback:#}，请检查 Windows 凭据管理器"
            );
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_round_trip_account_binding_delete_and_settings_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let target = format!(
            "LanDeskClient/test/{}-{}",
            std::process::id(),
            rand::random::<u64>()
        );
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = write_at(&self.0, None);
            }
        }
        let _cleanup = Cleanup(target.clone());
        let mut settings = Settings {
            host: "example.test".into(),
            user: "developer".into(),
            ..Settings::default()
        };
        let saved = SavedPassword::new(&settings, "测试-password");
        let path = temp.path().join("settings.json");
        save_at(&target, &settings, &path, Some(&saved)).unwrap();
        let loaded = read_at(&target).unwrap().unwrap();
        assert!(loaded.matches(&settings));
        assert_eq!(&*loaded.password(), "测试-password");
        settings.host = "other.test".into();
        assert!(!loaded.matches(&settings));
        let changed = SavedPassword::new(&settings, "replacement");
        assert!(save_at(&target, &settings, temp.path(), Some(&changed)).is_err());
        assert_eq!(
            &*read_at(&target).unwrap().unwrap().password(),
            "测试-password"
        );
        save_at(&target, &settings, &path, None).unwrap();
        assert!(read_at(&target).unwrap().is_none());
        assert!(!std::fs::read_to_string(path).unwrap().contains("password"));
    }
}
