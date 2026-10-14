//! Integrated native setup. There are no PowerShell, cmd, sc, or external script invocations.
use super::{
    desktop::{Handle, checked, wide},
    setup_dialog as dialog, setup_services as scm, setup_support as os, virtual_display as display,
};
use crate::setup::Transaction;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{SystemInformation::*, Threading::*},
    UI::Shell::IsUserAnAdmin,
};

const RESOURCES: &[(&str, &[u8])] = &[
    (
        "MttVDD.dll",
        include_bytes!("../../../vendor/virtual-display/MttVDD.dll"),
    ),
    (
        "MttVDD.inf",
        include_bytes!("../../../vendor/virtual-display/MttVDD.inf"),
    ),
    (
        "mttvdd.cat",
        include_bytes!("../../../vendor/virtual-display/mttvdd.cat"),
    ),
    (
        "vdd_settings.xml",
        include_bytes!("../../../vendor/virtual-display/vdd_settings.xml"),
    ),
    (
        "LICENSE",
        include_bytes!("../../../vendor/virtual-display/LICENSE"),
    ),
];
fn unique() -> Result<String> {
    let mut value = [0u8; 16];
    getrandom::fill(&mut value).map_err(|e| anyhow::anyhow!("安装随机数失败: {e}"))?;
    Ok(value.iter().map(|byte| format!("{byte:02x}")).collect())
}
struct Lock(Handle);
impl Lock {
    fn new() -> Result<Self> {
        ensure!(unsafe { IsUserAnAdmin() } != 0, "安装或卸载需要管理员权限");
        let handle = Handle::new(
            unsafe { CreateMutexW(null(), 0, wide(r"Global\LanDeskServerSetup").as_ptr()) },
            "创建安装互斥锁",
        )?;
        let wait = unsafe { WaitForSingleObject(handle.0, 0) };
        ensure!(
            wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED,
            "另一个 LanDesk 安装或卸载正在运行"
        );
        Ok(Self(handle))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0.0);
        }
    }
}

