use crate::protocol::{connection_code, valid_code};
use anyhow::{Context, Result, ensure};
use objc2_foundation::{
    NSSearchPathDirectory, NSSearchPathDomainMask, NSSearchPathForDirectoriesInDomains,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{ErrorKind, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Preference {
    Random {},
    Fixed { code: String },
}

impl Preference {
    fn code(&self) -> Result<String> {
        match self {
            Self::Random {} => connection_code(),
            Self::Fixed { code } => {
                ensure!(valid_code(code), "固定连接码必须是 6 位数字");
                Ok(code.clone())
            }
        }
    }
    pub fn description(&self) -> &'static str {
        match self {
            Self::Random {} => "随机连接码 · 每次启动更新",
            Self::Fixed { .. } => "固定连接码 · 重开保持不变",
        }
    }
}

pub struct CodeSettings {
    path: PathBuf,
    pub preference: Preference,
    pub code: String,
}

impl CodeSettings {
    pub fn app_path() -> Result<PathBuf> {
        let paths = NSSearchPathForDirectoriesInDomains(
            NSSearchPathDirectory::ApplicationSupportDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            true,
        );
        ensure!(paths.len() == 1, "无法定位应用设置目录");
        Ok(PathBuf::from(paths.objectAtIndex(0).to_string())
            .join("LanDesk")
            .join("settings.json"))
    }
    pub fn open(path: PathBuf) -> Result<Self> {
        let preference = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Preference>(&bytes)
                .context("连接码设置文件无效，请检查 settings.json")?,
            Err(error) if error.kind() == ErrorKind::NotFound => Preference::Random {},
            Err(error) => return Err(error).context("无法读取连接码设置"),
        };
        let code = preference.code()?;
        Ok(Self {
            path,
            preference,
            code,
        })
    }
    // The caller holds the same mutex used by authentication when acquiring a session.
    pub fn change(&mut self, preference: Preference, active: &AtomicBool) -> Result<()> {
        ensure!(
            !active.load(Ordering::Acquire),
            "请先断开远程连接，再修改连接码"
        );
        let code = preference.code()?;
        let parent = self.path.parent().context("设置文件路径无效")?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .context("无法创建连接码设置目录")?;
        let suffix = getrandom::u64().map_err(|e| anyhow::anyhow!("无法生成设置文件名称: {e}"))?;
        let temporary = parent.join(format!(".settings-{}-{suffix}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .context("无法创建连接码设置文件")?;
        let result = (|| -> Result<()> {
            file.write_all(&serde_json::to_vec_pretty(&preference)?)?;
            file.sync_all()?;
            // Rename is the final fallible operation: disk and memory commit together.
            fs::rename(&temporary, &self.path)?;
            Ok(())
        })();
        if let Err(error) = result {
            fs::remove_file(&temporary).context("保存失败，且无法清理临时设置文件")?;
            return Err(error).context("保存连接码失败，原设置未改变");
        }
        self.preference = preference;
        self.code = code;
        Ok(())
    }
    #[cfg(test)]
    pub fn fixture(code: &str) -> Self {
        Self {
            path: PathBuf::new(),
            preference: Preference::Fixed { code: code.into() },
            code: code.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("landesk-settings-{}", getrandom::u64().unwrap()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn fixed_code_and_leading_zero_survive_restart() {
        let dir = Directory::new();
        let mut settings = CodeSettings::open(dir.path()).unwrap();
        settings
            .change(
                Preference::Fixed {
                    code: "001234".into(),
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        let reopened = CodeSettings::open(dir.path()).unwrap();
        assert_eq!(reopened.code, "001234");
        assert_eq!(reopened.preference, settings.preference);
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn random_mode_survives_restart_without_persisting_code() {
        let dir = Directory::new();
        let mut settings = CodeSettings::open(dir.path()).unwrap();
        let inactive = AtomicBool::new(false);
        settings
            .change(
                Preference::Fixed {
                    code: "001234".into(),
                },
                &inactive,
            )
            .unwrap();
        settings.change(Preference::Random {}, &inactive).unwrap();
        assert!(valid_code(&settings.code));
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path()).unwrap()).unwrap();
        assert_eq!(saved, serde_json::json!({"mode":"random"}));
        let reopened = CodeSettings::open(dir.path()).unwrap();
        assert_eq!(reopened.preference, Preference::Random {});
        assert!(valid_code(&reopened.code));
    }
    #[test]
    fn invalid_code_and_active_session_do_not_change_disk_or_memory() {
        let dir = Directory::new();
        let mut settings = CodeSettings::open(dir.path()).unwrap();
        settings
            .change(
                Preference::Fixed {
                    code: "123456".into(),
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        let saved = fs::read(dir.path()).unwrap();
        for code in ["1234", "１２３４５６", "12 456", "abcdef"] {
            assert!(
                settings
                    .change(
                        Preference::Fixed { code: code.into() },
                        &AtomicBool::new(false)
                    )
                    .is_err()
            );
        }
        assert!(
            settings
                .change(Preference::Random {}, &AtomicBool::new(true))
                .is_err()
        );
        assert_eq!(fs::read(dir.path()).unwrap(), saved);
        assert_eq!(settings.code, "123456");
    }
    #[test]
    fn save_failure_preserves_memory_and_removes_temporary_file() {
        let dir = Directory::new();
        let mut settings = CodeSettings::open(dir.path()).unwrap();
        let previous_code = settings.code.clone();
        fs::create_dir(dir.path()).unwrap();
        assert!(
            settings
                .change(
                    Preference::Fixed {
                        code: "123456".into()
                    },
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert_eq!(settings.code, previous_code);
        assert_eq!(settings.preference, Preference::Random {});
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }
    #[test]
    fn corrupted_settings_are_reported_instead_of_reset() {
        let dir = Directory::new();
        for bytes in [
            b"{".as_slice(),
            b"{\"mode\":\"fixed\",\"code\":\"1234\"}",
            b"{\"mode\":\"random\",\"unexpected\":true}",
            &[0xff],
        ] {
            fs::write(dir.path(), bytes).unwrap();
            assert!(CodeSettings::open(dir.path()).is_err());
        }
    }
}
