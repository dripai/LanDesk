use crate::adapters::settings::Connections;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use landesk_core::domain::connection::Authentication;
use landesk_core::domain::connection::ConnectionProfile;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use zeroize::Zeroize;
use zeroize::Zeroizing;

mod files;
use files::{read_at, write_at};

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
    pub fn new(settings: &ConnectionProfile, password: &str) -> Self {
        Self {
            host: settings.normalized_host(),
            user: settings.ssh_user().into(),
            port: settings.ssh_port,
            password: password.into(),
        }
    }
    pub fn matches(&self, settings: &ConnectionProfile) -> bool {
        ConnectionProfile {
            host: self.host.clone(),
            ..ConnectionProfile::default()
        }
        .normalized_host()
            == settings.normalized_host()
            && self.user == settings.ssh_user()
            && self.port == settings.ssh_port
            && settings.authentication == Authentication::Password
    }
    pub fn password(&self) -> Zeroizing<String> {
        Zeroizing::new(self.password.clone())
    }
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
                "保存失败：{error:#}；凭据回滚失败：{}，请检查应用凭据文件",
                failures.join("；")
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
        let mut settings = ConnectionProfile {
            host: "example.test".into(),
            user: "developer".into(),
            ..ConnectionProfile::default()
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
        settings.authentication = Authentication::PrivateKey {
            path: temp.path().join("id_ed25519"),
        };
        assert!(!loaded.matches(&settings));
        settings.authentication = Authentication::Password;
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
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("测试-password")
        );
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
        let first = ConnectionProfile {
            host: "192.0.2.1".into(),
            user: "dev".into(),
            ..ConnectionProfile::default()
        };
        let second = ConnectionProfile {
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
