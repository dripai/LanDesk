//! Integrated native setup. There are no PowerShell, cmd, sc, or external script invocations.
use super::desktop::Handle;
use super::desktop::checked;
use super::desktop::wide;
use super::setup_dialog as dialog;
use super::setup_services as scm;
use super::setup_support as os;
use super::virtual_display as display;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use landesk_core::application::maintenance::Transaction;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::ptr::null;
use std::ptr::null_mut;
use std::time::Duration;
use std::time::Instant;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::SystemInformation::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Shell::IsUserAnAdmin;

const RESOURCES: &[(&str, &[u8])] = &[
    (
        "MttVDD.dll",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/virtual-display/MttVDD.dll"
        )),
    ),
    (
        "MttVDD.inf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/virtual-display/MttVDD.inf"
        )),
    ),
    (
        "mttvdd.cat",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/virtual-display/mttvdd.cat"
        )),
    ),
    (
        "vdd_settings.xml",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/virtual-display/vdd_settings.xml"
        )),
    ),
    (
        "LICENSE",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/virtual-display/LICENSE"
        )),
    ),
];
fn unique() -> Result<String> {
    let mut value = [0u8; 16];
    getrandom::fill(&mut value).map_err(|e| anyhow::anyhow!("安装随机数失败: {e}"))?;
    Ok(value.iter().map(|byte| format!("{byte:02x}")).collect())
}
pub(super) struct Lock(Handle);
impl Lock {
    pub(super) fn new() -> Result<Self> {
        ensure!(unsafe { IsUserAnAdmin() } != 0, "安装或卸载需要管理员权限");
        let handle = Handle::new(
            unsafe { CreateMutexW(null(), 0, wide(r"Global\LanDeskServerSetup").as_ptr()) },
            "创建安装互斥锁",
        )?;
        let wait = unsafe { WaitForSingleObject(handle.0, 0) };
        match wait {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {}
            WAIT_TIMEOUT => {
                anyhow::bail!("另一个 LanDesk 维护操作正在运行，请稍后重试")
            }
            WAIT_FAILED => {
                return Err(std::io::Error::last_os_error()).context("等待维护互斥锁失败");
            }
            other => anyhow::bail!("维护互斥锁返回未知状态 {other}"),
        }
        Ok(Self(handle))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            if ReleaseMutex(self.0.0) == 0 {
                crate::diagnostics::log(&format!(
                    "释放维护锁失败: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
}

struct Payload(PathBuf);
impl Payload {
    fn new() -> Result<Self> {
        let paths = Paths::new()?;
        paths.prepare_logs()?;
        let path = paths
            .data
            .join("runtime/setup")
            .join(format!("operation-{}", unique()?));
        os::create_directory(&path, os::PRIVATE)?;
        let payload = Self(path);
        fs::write(
            payload.0.join("bootstrap-owner.txt"),
            "LanDeskServer embedded setup v2",
        )?;
        Ok(payload)
    }
    fn driver(&self) -> Result<PathBuf> {
        let path = self.0.join("driver");
        os::create_directory(&path, os::PRIVATE)?;
        for (name, bytes) in RESOURCES {
            fs::write(path.join(name), bytes)?;
        }
        for (name, expected) in [
            (
                "MttVDD.dll",
                "c9ca837f57a98fbd43bc416a7f535a95843626e7759eaf85cf0cd7ce334dbb05",
            ),
            (
                "MttVDD.inf",
                "550d211fe481e74dfe3f9d724ed78be48b3a9113405965d683d9373e8d672f5d",
            ),
            (
                "mttvdd.cat",
                "08a0093fc9b2e32b287a6f8a77ca4de0a31830d29fc33d2b13a918dc859468f6",
            ),
        ] {
            ensure!(
                format!("{:x}", Sha256::digest(fs::read(path.join(name))?)) == expected,
                "内置驱动哈希不匹配: {name}"
            );
        }
        os::verify_catalog(&path.join("mttvdd.cat"))?;
        Ok(path.join("MttVDD.inf"))
    }
}
impl Drop for Payload {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            crate::diagnostics::log(&format!(
                "临时安装目录清理失败 {}: {error}",
                self.0.display()
            ));
        }
    }
}
pub(super) struct Paths {
    pub(super) app: PathBuf,
    pub(super) exe: PathBuf,
    pub(super) data: PathBuf,
    pub(super) transport: PathBuf,
    pub(super) logs: PathBuf,
}
impl Paths {
    pub(super) fn new() -> Result<Self> {
        let app = os::environment("ProgramFiles")?.join("LanDeskServer");
        let data = landesk_storage::data_directory(&app.join("LanDeskServer.exe"), "server")?;
        Ok(Self {
            exe: app.join("LanDeskServer.exe"),
            app,
            transport: data.join("config"),
            logs: data.join("logs"),
            data,
        })
    }
    pub(super) fn prepare_logs(&self) -> Result<()> {
        if !self.app.exists() {
            os::create_directory(&self.app, os::APP)?;
        }
        let root = self.app.join("data");
        if self.data.parent() == Some(root.as_path()) {
            os::ensure_directory(&root, os::APP, &["S-1-5-32-545"])?;
            os::protect(&root, os::APP)?;
        }
        os::ensure_directory(&self.data, os::APP, &["S-1-5-32-545"])?;
        os::protect(&self.data, os::APP)?;
        let runtime = self.data.join("runtime");
        os::ensure_directory(&runtime, os::APP, &["S-1-5-32-545"])?;
        os::ensure_directory(&runtime.join("setup"), os::PRIVATE, &[])?;
        os::ensure_request_directory(&runtime.join("ui"))?;
        os::ensure_directory(&self.logs, os::LOGS, &["S-1-5-32-545"])?;
        for name in [
            "transport.log",
            "transport.previous.log",
            "ui.log",
            "ui.previous.log",
        ] {
            let file = self.logs.join(name);
            if file.exists() {
                os::plain(&file)?;
            } else {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file)?;
            }
            os::protect(
                &file,
                if name.starts_with("ui.") {
                    "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGW;;;BU)"
                } else {
                    "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGW;;;LS)(A;;GR;;;BU)"
                },
            )?;
        }
        Ok(())
    }
    fn logs(&self, role: &str) -> Result<()> {
        self.prepare_logs()?;
        let file = self.logs.join(format!("{role}.log"));
        if file.exists() {
            os::plain(&file)?;
            let previous = self.logs.join(format!("{role}.previous.log"));
            if previous.exists() {
                os::plain(&previous)?;
            }
            fs::copy(&file, previous)?;
        }
        fs::write(&file, [])?;
        crate::diagnostics::initialize(file)?;
        crate::diagnostics::log("Native Windows setup started");
        Ok(())
    }
    fn transport(&self) -> Result<landesk_core::domain::identity::ServerIdentity> {
        self.prepare_logs()?;
        os::ensure_directory(&self.transport, os::TRANSPORT_ACL, &["S-1-5-19"])?;
        let path = self.transport.join("config.json");
        ensure!(!path.exists(), "全新安装前连接配置未清理");
        let password = landesk_core::domain::password::generate()?;
        crate::adapters::identity::write_new(
            &crate::adapters::identity::create_stored(&password, 2222, &path)?,
            &path,
        )?;
        os::plain(&path)?;
        let config = crate::adapters::identity::read(&path)?;
        let _ = landesk_core::application::password::recover_password(
            &config,
            &landesk_storage::LocalSecrets::beside(&path),
        )?;
        Ok(config)
    }
}

fn desktop_ready(config: &landesk_core::domain::identity::ServerIdentity) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    runtime.block_on(async {
        loop {
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                super::pipe::health(&config.backend_key),
            )
            .await;
            if matches!(result, Ok(Ok(()))) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                match result {
                    Ok(Err(error)) => {
                        return Err(error).context("桌面组件未就绪，请查看 worker.log");
                    }
                    _ => anyhow::bail!("桌面组件响应超时，请查看 worker.log"),
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
}
fn restore_policy(data: &Path) -> Result<()> {
    let marker = data.join("sas-policy-created.txt");
    if marker.exists() {
        os::plain(&marker)?;
        if os::read(os::POLICY, os::SAS)? == Some(os::number(1)) {
            os::put(os::POLICY, os::SAS, None)?;
        } else {
            crate::diagnostics::log("SAS 策略已被其他程序改变，保留当前值");
        }
        fs::remove_file(marker)?;
    }
    Ok(())
}
fn install_display(payload: &Payload, transaction: &mut Transaction) -> Result<bool> {
    let state = display::status()?;
    crate::diagnostics::log(&format!("Display status: {state}"));
    if state["available"] == true {
        crate::diagnostics::log("复用已运行的 MttVDD；不修改设备、配置和归属");
        return Ok(false);
    }
    ensure!(
        state["installed"] != true
            && state["devices"]
                .as_array()
                .context("设备状态缺失")?
                .is_empty(),
        "已有 MttVDD 未启动: {state}"
    );
    // Driver 25.7.23 reads VDDPATH (REG_SZ) before loading vdd_settings.xml.
    let registry = r"SOFTWARE\MikeTheTech\VirtualDisplayDriver";
    let config = Paths::new()?.data.join("display");
    ensure!(
        wide(&config.to_string_lossy()).len() <= 260,
        "显示配置路径超过驱动 MAX_PATH 限制"
    );
    os::plain_chain(&config)?;
    let previous = os::RegistrySnapshot::read(registry, &["VDDPATH"])?;
    let created = !config.exists();
    if !created {
        os::plain(&config.join("landesk-owner.txt"))?;
        ensure!(
            fs::read_to_string(config.join("landesk-owner.txt"))?
                == r"LanDesk ROOT\LANDESKDISPLAY\0000 v1",
            "显示配置目录不属于 LanDesk"
        );
        crate::setup::validate_display_xml(&fs::read(config.join("vdd_settings.xml"))?)?;
    } else {
        os::create_directory(
            &config,
            "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;AU)",
        )?;
    }
    let rollback = config.clone();
    transaction.record("显示配置与注册路径", move || {
        ensure!(
            display::status()?["installed"] != true,
            "设备未移除，显示配置已保留"
        );
        previous.restore()?;
        if created {
            for name in ["vdd_settings.xml", "landesk-owner.txt"] {
                let path = rollback.join(name);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
            fs::remove_dir(&rollback)?;
        }
        Ok(())
    });
    if created {
        fs::write(
            config.join("landesk-owner.txt"),
            r"LanDesk ROOT\LANDESKDISPLAY\0000 v1",
        )?;
        fs::write(
            config.join("vdd_settings.xml"),
            RESOURCES
                .iter()
                .find(|(name, _)| *name == "vdd_settings.xml")
                .context("内置显示配置缺失")?
                .1,
        )?;
    }
    os::put(
        registry,
        "VDDPATH",
        Some(&os::text(&config.to_string_lossy())),
    )?;
    let result = display::install(&payload.driver()?)?;
    crate::diagnostics::log(&format!("Display install: {result}"));
    if result["created"] == true {
        transaction.record("虚拟显示设备", || {
            display::uninstall()?;
            Ok(())
        });
    }
    Ok(result["reboot_required"] == true)
}

/// File replacement leaves unrelated files untouched, restoring bytes and ACLs on failure.
fn replace_executable(source: &Path, target: &Path) -> Result<()> {
    use std::io::Write;
    let stage = target.with_file_name(format!("LanDeskServer.next-{}.exe", unique()?));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)?;
        os::protect(
            &stage,
            "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;LS)(A;;0x1200a9;;;BU)",
        )?;
        file.write_all(&fs::read(source)?)?;
        file.sync_all()?;
        drop(file);
        checked(
            unsafe {
                MoveFileExW(
                    wide(&stage.to_string_lossy()).as_ptr(),
                    wide(&target.to_string_lossy()).as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            },
            "替换服务端程序",
        )
    })();
    if stage.exists() {
        fs::remove_file(&stage).context("程序替换失败，临时文件清理失败")?;
    }
    result.with_context(|| format!("{} → {}", source.display(), target.display()))
}
fn prepare_application(paths: &Paths, transaction: &mut Transaction) -> Result<Option<PathBuf>> {
    let source = std::env::current_exe()?;
    let existed = paths.app.exists();
    let old_acl = if existed {
        ensure!(paths.app.is_dir(), "安装目录路径不是目录");
        os::plain_chain(&paths.app)?;
        let entries = fs::read_dir(&paths.app)?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>>>()?;
        crate::diagnostics::log(&format!("保留安装目录已有内容: {}", entries.join(", ")));
        Some(os::Security::read(&paths.app)?)
    } else {
        None
    };
    let marker = paths.app.join("landesk-owner.txt");
    let marker_existed = marker.exists();
    if marker_existed {
        os::owned(&paths.app, &["S-1-5-19", "S-1-5-32-545"])?;
    }
    let exe_existed = paths.exe.exists();
    let same = if exe_existed {
        os::plain(&paths.exe)?;
        fs::read(&paths.exe)? == fs::read(&source)?
    } else {
        false
    };
    ensure!(
        !exe_existed || marker_existed || same,
        "同名 EXE 无归属标记且不同于安装包，未覆盖"
    );
    let old_exe_acl = if exe_existed {
        Some(os::Security::read(&paths.exe)?)
    } else {
        None
    };
    let backup = if exe_existed && !same {
        let backup = paths
            .data
            .join(format!("server-install-backup-{}.exe", unique()?));
        fs::copy(&paths.exe, &backup)
            .with_context(|| format!("备份旧程序到 {}", backup.display()))?;
        Some(backup)
    } else {
        None
    };
    let app = paths.app.clone();
    let exe = paths.exe.clone();
    let restored_backup = backup.clone();
    transaction.record("程序及目录权限", move || {
        if let Some(backup) = &restored_backup {
            replace_executable(backup, &exe)?;
        } else if !exe_existed && exe.exists() {
            fs::remove_file(&exe)?;
        }
        if let Some(acl) = old_exe_acl {
            acl.restore(&exe)?;
        }
        let marker = app.join("landesk-owner.txt");
        if !marker_existed && marker.exists() {
            fs::remove_file(marker)?;
        }
        if let Some(acl) = old_acl {
            acl.restore(&app)?;
        } else if app.exists() && fs::read_dir(&app)?.next().is_none() {
            fs::remove_dir(&app)?;
        }
        if let Some(backup) = restored_backup {
            fs::remove_file(backup)?;
        }
        Ok(())
    });
    if existed {
        os::protect(&paths.app, os::APP)?;
    } else {
        os::create_directory(&paths.app, os::APP)?;
    }
    if !marker_existed {
        fs::write(&marker, os::OWNER)?;
    }
    if !same {
        replace_executable(&source, &paths.exe)?;
    }
    os::protect(
        &paths.exe,
        "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;LS)(A;;0x1200a9;;;BU)",
    )?;
    Ok(backup)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RemovalStage {
    Desktop,
    Transport,
    Managers,
    Services,
    Entries,
    Display,
    Connection,
    Program,
}
impl RemovalStage {
    fn text(self) -> &'static str {
        match self {
            Self::Desktop => "正在停止桌面服务…",
            Self::Transport => "正在停止通信服务…",
            Self::Managers => "正在关闭其他管理进程…",
            Self::Services => "正在删除后台服务…",
            Self::Entries => "正在移除启动入口、快捷方式和防火墙规则…",
            Self::Display => "正在移除 LanDesk 自建虚拟显示设备…",
            Self::Connection => "正在清理连接配置…",
            Self::Program => "正在移除程序和应用登记…",
        }
    }
}
/// Shared removal path for uninstall and fresh installation. No old connection
/// configuration is deserialized: missing, legacy and damaged files are removed
/// in exactly the same way after validating the managed resources.
fn remove_installation(
    paths: &Paths,
    remove_display: bool,
    clear_connection: bool,
    launcher: u32,
    stage: impl Fn(RemovalStage) -> Result<()>,
) -> Result<bool> {
    scm::assert_owned(&paths.exe)?;
    let marker = paths.app.join("landesk-owner.txt");
    let owned_app = marker.exists();
    if owned_app {
        os::owned(&paths.app, &["S-1-5-19", "S-1-5-32-545"])?;
    }
    if paths.exe.exists() {
        os::plain(&paths.exe)?;
        ensure!(
            owned_app || fs::read(&paths.exe)? == fs::read(std::env::current_exe()?)?,
            "同名程序没有 LanDesk 归属标记，未删除"
        );
    }
    let _ = os::uninstall_snapshot(&paths.app)?;
    let firewall = os::firewall(&paths.exe)?.is_some();
    let shortcut = os::shortcut_path()?;
    if shortcut.exists() {
        os::shortcut_owned(&shortcut, &paths.exe)?;
    }
    let startup = os::read(os::RUN, scm::SERVER)?;
    ensure!(
        startup.is_none()
            || startup
                == Some(os::text(&format!(
                    "\"{}\" --startup-ui",
                    paths.exe.display()
                ))),
        "启动入口不属于 LanDesk"
    );
    let connection = paths.transport.join("config.json");
    if clear_connection && paths.transport.exists() {
        os::owned(&paths.transport, &["S-1-5-19"])?;
        if connection.exists() {
            os::plain(&connection)?;
        }
    }
    stage(RemovalStage::Desktop)?;
    scm::stop(scm::SERVER)?;
    stage(RemovalStage::Transport)?;
    scm::stop(scm::TRANSPORT)?;
    stage(RemovalStage::Managers)?;
    os::close_installed_processes(&paths.exe, launcher)?;
    stage(RemovalStage::Services)?;
    scm::delete(scm::SERVER)?;
    scm::delete(scm::TRANSPORT)?;
    stage(RemovalStage::Entries)?;
    if startup.is_some() {
        os::put(os::RUN, scm::SERVER, None)?;
    }
    if shortcut.exists() {
        fs::remove_file(shortcut)?;
    }
    if firewall {
        os::remove_firewall()?;
    }
    restore_policy(&paths.data)?;
    let mut reboot = false;
    if remove_display {
        stage(RemovalStage::Display)?;
        let result = display::uninstall()?;
        reboot = result["reboot_required"] == true;
        crate::diagnostics::log(&format!("Display uninstall: {result}"));
        let display_path = paths.data.join("display");
        let registry = r"SOFTWARE\MikeTheTech\VirtualDisplayDriver";
        if result["removed"] == true
            && os::read(registry, "VDDPATH")? == Some(os::text(&display_path.to_string_lossy()))
        {
            os::put(registry, "VDDPATH", None)?;
        }
    }
    if clear_connection {
        stage(RemovalStage::Connection)?;
        if connection.exists() {
            fs::remove_file(&connection)?;
        }
        let preferences = paths.data.join("preferences.json");
        if preferences.exists() {
            os::plain(&preferences)?;
            fs::remove_file(preferences)?;
        }
    }
    stage(RemovalStage::Program)?;
    // A manually invoked installer may itself be in Program Files. That image
    // is already the new EXE; keep it and its launcher while removing services.
    if paths.exe.exists()
        && fs::canonicalize(&paths.exe)? != fs::canonicalize(std::env::current_exe()?)?
    {
        fs::remove_file(&paths.exe)?;
    }
    if owned_app && !paths.exe.exists() {
        fs::remove_file(marker)?;
    }
    if paths.app.exists() && fs::read_dir(&paths.app)?.next().is_none() {
        fs::remove_dir(&paths.app)?;
    }
    os::delete_key(os::UNINSTALL)?;
    Ok(reboot)
}
pub(super) fn install(launcher: u32) -> Result<()> {
    let _lock = Lock::new()?;
    let paths = Paths::new()?;
    paths.logs("install")?;
    let result = (|| {
        let _com = os::Com::new()?;
        let progress = dialog::Progress::new("正在安装 LanDeskServer")?;
        remove_installation(&paths, false, true, launcher, |stage| {
            progress.stage(stage.text())
        })
        .context("旧安装清理未完成，新版尚未安装；请保留日志并重新运行安装程序")?;
        progress.stage("旧安装已清理，开始全新安装…")?;
        let mut tx = Transaction::new();
        let connection = paths.transport.join("config.json");
        tx.record("本次连接配置", move || {
            if connection.exists() {
                os::plain(&connection)?;
                fs::remove_file(&connection)?;
            }
            Ok(())
        });
        let result = (|| -> Result<(bool, Option<PathBuf>)> {
            progress.stage("生成并保存新的加密连接密码…")?;
            let config = paths.transport()?;
            let payload = Payload::new()?;
            let shortcut = os::shortcut_path()?;
            ensure!(os::firewall(&paths.exe)?.is_none(), "旧防火墙规则未清理");
            ensure!(!shortcut.exists(), "旧快捷方式未清理");
            ensure!(
                os::read(os::RUN, scm::SERVER)?.is_none(),
                "旧启动入口未清理"
            );
            let uninstall = os::uninstall_snapshot(&paths.app)?;
            let policy = os::read(os::POLICY, os::SAS)?;
            if let Some(value) = &policy {
                ensure!(
                    *value == os::number(1) || *value == os::number(3),
                    "已有策略禁止服务生成 Ctrl+Alt+Delete"
                );
            }
            progress.stage("安装或复用虚拟显示器…")?;
            let reboot = install_display(&payload, &mut tx)?;
            progress.stage("准备服务端程序…")?;
            let backup = prepare_application(&paths, &mut tx)?;
            if policy.is_none() {
                let data = paths.data.clone();
                tx.record("SAS 策略", move || restore_policy(&data));
                fs::write(paths.data.join("sas-policy-created.txt"), "1")?;
                os::put(os::POLICY, os::SAS, Some(&os::number(1)))?;
            }
            progress.stage("通过 Windows API 注册后台服务…")?;
            let transport = scm::create(&paths.exe, scm::TRANSPORT, true, None)?;
            tx.record("通信服务", || scm::delete(scm::TRANSPORT));
            crate::diagnostics::log("LanDeskTransport 已注册，账号 LocalService");
            scm::configure(&transport, true)?;
            drop(transport);
            os::add_firewall(&paths.exe, config.port)?;
            tx.record("防火墙规则", os::remove_firewall);
            let server = scm::create(&paths.exe, scm::SERVER, false, Some(scm::TRANSPORT))?;
            tx.record("桌面服务", || scm::delete(scm::SERVER));
            crate::diagnostics::log(
                "LanDeskServer 已注册，账号 LocalSystem，依赖 LanDeskTransport",
            );
            scm::configure(&server, false)?;
            drop(server);
            progress.stage("启动服务并检查桌面组件…")?;
            scm::start(scm::SERVER)?;
            if !reboot {
                desktop_ready(&config)?;
            } else {
                crate::diagnostics::log("显示驱动要求重启，服务已启动；桌面画面在重启后验收");
            }
            progress.stage("保存开机启动和托盘设置…")?;
            let run = os::RegistrySnapshot::read(os::RUN, &[scm::SERVER])?;
            tx.record("启动入口", move || run.restore());
            let preferences = paths.data.join("preferences.json");
            let rollback = preferences.clone();
            tx.record("托盘设置", move || {
                if rollback.exists() {
                    fs::remove_file(rollback)?;
                }
                Ok(())
            });
            super::management::Preferences::default().save(&preferences)?;
            os::put(
                os::RUN,
                scm::SERVER,
                Some(&os::text(&format!(
                    "\"{}\" --startup-ui",
                    paths.exe.display()
                ))),
            )?;
            let restored_shortcut = shortcut.clone();
            tx.record("快捷方式", move || {
                if restored_shortcut.exists() {
                    fs::remove_file(restored_shortcut)?;
                }
                Ok(())
            });
            os::create_shortcut(&shortcut, &paths.exe)?;
            tx.record("卸载入口", move || uninstall.restore());
            os::register_uninstall(&paths.app)?;
            Ok((reboot, backup))
        })();
        match result {
            Ok((reboot, backup)) => {
                if let Some(backup) = backup {
                    fs::remove_file(backup).context("安装成功，旧程序备份清理失败")?;
                }
                progress.stage("安装完成")?;
                Ok(reboot)
            }
            Err(error) => Err(tx.rollback(error)).context("旧安装已清理，新版安装未完成；已尝试回滚本次创建的组件，详情见日志；可重新运行 EXE"),
        }
    })();
    if let Err(error) = &result {
        crate::diagnostics::log(&format!("Native installation FAILED: {error:#}"));
    }
    drop(_lock);
    if result? {
        dialog::complete("安装完成。Windows 要求重启以启用虚拟显示驱动，请重启后连接。");
    }
    Ok(())
}

pub(super) fn prepare_uninstall() -> Result<()> {
    ensure!(unsafe { IsUserAnAdmin() } != 0, "卸载需要管理员权限");
    let payload = Payload::new()?;
    let helper = payload.0.join("LanDeskUninstall.exe");
    fs::copy(std::env::current_exe()?, &helper)?;
    os::protect(&helper, "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)")?;
    // A running Windows image cannot be removed portably. Queue only this private helper
    // and its directory for the next reboot, without requiring or initiating a reboot.
    for path in [&helper, &payload.0.join("bootstrap-owner.txt"), &payload.0] {
        checked(
            unsafe {
                MoveFileExW(
                    wide(&path.to_string_lossy()).as_ptr(),
                    null(),
                    MOVEFILE_DELAY_UNTIL_REBOOT,
                )
            },
            "登记卸载临时文件清理",
        )?;
    }
    std::process::Command::new(&helper)
        .arg("--run-uninstall")
        .arg(std::process::id().to_string())
        .current_dir(os::environment("SystemRoot")?)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("启动原生卸载进程")?;
    std::mem::forget(payload);
    Ok(())
}
pub(super) fn run_uninstall(parent: u32) -> Result<()> {
    let result = (|| {
        let exe = std::env::current_exe()?;
        let directory = exe.parent().context("卸载目录缺失")?;
        let name = directory
            .file_name()
            .context("卸载目录名称缺失")?
            .to_string_lossy();
        ensure!(
            directory.parent() == Some(Paths::new()?.data.join("runtime/setup").as_path())
                && name
                    .strip_prefix("operation-")
                    .is_some_and(|s| s.len() == 32 && s.bytes().all(|v| v.is_ascii_hexdigit()))
                && fs::read_to_string(directory.join("bootstrap-owner.txt"))?
                    == "LanDeskServer embedded setup v2",
            "卸载进程不是内置组件"
        );
        let raw = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, parent) };
        if !raw.is_null() {
            let handle = Handle::new(raw, "打开卸载启动进程")?;
            ensure!(
                unsafe { WaitForSingleObject(handle.0, 30000) } == WAIT_OBJECT_0,
                "卸载启动进程未退出"
            );
        } else {
            ensure!(
                unsafe { GetLastError() } == ERROR_INVALID_PARAMETER,
                "读取卸载启动进程失败"
            );
        }
        let Some(remove_display) = dialog::uninstall_choice()? else {
            crate::diagnostics::log("卸载已取消");
            return Ok(());
        };
        let _lock = Lock::new()?;
        let paths = Paths::new()?;
        paths.logs("uninstall")?;
        let _com = os::Com::new()?;
        let progress = dialog::Progress::new("正在卸载 LanDeskServer")?;
        let reboot = remove_installation(&paths, remove_display, false, 0, |stage| {
            progress.stage(stage.text())
        })
        .context("卸载未完成，部分组件可能已移除；详情见日志")?;
        progress.stage("卸载完成，密码配置、日志和其他文件已保留")?;
        drop(_lock);
        dialog::complete(if reboot {
            "卸载完成。配置和日志已保留。Windows 要求重启以完成显示设备移除。"
        } else {
            "卸载完成。配置和日志已保留。"
        });
        Ok(())
    })();
    if let Err(error) = &result {
        crate::diagnostics::log(&format!("Native uninstall FAILED: {error:#}"));
        super::privilege::error_box(null_mut(), format!("卸载未完成：{error:#}。请保留日志。"));
    }
    // The EXE itself stays queued for deletion at the next reboot; no external cleanup script.
    result
}
pub(super) fn export() -> Result<()> {
    let paths = Paths::new()?;
    let path = export_at(&paths)?;
    crate::diagnostics::log(&format!("日志已导出: {}", path.display()));
    Ok(())
}
fn export_at(paths: &Paths) -> Result<PathBuf> {
    os::owned(&paths.data, &["S-1-5-32-545"])?;
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    // Diagnostics remain available while maintenance owns the mutex. A snapshot
    // may span log rotation; failed reads are reported, never replaced by other data.
    let snapshot = (|| -> Result<()> {
        os::owned(&paths.logs, &["S-1-5-32-545"])?;
        for entry in fs::read_dir(&paths.logs)? {
            let result = (|| -> Result<()> {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if crate::setup::diagnostic_name(&name) {
                    os::plain(&entry.path())?;
                    entries.push((
                        name,
                        fs::read(entry.path())
                            .with_context(|| entry.path().display().to_string())?,
                    ));
                }
                Ok(())
            })();
            if let Err(error) = result {
                errors.push(format!("{error:#}"));
            }
        }
        Ok(())
    })();
    if let Err(error) = snapshot {
        errors.push(format!("{error:#}"));
    }
    let mut windows = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    ensure!(
        unsafe { windows_sys::Wdk::System::SystemServices::RtlGetVersion(&mut windows) } == 0,
        "读取 Windows 版本失败"
    );
    let status = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"log_errors":errors,"windows":{"major":windows.dwMajorVersion,"minor":windows.dwMinorVersion,"build":windows.dwBuildNumber},"services":scm::report().unwrap_or_else(|error|serde_json::json!({"error":format!("{error:#}")})),"display":display::status().unwrap_or_else(|error|serde_json::json!({"error":format!("{error:#}")})),"interactive":super::interactive::report()});
    entries.push(("status.json".into(), serde_json::to_vec_pretty(&status)?));
    // Keep exports outside the runtime log directory: missing logs remain a
    // visible fault until the explicit repair operation provisions them.
    let exports = paths.data.join("diagnostics");
    os::ensure_directory(&exports, os::LOGS, &["S-1-5-32-545"])?;
    let path = exports.join(format!("LanDeskServer-diagnostics-{}.zip", unique()?));
    let result: Result<()> = (|| {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let file = crate::setup::write_archive(file, &entries)?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() && path.exists() {
        fs::remove_file(&path).context("导出失败后的文件清理失败")?;
    }
    result?;
    Ok(path)
}

pub(super) fn status() -> Result<serde_json::Value> {
    Ok(
        serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"services":scm::report()?,"display":display::status()?,"interactive":super::interactive::report()}),
    )
}