pub(super) fn change_password() -> Result<()> {
    let _lock = Lock::new()?;
    let directory = os::environment("ProgramData")?.join("LanDeskServer/transport");
    os::ensure_directory(&directory, os::TRANSPORT_ACL, &["S-1-5-19"])?;
    let path = directory.join("config.json");
    os::plain(&path)?;
    let mut password = dialog::password(None)?;
    if password.is_empty() {
        password = crate::connection_password::generate()?;
        let _ = dialog::password(Some(&password))?;
    }
    crate::transport::Config::change_password(&path, &password)?;
    crate::diagnostics::log("Connection password changed; existing sessions retained");
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            null_mut(),
            wide("连接密码已保存。下次连接请使用新密码；当前连接不受影响。").as_ptr(),
            wide("LanDeskServer").as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK,
        );
    }
    Ok(())
}
struct Payload(PathBuf);
impl Payload {
    fn new() -> Result<Self> {
        let path = os::environment("ProgramFiles")?.join(format!(".LanDeskSetup-{}", unique()?));
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
struct Paths {
    app: PathBuf,
    exe: PathBuf,
    data: PathBuf,
    transport: PathBuf,
    logs: PathBuf,
}
impl Paths {
    fn new() -> Result<Self> {
        let app = os::environment("ProgramFiles")?.join("LanDeskServer");
        let data = os::environment("ProgramData")?.join("LanDeskServer");
        Ok(Self {
            exe: app.join("LanDeskServer.exe"),
            app,
            transport: data.join("transport"),
            logs: data.join("logs"),
            data,
        })
    }
    fn logs(&self, role: &str) -> Result<()> {
        os::ensure_directory(&self.data, os::PRIVATE, &[])?;
        os::ensure_directory(&self.logs, os::LOGS, &["S-1-5-32-545"])?;
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
    fn transport(&self, existing: bool) -> Result<crate::transport::Config> {
        os::ensure_directory(&self.transport, os::TRANSPORT_ACL, &["S-1-5-19"])?;
        let path = self.transport.join("config.json");
        if !path.exists() {
            ensure!(!existing, "已安装服务缺少连接配置，未生成新身份");
            let mut password = dialog::password(None)?;
            if password.is_empty() {
                password = crate::connection_password::generate()?;
                let _ = dialog::password(Some(&password))?;
            }
            crate::transport::Config::create(&password, 2222)?.write_new(&path)?;
        }
        os::plain(&path)?;
        let config = crate::transport::Config::read(&path)?;
        for name in ["transport.log", "transport.previous.log"] {
            let file = self.logs.join(name);
            if !file.exists() {
                fs::write(&file, [])?;
            }
            os::protect(
                &file,
                "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGW;;;LS)(A;;GR;;;BU)",
            )?;
        }
        Ok(config)
    }
}
fn desktop_ready(config: &crate::transport::Config) -> Result<()> {
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
    let registry = r"SOFTWARE\MikeTheTech\VirtualDisplayDriver";
    let config = match os::read(registry, "VDDPATH")? {
        Some(value) => {
            let path = PathBuf::from(os::decode(&value)?);
            ensure!(path.is_absolute(), "MttVDD VDDPATH 不是绝对路径");
            path
        }
        None => PathBuf::from(r"C:\VirtualDisplayDriver"),
    };
    os::plain_chain(&config)?;
    if config.exists() {
        let settings = config.join("vdd_settings.xml");
        os::plain(&settings)?;
        crate::setup::validate_display_xml(&fs::read(settings)?)?;
        crate::diagnostics::log(&format!("保留现有 MttVDD 配置: {}", config.display()));
    } else {
        os::create_directory(
            &config,
            "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;AU)",
        )?;
        let rollback = config.clone();
        transaction.record("显示配置", move || {
            // Keep configuration if an earlier device rollback failed.
            ensure!(
                display::status()?["installed"] != true,
                "设备未移除，显示配置已保留"
            );
            for name in ["vdd_settings.xml", "landesk-owner.txt"] {
                let file = rollback.join(name);
                if file.exists() {
                    fs::remove_file(file)?;
                }
            }
            if fs::read_dir(&rollback)?.next().is_none() {
                fs::remove_dir(rollback)?;
            }
            Ok(())
        });
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
fn update(
    paths: &Paths,
    progress: &dialog::Progress,
    config: &crate::transport::Config,
) -> Result<()> {
    os::owned(&paths.app, &["S-1-5-19", "S-1-5-32-545"])?;
    scm::assert_owned(&paths.exe)?;
    let server = scm::open(scm::SERVER)?.context("安装不完整: LanDeskServer 缺失")?;
    let transport = scm::open(scm::TRANSPORT)?.context("安装不完整: LanDeskTransport 缺失")?;
    let was_server = scm::status(&server)?.dwCurrentState
        != windows_sys::Win32::System::Services::SERVICE_STOPPED;
    let was_transport = scm::status(&transport)?.dwCurrentState
        != windows_sys::Win32::System::Services::SERVICE_STOPPED;
    drop(server);
    drop(transport);
    progress.stage("更新后台程序，保留密码、设备和启动设置…")?;
    let snapshot = os::uninstall_snapshot(&paths.app)?;
    let mut tx = Transaction::new();
    let exe = paths.exe.clone();
    tx.record("原服务运行状态", move || {
        scm::stop(scm::SERVER)?;
        scm::stop(scm::TRANSPORT)?;
        if was_server {
            scm::start(scm::SERVER)?;
        } else if was_transport {
            scm::start(scm::TRANSPORT)?;
        }
        Ok(())
    });
    let result = (|| -> Result<Option<PathBuf>> {
        scm::stop(scm::SERVER)?;
        scm::stop(scm::TRANSPORT)?;
        os::close_installed_processes(&exe)?;
        let backup = prepare_application(paths, &mut tx)?;
        // A later failed health check must stop the new binary before restoring the old file.
        tx.record("停止更新后服务", || {
            scm::stop(scm::SERVER)?;
            scm::stop(scm::TRANSPORT)
        });
        if was_server {
            scm::start(scm::SERVER)?;
            desktop_ready(config)?;
        } else if was_transport {
            scm::start(scm::TRANSPORT)?;
        }
        tx.record("卸载入口", move || snapshot.restore());
        os::register_uninstall(&paths.app)?;
        Ok(backup)
    })();
    match result {
        Ok(backup) => {
            if let Some(backup) = backup {
                fs::remove_file(backup).context("更新成功，旧程序备份清理失败")?;
            }
            Ok(())
        }
        Err(error) => Err(tx.rollback(error)),
    }
}
pub(super) fn install() -> Result<()> {
    let _lock = Lock::new()?;
    let paths = Paths::new()?;
    paths.logs("install")?;
    let result = (|| {
        let _com = os::Com::new()?;
        let progress = dialog::Progress::new("正在安装 LanDeskServer")?;
        progress.stage("配置加密连接…")?;
        let existing = scm::open(scm::SERVER)?.is_some() || scm::open(scm::TRANSPORT)?.is_some();
        scm::assert_owned(&paths.exe)?;
        let config = paths.transport(existing)?;
        if existing {
            update(&paths, &progress, &config)?;
            progress.stage("更新完成")?;
            return Ok(());
        }
        let payload = Payload::new()?;
        let shortcut = os::shortcut_path()?;
        ensure!(
            os::firewall(&paths.exe)?.is_none(),
            "LanDesk 防火墙规则已存在，请检查安装状态"
        );
        ensure!(!shortcut.exists(), "LanDesk 快捷方式已存在，请检查安装状态");
        ensure!(
            os::read(os::RUN, scm::SERVER)?.is_none(),
            "LanDesk 启动入口已存在，请检查安装状态"
        );
        let uninstall = os::uninstall_snapshot(&paths.app)?;
        let policy = os::read(os::POLICY, os::SAS)?;
        if let Some(value) = &policy {
            ensure!(
                *value == os::number(1) || *value == os::number(3),
                "已有策略禁止服务生成 Ctrl+Alt+Delete"
            );
        }
        let mut tx = Transaction::new();
        let result = (|| -> Result<(bool, Option<PathBuf>)> {
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
            let options = os::RegistrySnapshot::read(os::OPTIONS, &["MinimizeOnStart"])?;
            tx.record("启动入口", move || run.restore());
            tx.record("托盘设置", move || options.restore());
            if os::read(os::OPTIONS, "MinimizeOnStart")?.is_none() {
                os::put(os::OPTIONS, "MinimizeOnStart", Some(&os::number(1)))?;
            }
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
                if reboot {
                    dialog::complete(
                        "安装完成。Windows 要求重启以启用虚拟显示驱动，请重启后连接。",
                    );
                }
                Ok(())
            }
            Err(error) => Err(tx.rollback(error)),
        }
    })();
    if let Err(error) = &result {
        crate::diagnostics::log(&format!("Native installation FAILED: {error:#}"));
    }
    result
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
            directory.parent() == Some(os::environment("ProgramFiles")?.as_path())
                && name
                    .strip_prefix(".LanDeskSetup-")
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
        let _lock = Lock::new()?;
        let paths = Paths::new()?;
        paths.logs("uninstall")?;
        let Some(remove_display) = dialog::uninstall_choice()? else {
            crate::diagnostics::log("卸载已取消");
            return Ok(());
        };
        let _com = os::Com::new()?;
        let progress = dialog::Progress::new("正在卸载 LanDeskServer")?;
        os::owned(&paths.app, &["S-1-5-19", "S-1-5-32-545"])?;
        scm::assert_owned(&paths.exe)?;
        let _ = os::uninstall_snapshot(&paths.app)?;
        let firewall = os::firewall(&paths.exe)?.is_some();
        let shortcut = os::shortcut_path()?;
        if shortcut.exists() {
            os::shortcut_owned(&shortcut, &paths.exe)?;
        }
        let startup = os::read(os::RUN, scm::SERVER)?;
        let expected_startup = os::text(&format!("\"{}\" --startup-ui", paths.exe.display()));
        ensure!(
            startup.is_none() || startup == Some(expected_startup),
            "启动入口不属于 LanDesk"
        );
        progress.stage("停止后台服务及管理界面…")?;
        scm::stop(scm::SERVER)?;
        scm::stop(scm::TRANSPORT)?;
        os::close_installed_processes(&paths.exe)?;
        progress.stage("移除服务、开机入口与防火墙规则…")?;
        scm::delete(scm::SERVER)?;
        scm::delete(scm::TRANSPORT)?;
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
            progress.stage("移除 LanDesk 自建虚拟显示设备…")?;
            let result = display::uninstall()?;
            reboot = result["reboot_required"] == true;
            crate::diagnostics::log(&format!("Display uninstall: {result}"));
        }
        progress.stage("移除程序及应用列表入口…")?;
        fs::remove_file(&paths.exe)?;
        fs::remove_file(paths.app.join("landesk-owner.txt"))?;
        if fs::read_dir(&paths.app)?.next().is_none() {
            fs::remove_dir(&paths.app)?;
        }
        os::delete_key(os::UNINSTALL)?;
        progress.stage("卸载完成，密码配置、日志和其他文件已保留")?;
        dialog::complete(if reboot {
            "卸载完成。配置和日志已保留。Windows 要求重启以完成显示设备移除。"
        } else {
            "卸载完成。配置和日志已保留。"
        });
        Ok(())
    })();
    if let Err(error) = &result {
        crate::diagnostics::log(&format!(
            "Native uninstall FAILED (some components may already be removed): {error:#}"
        ));
        super::ui::error_box(
            null_mut(),
            format!("卸载未完成：{error:#}。部分组件可能已移除，请保留日志。"),
        );
    }
    // The EXE itself stays queued for deletion at the next reboot; no external cleanup script.
    result
}
pub(super) fn export() -> Result<()> {
    let _lock = Lock::new()?;
    let paths = Paths::new()?;
    os::owned(&paths.data, &[])?;
    os::owned(&paths.logs, &["S-1-5-32-545"])?;
    let exports = paths.logs.join("exports");
    if !exports.exists() {
        os::create_directory(&exports, os::LOGS)?;
    } else {
        os::plain_chain(&exports)?;
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&paths.logs)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if crate::setup::diagnostic_name(&name) {
            os::plain(&entry.path())?;
            entries.push((name, fs::read(entry.path())?));
        }
    }
    let mut windows = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    ensure!(
        unsafe { windows_sys::Wdk::System::SystemServices::RtlGetVersion(&mut windows) } == 0,
        "读取 Windows 版本失败"
    );
    let status = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"windows":{"major":windows.dwMajorVersion,"minor":windows.dwMinorVersion,"build":windows.dwBuildNumber},"services":scm::report().unwrap_or_else(|error|serde_json::json!({"error":format!("{error:#}")})),"display":display::status().unwrap_or_else(|error|serde_json::json!({"error":format!("{error:#}")})),"interactive":super::interactive::report()});
    entries.push(("status.json".into(), serde_json::to_vec_pretty(&status)?));
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
    crate::diagnostics::log(&format!("日志已导出: {}", path.display()));
    Ok(())
}

pub(super) fn status() -> Result<serde_json::Value> {
    Ok(
        serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"services":scm::report()?,"display":display::status()?,"interactive":super::interactive::report()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires Windows administrator; writes only its private temporary directory"]
    fn native_setup_application_preserves_residue_and_restores_previous_exe() -> Result<()> {
        let payload = Payload::new()?;
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
        let payload = Payload::new()?;
        assert!(payload.driver()?.is_file());
        Ok(())
    }
}
