//! One serialized, elevated maintenance operation; progress is read by the GPUI host.
use super::{privilege, setup, setup_services as scm, setup_support as os};
use anyhow::{Context, Result, ensure};
use landesk_core::{
    application::password::PasswordOutcome, ports::SecretProtector, security::secret::Secret,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    ptr::null_mut,
};
use windows_sys::Win32::System::Services::*;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub automatic: bool,
    pub minimize: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            automatic: true,
            minimize: true,
        }
    }
}
impl Preferences {
    pub fn load() -> Result<Self> {
        serde_json::from_slice(&fs::read(
            setup::Paths::new()?.data.join("preferences.json"),
        )?)
        .context("无法读取托盘设置")
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        write_json(path, self)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Request {
    Start,
    Save { port: Option<u16>, password: Secret },
    Startup(Preferences),
    Restart,
    Uninstall { owner: u32, remove_display: bool },
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub text: String,
    pub close: bool,
    pub error: bool,
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut stage = tempfile::NamedTempFile::new_in(path.parent().context("写入目录缺失")?)?;
    stage.write_all(&serde_json::to_vec(value)?)?;
    stage.as_file().sync_all()?;
    stage.persist(path).context("无法原子保存管理状态")?;
    Ok(())
}
pub struct Job {
    directory: tempfile::TempDir,
    executable: PathBuf,
}
impl Job {
    pub fn new() -> Result<Self> {
        let paths = setup::Paths::new()?;
        let directory = tempfile::Builder::new()
            .prefix("request-")
            .tempdir_in(paths.data.join("runtime/ui"))?;
        os::restrict_request_directory(directory.path())?;
        landesk_storage::LocalSecrets::beside(&directory.path().join("request.json"))
            .key_for_write("LanDesk.SettingsResponse")?;
        Ok(Self {
            directory,
            executable: paths.exe,
        })
    }
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    pub fn password(&self, password: &str) -> Result<Secret> {
        landesk_core::domain::password::validate(password)?;
        Secret::encrypt(
            password.as_bytes(),
            "LanDesk.SettingsRequest",
            &landesk_storage::LocalSecrets::beside(&self.path().join("request.json")),
        )
    }
    pub fn submit(&self, request: &Request) -> Result<()> {
        write_json(&self.path().join("request.json"), request)
    }
    pub fn invocation(&self) -> (PathBuf, String) {
        (
            self.executable.clone(),
            format!("--manage \"{}\"", self.path().display()),
        )
    }
    pub fn progress(&self) -> Result<Option<Progress>> {
        let bytes = match fs::read(self.path().join("progress.json")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(serde_json::from_slice(&bytes)?))
    }
    pub fn helper_running(&self) -> Result<bool> {
        use windows_sys::Win32::{Foundation::*, System::Threading::*};
        let pid: u32 = serde_json::from_slice(&fs::read(self.path().join("helper.json"))?)?;
        let raw = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, pid) };
        if raw.is_null() {
            let code = unsafe { GetLastError() };
            ensure!(
                code == ERROR_INVALID_PARAMETER,
                "无法读取卸载辅助进程状态（{code}）"
            );
            return Ok(false);
        }
        let handle = super::desktop::Handle::new(raw, "打开卸载辅助进程")?;
        match unsafe { WaitForSingleObject(handle.0, 0) } {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            _ => Err(std::io::Error::last_os_error()).context("无法检查卸载辅助进程"),
        }
    }
    pub fn settings(&self) -> Result<(u16, Zeroizing<String>)> {
        let saved: Settings =
            serde_json::from_slice(&fs::read(self.path().join("settings.json"))?)?;
        let bytes = saved.password.decrypt(
            "LanDesk.SettingsResponse",
            &landesk_storage::LocalSecrets::beside(&self.path().join("request.json")),
        )?;
        Ok((
            saved.port,
            Zeroizing::new(String::from_utf8(bytes.to_vec())?),
        ))
    }
    pub fn result(&self) -> Result<String> {
        let result: std::result::Result<String, String> =
            serde_json::from_slice(&fs::read(self.path().join("result.json"))?)?;
        result.map_err(anyhow::Error::msg)
    }
    /// The uninstall helper owns cleanup after the management process exits.
    pub fn close(self) -> Result<()> {
        self.directory.close().context("管理请求与临时密钥清理失败")
    }
    pub fn retain(self) {
        let _ = self.directory.keep();
    }
}
pub fn progress(directory: &Path, text: &str, close: bool, error: bool) -> Result<()> {
    crate::diagnostics::log(text);
    write_json(
        &directory.join("progress.json"),
        &Progress {
            text: text.into(),
            close,
            error,
        },
    )
}
#[derive(Serialize, Deserialize)]
struct Settings {
    port: u16,
    password: Secret,
}
fn settings(directory: &Path, paths: &setup::Paths) -> Result<()> {
    let path = paths.transport.join("config.json");
    let config = crate::adapters::identity::read(&path)?;
    let password = landesk_core::application::password::recover_password(
        &config,
        &landesk_storage::LocalSecrets::beside(&path),
    )?;
    let secret = Secret::encrypt(
        password.as_bytes(),
        "LanDesk.SettingsResponse",
        &landesk_storage::LocalSecrets::beside(&directory.join("request.json")),
    )?;
    write_json(
        &directory.join("settings.json"),
        &Settings {
            port: config.port,
            password: secret,
        },
    )
}
pub fn validate_directory(directory: &Path) -> Result<()> {
    os::plain_chain(directory)?;
    let root = setup::Paths::new()?.data.join("runtime/ui");
    ensure!(
        directory.parent() == Some(root.as_path())
            && directory
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("request-")),
        "管理请求不在程序运行目录"
    );
    Ok(())
}
pub fn run(directory: &Path) -> Result<()> {
    validate_directory(directory)?;
    let request = directory.join("request.json");
    os::plain(&request)?;
    ensure!(fs::metadata(&request)?.len() <= 16384, "管理请求过大");
    let command: Request = serde_json::from_slice(&fs::read(&request)?)?;
    let result = execute(directory, command);
    let response = result
        .as_ref()
        .map(Clone::clone)
        .map_err(|error| format!("{error:#}"));
    write_json(&directory.join("result.json"), &response)?;
    if let Err(error) = result {
        crate::diagnostics::log(&format!("Management failed: {error:#}"));
    }
    // The result receipt carries the precise failure; no extra helper error dialog.
    Ok(())
}
fn execute(directory: &Path, request: Request) -> Result<String> {
    let _lock = setup::Lock::new()?;
    let paths = setup::Paths::new()?;
    scm::assert_owned(&paths.exe)?;
    let stage = |text| progress(directory, text, false, false);
    match request {
        Request::Start => {
            settings(directory, &paths)?;
            stage("正在启动后台服务…")?;
            start(&paths)?;
            Ok("后台服务运行中".into())
        }
        Request::Save { port, password } => {
            stage("正在保存连接设置…")?;
            let bytes = password.decrypt(
                "LanDesk.SettingsRequest",
                &landesk_storage::LocalSecrets::beside(&directory.join("request.json")),
            )?;
            let password = Zeroizing::new(String::from_utf8(bytes.to_vec())?);
            let path = paths.transport.join("config.json");
            let previous = crate::adapters::identity::read(&path)?;
            let port = port.unwrap_or(previous.port);
            ensure!(port != 0, "端口必须在 1–65535 之间");
            landesk_core::domain::password::validate(&password)?;
            // Keep the active port allowed while also preparing the next boot's port.
            // The transport runs as LocalService and cannot edit the firewall on boot.
            let _com = os::Com::new()?;
            let rule = os::firewall(&paths.exe)?.context("通信防火墙规则缺失")?;
            let old_ports = unsafe { rule.LocalPorts()? };
            let service = scm::open_access(scm::TRANSPORT, SERVICE_QUERY_STATUS)?
                .context("通信服务未安装")?;
            let ports = if scm::status(&service)?.dwCurrentState == SERVICE_RUNNING {
                let active = password_info()?.active_port;
                if active == port {
                    port.to_string()
                } else {
                    format!("{active},{port}")
                }
            } else {
                port.to_string()
            };
            unsafe {
                rule.SetLocalPorts(&windows::core::BSTR::from(ports))?;
            }
            match landesk_core::application::password::change_connection(
                &crate::adapters::identity::IdentityFile(path.clone()),
                &landesk_storage::LocalSecrets::beside(&path),
                password,
                port,
            ) {
                PasswordOutcome::Saved { warning, .. } => Ok(match warning {
                    Some(w) => format!("已保存，下次重启后生效；{w}"),
                    None => "已保存，下次重启后生效".into(),
                }),
                PasswordOutcome::Failed(error) => {
                    if matches!(
                        &error,
                        landesk_core::domain::identity::CommitFailure::BeforeCommit(_)
                    ) {
                        unsafe { rule.SetLocalPorts(&old_ports) }
                            .with_context(|| format!("{error}；防火墙回滚失败"))?;
                    }
                    Err(error.into())
                }
            }
        }
        Request::Startup(preferences) => {
            stage("正在保存托盘设置…")?;
            set_startup(&paths, preferences)?;
            Ok("托盘设置已保存".into())
        }
        Request::Restart => {
            settings(directory, &paths)?;
            // Validate persisted settings before interrupting a working session.
            let config = crate::adapters::identity::read(&paths.transport.join("config.json"))?;
            let _ = landesk_core::application::password::recover_password(
                &config,
                &landesk_storage::LocalSecrets::beside(&paths.transport.join("config.json")),
            )?;
            stage("正在停止桌面服务…")?;
            scm::stop(scm::SERVER)?;
            stage("正在停止通信服务…")?;
            scm::stop(scm::TRANSPORT)?;
            stage("正在应用连接设置并启动服务…")?;
            start(&paths).context("重启未完成，当前连接已断开；请修正错误后再次重启")?;
            Ok(format!(
                "后台服务已重启，新设置已生效（端口 {}）",
                config.port
            ))
        }
        Request::Uninstall {
            owner,
            remove_display,
        } => {
            setup::launch_managed_uninstall(directory, owner, remove_display)?;
            Ok("正在卸载…".into())
        }
    }
}
pub fn start(paths: &setup::Paths) -> Result<()> {
    // A running transport keeps its startup snapshot and its matching firewall rule.
    let transport =
        scm::open_access(scm::TRANSPORT, SERVICE_QUERY_STATUS)?.context("通信服务未安装")?;
    if scm::status(&transport)?.dwCurrentState != SERVICE_RUNNING {
        paths.prepare_logs()?;
        let _com = os::Com::new()?;
        let config = crate::adapters::identity::read(&paths.transport.join("config.json"))?;
        let rule = os::firewall(&paths.exe)?.context("通信服务防火墙规则缺失，请重新安装")?;
        unsafe {
            rule.SetLocalPorts(&windows::core::BSTR::from(config.port.to_string()))?;
        }
        scm::start(scm::TRANSPORT)?;
    }
    scm::start(scm::SERVER)
}
fn set_startup(paths: &setup::Paths, preferences: Preferences) -> Result<()> {
    let service = scm::open_access(scm::SERVER, SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG)?
        .context("桌面服务未安装")?;
    let previous = scm::configuration(&service)?.start;
    let old = Preferences::load()?;
    let old_run = os::read(os::RUN, scm::SERVER)?;
    let command = os::text(&format!("\"{}\" --startup-ui", paths.exe.display()));
    let change = |start| {
        super::desktop::checked(
            unsafe {
                ChangeServiceConfigW(
                    service.0,
                    SERVICE_NO_CHANGE,
                    start,
                    SERVICE_NO_CHANGE,
                    std::ptr::null(),
                    std::ptr::null(),
                    null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            "设置服务开机启动",
        )
    };
    let result = (|| {
        change(if preferences.automatic {
            SERVICE_AUTO_START
        } else {
            SERVICE_DEMAND_START
        })?;
        os::put(
            os::RUN,
            scm::SERVER,
            preferences.automatic.then_some(&command),
        )?;
        preferences.save(&paths.data.join("preferences.json"))
    })();
    if let Err(error) = result {
        // Attempt every restoration even if another one fails.
        let restores = [
            change(previous),
            os::put(os::RUN, scm::SERVER, old_run.as_ref()),
            old.save(&paths.data.join("preferences.json")),
        ];
        let failures: Vec<_> = restores
            .into_iter()
            .filter_map(Result::err)
            .map(|e| format!("{e:#}"))
            .collect();
        ensure!(
            failures.is_empty(),
            "{error:#}；启动设置回滚失败：{}",
            failures.join("；")
        );
        return Err(error);
    }
    Ok(())
}
pub fn service_states() -> Result<[String; 2]> {
    let mut states = Vec::new();
    for name in [scm::TRANSPORT, scm::SERVER] {
        let state = match scm::open_access(name, SERVICE_QUERY_STATUS)? {
            Some(handle) => match scm::status(&handle)?.dwCurrentState {
                SERVICE_RUNNING => "运行中",
                SERVICE_STOPPED => "停止",
                SERVICE_START_PENDING => "启动中",
                SERVICE_STOP_PENDING => "停止中",
                _ => "状态未知",
            },
            None => "未安装",
        };
        states.push(state.to_owned());
    }
    Ok(states.try_into().unwrap())
}
pub fn password_info() -> Result<super::password_pipe::PasswordInfo> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(super::password_pipe::read())
}
pub fn invoke(executable: PathBuf, arguments: String) -> Result<()> {
    privilege::elevated(null_mut(), &executable, &arguments, true)
}