/// The helper resides in retained runtime data, so only the installed EXE must exit.
pub(super) fn launch_managed_uninstall(
    directory: &Path,
    owner: u32,
    remove_display: bool,
) -> Result<()> {
    let payload = Payload::new()?;
    let helper = payload.0.join("LanDeskUninstall.exe");
    fs::copy(std::env::current_exe()?, &helper)?;
    os::protect(&helper, "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)")?;
    for path in [&helper, &payload.0.join("bootstrap-owner.txt"), &payload.0] {
        checked(
            unsafe {
                MoveFileExW(
                    wide(&path.to_string_lossy()).as_ptr(),
                    null(),
                    MOVEFILE_DELAY_UNTIL_REBOOT,
                )
            },
            "登记卸载辅助文件清理",
        )?;
    }
    let mut child = std::process::Command::new(&helper)
        .arg("--managed-uninstall")
        .arg(directory)
        .arg(owner.to_string())
        .arg(if remove_display { "1" } else { "0" })
        .arg(std::process::id().to_string())
        .current_dir(&payload.0)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    use std::os::windows::io::AsRawHandle;
    if let Err(error) = os::allow_requester_to_wait(child.as_raw_handle(), directory)
        .and_then(|()| super::management::write_json(&directory.join("helper.json"), &child.id()))
    {
        child.kill().context("卸载辅助进程登记失败，且无法终止")?;
        child.wait()?;
        return Err(error);
    }
    std::mem::forget(payload);
    Ok(())
}
pub(super) fn managed_uninstall(
    directory: &Path,
    owner: u32,
    remove_display: bool,
    parent: u32,
) -> Result<()> {
    super::management::validate_directory(directory)?;
    let result = (|| -> Result<()> {
        let parent_handle = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, parent) };
        if !parent_handle.is_null() {
            let parent_handle = Handle::new(parent_handle, "打开卸载准备进程")?;
            ensure!(
                unsafe { WaitForSingleObject(parent_handle.0, 30000) } == WAIT_OBJECT_0,
                "卸载准备进程未退出"
            );
        } else {
            ensure!(
                unsafe { GetLastError() } == ERROR_INVALID_PARAMETER,
                "无法验证卸载准备进程退出"
            );
        }
        // The launching maintenance operation releases its lock immediately after spawning us.
        let parent = unsafe {
            OpenProcess(
                SYNCHRONIZATION_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                owner,
            )
        };
        let owner = Handle::new(parent, "打开管理界面进程")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        // Do not acquire a recursive lock across processes or run concurrently with setup.
        let lock = loop {
            match Lock::new() {
                Ok(lock) => break lock,
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(error);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        let paths = Paths::new()?;
        paths.logs("uninstall")?;
        let _com = os::Com::new()?;
        let owner_id = unsafe { GetProcessId(owner.0) };
        let reboot = remove_installation(&paths, remove_display, false, owner_id, |stage| {
            let closing = stage == RemovalStage::Program;
            let text = stage.text();
            super::management::progress(
                directory,
                if closing {
                    "后台服务已移除，正在关闭界面并清理程序…"
                } else {
                    text
                },
                closing,
                false,
            )?;
            if closing {
                ensure!(
                    unsafe { WaitForSingleObject(owner.0, 30000) } == WAIT_OBJECT_0,
                    "管理界面未退出，程序文件尚未删除"
                );
            }
            Ok(())
        })?;
        super::management::progress(
            directory,
            if reboot {
                "卸载完成，配置和日志已保留；显示设备移除需要重启 Windows"
            } else {
                "卸载完成，配置和日志已保留"
            },
            false,
            false,
        )?;
        drop(lock);
        // Contains only our encrypted request and local request key.
        fs::remove_dir_all(directory).context("卸载完成，管理请求清理失败")?;
        Ok(())
    })();
    if let Err(error) = &result {
        let text = format!("卸载未完成：{error:#}");
        crate::diagnostics::log(&text);
        super::management::progress(directory, &text, false, true)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_payload() -> Result<Payload> {
        let path = std::env::temp_dir().join(format!("LanDeskSetupTest-{}", unique()?));
        os::create_directory(&path, os::PRIVATE)?;
        fs::write(
            path.join("bootstrap-owner.txt"),
            "LanDeskServer embedded setup v2",
        )?;
        Ok(Payload(path))
    }
    fn isolated_paths(payload: &Payload) -> Paths {
        let data = payload.0.join("data");
        let app = payload.0.join("app");
        Paths {
            exe: app.join("LanDeskServer.exe"),
            transport: data.join("config"),
            logs: data.join("logs"),
            app,
            data,
        }
    }
    #[test]
    #[ignore = "Requires Windows administrator; uses isolated directories and the maintenance mutex, never changes services"]
    fn native_setup_log_repair_preserves_config_and_export_works_while_locked() -> Result<()> {
        let payload = test_payload()?;
        let paths = isolated_paths(&payload);
        os::ensure_directory(&paths.data, os::PRIVATE, &[])?;
        os::ensure_directory(&paths.transport, os::TRANSPORT_ACL, &["S-1-5-19"])?;
        let config = paths.transport.join("config.json");
        // Repair must not even parse or rewrite this file.
        fs::write(&config, b"existing identity and ciphertext")?;
        let lock = Lock::new()?;
        // Use another thread: Windows mutexes are recursive on the owning thread.
        let export_paths = isolated_paths(&payload);
        let archive = std::thread::spawn(move || export_at(&export_paths))
            .join()
            .map_err(|_| anyhow::anyhow!("Export test panicked"))??;
        ensure!(!paths.logs.exists(), "Export silently created runtime logs");
        let mut zip = zip::ZipArchive::new(fs::File::open(archive)?)?;
        let status: serde_json::Value = serde_json::from_reader(zip.by_name("status.json")?)?;
        ensure!(
            !status["log_errors"].as_array().unwrap().is_empty(),
            "Missing log directory was not reported"
        );
        ensure!(
            zip.len() == 1,
            "Credentials or unrelated files were exported"
        );
        drop(zip);
        paths.prepare_logs()?;
        fs::write(paths.logs.join("transport.log"), b"existing log")?;
        paths.prepare_logs()?;
        ensure!(
            fs::read(&config)? == b"existing identity and ciphertext",
            "Repair changed connection configuration"
        );
        ensure!(
            fs::read(paths.logs.join("transport.log"))? == b"existing log",
            "Repair truncated logs"
        );
        ensure!(
            paths.logs.join("transport.previous.log").is_file(),
            "Rotation target missing"
        );
        drop(lock);
        Ok(())
    }
    #[test]
    #[ignore = "Requires Windows administrator; only acquires the maintenance mutex"]
    fn native_setup_maintenance_lock_reports_contention_and_releases() -> Result<()> {
        let lock = Lock::new()?;
        let error = std::thread::spawn(|| Lock::new().err().map(|error| error.to_string()))
            .join()
            .unwrap()
            .context("Concurrent maintenance unexpectedly acquired the lock")?;
        ensure!(
            error.contains("维护操作正在运行"),
            "Wrong contention error: {error}"
        );
        drop(lock);
        std::thread::spawn(|| Lock::new().map(drop)).join().unwrap()
    }
    #[test]
    #[ignore = "Requires Windows administrator and a clean machine; creates stopped test services and managed entries, never installs a driver"]
    fn native_setup_fresh_install_removes_legacy_or_missing_config_and_reuses_service_names()
    -> Result<()> {
        let _com = os::Com::new()?;
        for legacy_config_exists in [true, false] {
            let payload = test_payload()?;
            let app = payload.0.join("app");
            let data = payload.0.join("data");
            let paths = Paths {
                exe: app.join("LanDeskServer.exe"),
                transport: data.join("config"),
                logs: data.join("logs"),
                app,
                data,
            };
            let shortcut = os::shortcut_path()?;
            // Refuse to touch a developer's actual installation or settings.
            ensure!(
                scm::open(scm::SERVER)?.is_none() && scm::open(scm::TRANSPORT)?.is_none(),
                "Real LanDesk services exist; test refused"
            );
            ensure!(
                os::firewall(&paths.exe)?.is_none() && !shortcut.exists(),
                "Real LanDesk firewall/shortcut exists; test refused"
            );
            ensure!(
                os::read(os::RUN, scm::SERVER)?.is_none(),
                "Real LanDesk settings exist; test refused"
            );
            let _ = os::uninstall_snapshot(&paths.app)?;
            os::ensure_directory(&paths.app, os::APP, &["S-1-5-19", "S-1-5-32-545"])?;
            os::ensure_directory(&paths.data, os::PRIVATE, &[])?;
            os::ensure_directory(&paths.transport, os::TRANSPORT_ACL, &["S-1-5-19"])?;
            fs::write(&paths.exe, b"owned old test executable")?;
            fs::write(paths.app.join("aad-user-file"), b"keep unrelated contents")?;
            let driver = paths.app.join("virtual-display-25.7.23");
            fs::create_dir(&driver)?;
            fs::write(driver.join("existing-driver"), b"keep display contents")?;
            let config_path = paths.transport.join("config.json");
            if legacy_config_exists {
                fs::write(&config_path, br#"{"port":2222,"password_hash":"legacy"}"#)?;
            }
            let result = (|| -> Result<()> {
                drop(scm::create(&paths.exe, scm::TRANSPORT, true, None)?);
                drop(scm::create(
                    &paths.exe,
                    scm::SERVER,
                    false,
                    Some(scm::TRANSPORT),
                )?);
                os::add_firewall(&paths.exe, 2222)?;
                os::create_shortcut(&shortcut, &paths.exe)?;
                os::put(
                    os::RUN,
                    scm::SERVER,
                    Some(&os::text(&format!(
                        "\"{}\" --startup-ui",
                        paths.exe.display()
                    ))),
                )?;
                crate::platform::windows::management::Preferences::default()
                    .save(&paths.data.join("preferences.json"))?;
                os::register_uninstall(&paths.app)?;
                remove_installation(&paths, false, true, 0, |_| Ok(()))?;
                ensure!(
                    scm::open(scm::SERVER)?.is_none() && scm::open(scm::TRANSPORT)?.is_none(),
                    "Old services remain"
                );
                ensure!(
                    !config_path.exists() && !paths.exe.exists(),
                    "Old configuration/executable remains"
                );
                ensure!(
                    !shortcut.exists()
                        && os::firewall(&paths.exe)?.is_none()
                        && os::read(os::RUN, scm::SERVER)?.is_none(),
                    "Old managed entries remain"
                );
                ensure!(
                    fs::read(paths.app.join("aad-user-file"))? == b"keep unrelated contents"
                        && fs::read(driver.join("existing-driver"))? == b"keep display contents",
                    "Unrelated files or display contents changed"
                );
                let config = paths.transport()?;
                let password = landesk_core::application::password::recover_password(
                    &config,
                    &landesk_storage::LocalSecrets::beside(&config_path),
                )?;
                ensure!(
                    password.len() == 8 && password.bytes().all(|b| b.is_ascii_alphanumeric()),
                    "Fresh password invalid"
                );
                ensure!(
                    landesk_core::application::password::recover_password(
                        &crate::adapters::identity::read(&config_path)?,
                        &landesk_storage::LocalSecrets::beside(&config_path)
                    )?
                    .as_str()
                        == password.as_str(),
                    "Saved fresh password differs"
                );
                // Removal keeps unrelated files but drops the ownership marker.
                // Reinstall must use the same preparation path as install(),
                // rather than requiring the now-absent marker via ensure_directory.
                ensure!(
                    !paths.app.join("landesk-owner.txt").exists(),
                    "Uninstall left an application ownership marker"
                );
                let mut tx = Transaction::new();
                ensure!(
                    prepare_application(&paths, &mut tx)
                        .context("Prepare application after removing old installation")?
                        .is_none(),
                    "Removed executable unexpectedly required a backup"
                );
                os::owned(&paths.app, &["S-1-5-19", "S-1-5-32-545"])?;
                ensure!(
                    fs::read(paths.app.join("aad-user-file"))? == b"keep unrelated contents"
                        && fs::read(driver.join("existing-driver"))? == b"keep display contents",
                    "Reinstall changed unrelated files or display contents"
                );
                // Prove that real SCM names can be registered again immediately.
                drop(scm::create(&paths.exe, scm::TRANSPORT, true, None)?);
                drop(scm::create(
                    &paths.exe,
                    scm::SERVER,
                    false,
                    Some(scm::TRANSPORT),
                )?);
                Ok(())
            })();
            let cleanup = remove_installation(&paths, false, true, 0, |_| Ok(())).map(|_| ());
            if let Err(error) = cleanup {
                return Err(error).context("Fresh-install native test cleanup failed");
            }
            result?;
        }
        Ok(())
    }
    #[test]
    #[ignore = "Requires Windows administrator; writes only its private temporary directory"]
    fn native_setup_application_preserves_residue_and_restores_previous_exe() -> Result<()> {
        let payload = test_payload()?;
        let app = payload.0.join("app");
        let data = payload.0.join("data");
        os::ensure_directory(&data, os::PRIVATE, &[])?;
        os::create_directory(&app, os::APP)?;
        fs::write(app.join("aad-existing"), b"user contents")?;
        fs::create_dir(app.join("virtual-display-25.7.23"))?;
        let paths = Paths {
            exe: app.join("LanDeskServer.exe"),
            app: app.clone(),
            data,
            transport: PathBuf::new(),
            logs: PathBuf::new(),
        };
        let mut tx = Transaction::new();
        assert!(prepare_application(&paths, &mut tx)?.is_none());
        assert!(paths.exe.exists());
        let _ = tx.rollback(anyhow::anyhow!("injected failure after copying"));
        assert!(!paths.exe.exists() && !app.join("landesk-owner.txt").exists());
        assert_eq!(fs::read(app.join("aad-existing"))?, b"user contents");
        assert!(app.join("virtual-display-25.7.23").is_dir());
        fs::write(app.join("landesk-owner.txt"), os::OWNER)?;
        fs::write(&paths.exe, b"previous executable bytes")?;
        os::protect(
            &paths.exe,
            "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;LS)(A;;0x1200a9;;;BU)",
        )?;
        let mut tx = Transaction::new();
        let backup = prepare_application(&paths, &mut tx)?.context("Expected backup")?;
        assert_eq!(fs::read(&backup)?, b"previous executable bytes");
        assert_ne!(fs::read(&paths.exe)?, b"previous executable bytes");
        let _ = tx.rollback(anyhow::anyhow!("injected health-check failure"));
        assert_eq!(fs::read(&paths.exe)?, b"previous executable bytes");
        assert!(!backup.exists());
        os::owned(&app, &["S-1-5-19", "S-1-5-32-545"])?;
        Ok(())
    }
    #[test]
    #[ignore = "Requires Windows administrator; checks the bundled catalog without installing a device"]
    fn native_setup_signed_driver_payload() -> Result<()> {
        let payload = test_payload()?;
        assert!(payload.driver()?.is_file());
        Ok(())
    }
}
