use crate::settings::{Connections, Settings};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;
use windows_sys::Win32::{Foundation::ERROR_NOT_FOUND, Security::Credentials::*};
use zeroize::{Zeroize, Zeroizing};

const TARGET: &str = "LanDeskClient/SSH";
static ACCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Serialize, Deserialize, PartialEq, Eq)]
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
            host: settings.normalized_host(),
            user: settings.user.clone(),
            port: settings.ssh_port,
            password: password.into(),
        }
    }
    pub fn matches(&self, settings: &Settings) -> bool {
        Settings {
            host: self.host.clone(),
            ..Settings::default()
        }
        .normalized_host()
            == settings.normalized_host()
            && self.user == settings.user
            && self.port == settings.ssh_port
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
    let mut target_wide = wide(target);
    let Some(saved) = saved else {
        if unsafe { CredDeleteW(target_wide.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
                return Err(error).context("无法删除保存的 SSH 密码");
            }
        }
        ensure!(
            read_at(target)?.is_none(),
            "Windows SSH 凭据删除后状态核验失败"
        );
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
        TargetName: target_wide.as_mut_ptr(),
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
    ensure!(
        read_at(target)?.as_ref() == Some(saved),
        "Windows SSH 凭据保存后状态核验失败"
    );
    Ok(())
}

fn target(id: &str) -> Result<String> {
    ensure!(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "连接标识无效"
    );
    Ok(format!("LanDeskClient/SSH/{id}"))
}
pub fn read(id: &str) -> Result<Option<SavedPassword>> {
    let _lock = ACCESS
        .lock()
        .map_err(|_| anyhow::anyhow!("凭据操作状态不可用"))?;
    read_at(&target(id)?)
}

pub fn commit(
    config: &Connections,
    path: &Path,
    changes: Vec<(String, Option<SavedPassword>)>,
) -> Result<()> {
    let _lock = ACCESS
        .lock()
        .map_err(|_| anyhow::anyhow!("凭据操作状态不可用"))?;
    let changes = changes
        .into_iter()
        .map(|(id, saved)| Ok((target(&id)?, saved)))
        .collect::<Result<Vec<_>>>()?;
    transaction(config, path, changes)
}
fn transaction(
    config: &Connections,
    path: &Path,
    changes: Vec<(String, Option<SavedPassword>)>,
) -> Result<()> {
    config.validate()?;
    let mut seen = std::collections::HashSet::new();
    let mut previous = Vec::new();
    for (target, _) in &changes {
        ensure!(seen.insert(target), "重复的凭据操作");
        previous.push((target.clone(), read_at(target)?));
    }
    let result = (|| {
        for (target, password) in &changes {
            write_at(target, password.as_ref())?;
        }
        config.save(path)
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        for (target, password) in previous.iter().rev() {
            if let Err(error) = write_at(target, password.as_ref()) {
                failures.push(format!("{error:#}"));
            }
        }
        if !failures.is_empty() {
            bail!(
                "保存失败：{error:#}；凭据回滚失败：{}，请检查 Windows 凭据管理器",
                failures.join("；")
            );
        }
        return Err(error);
    }
    Ok(())
}
pub fn migrate(config: &Connections, path: &Path) -> Result<()> {
    let _lock = ACCESS
        .lock()
        .map_err(|_| anyhow::anyhow!("凭据操作状态不可用"))?;
    ensure!(config.profiles.len() == 1, "旧配置迁移缺少唯一连接");
    let profile = &config.profiles[0];
    let old = read_at(TARGET)?;
    let destination = target(&profile.id())?;
    ensure!(
        read_at(&destination)?.is_none(),
        "迁移目标凭据已存在，请先检查凭据管理器"
    );
    // Preserve an existing password only when it belongs to the imported account.
    if let Some(saved) = &old {
        ensure!(
            saved.matches(profile),
            "旧密码与当前连接不匹配，请先检查凭据管理器"
        );
    }
    transaction(
        config,
        path,
        vec![(destination, old), (TARGET.into(), None)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_round_trip_account_binding_delete_and_settings_rollback() {
        let _lock = ACCESS.lock().unwrap();
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
        let config = Connections {
            profiles: vec![settings.clone()],
            ..Connections::default()
        };
        transaction(&config, &path, vec![(target.clone(), Some(saved))]).unwrap();
        let loaded = read_at(&target).unwrap().unwrap();
        assert!(loaded.matches(&settings));
        assert_eq!(&*loaded.password(), "测试-password");
        settings.host = "other.test".into();
        assert!(!loaded.matches(&settings));
        let changed = SavedPassword::new(&settings, "replacement");
        assert!(transaction(&config, temp.path(), vec![(target.clone(), Some(changed))]).is_err());
        assert_eq!(
            &*read_at(&target).unwrap().unwrap().password(),
            "测试-password"
        );
        transaction(&config, &path, vec![(target.clone(), None)]).unwrap();
        assert!(read_at(&target).unwrap().is_none());
        assert!(!std::fs::read_to_string(path).unwrap().contains("password"));
    }
    #[test]
    fn independent_credentials_and_multi_record_rollback() {
        let _lock = ACCESS.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let base = format!(
            "LanDeskClient/test/{}-{}",
            std::process::id(),
            rand::random::<u64>()
        );
        let a = format!("{base}/a");
        let b = format!("{base}/b");
        let c = format!("{base}/c");
        struct Cleanup(Vec<String>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for target in &self.0 {
                    let _ = write_at(target, None);
                }
            }
        }
        let _cleanup = Cleanup(vec![a.clone(), b.clone(), c.clone()]);
        let first = Settings {
            host: "192.0.2.1".into(),
            user: "dev".into(),
            ..Settings::default()
        };
        let second = Settings {
            host: "192.0.2.2".into(),
            ..first.clone()
        };
        let config = Connections {
            profiles: vec![first.clone(), second.clone()],
            ..Connections::default()
        };
        let path = temp.path().join("settings.json");
        transaction(
            &config,
            &path,
            vec![
                (a.clone(), Some(SavedPassword::new(&first, "one"))),
                (b.clone(), Some(SavedPassword::new(&second, "two"))),
            ],
        )
        .unwrap();
        let error = transaction(
            &config,
            temp.path(),
            vec![
                (a.clone(), None),
                (c.clone(), Some(SavedPassword::new(&first, "changed"))),
            ],
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("无法原子保存连接列表"),
            "expected config failure with successful rollback, got: {error:#}"
        );
        assert_eq!(&*read_at(&a).unwrap().unwrap().password(), "one");
        assert_eq!(&*read_at(&b).unwrap().unwrap().password(), "two");
        assert!(
            read_at(&c).unwrap().is_none(),
            "new credential survived rollback: {error:#}"
        );
        transaction(&config, &path, vec![(a.clone(), None)]).unwrap();
        assert!(read_at(&a).unwrap().is_none());
        assert_eq!(&*read_at(&b).unwrap().unwrap().password(), "two");
    }
}
