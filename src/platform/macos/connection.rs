use anyhow::{Context, Result, ensure};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSTextField, NSView,
};
use objc2_foundation::{NSHomeDirectory, NSPoint, NSRect, NSSize, NSString};
use std::path::PathBuf;
use zeroize::Zeroizing;

pub fn path() -> PathBuf {
    PathBuf::from(NSHomeDirectory().to_string())
        .join("Library/Application Support/LanDesk/transport/config.json")
}

fn prompt(mtm: MainThreadMarker) -> Result<Option<Zeroizing<String>>> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("LanDesk 连接密码"));
    alert.setInformativeText(&NSString::from_str(
        "手工设置或随机生成连接密码。此密码独立于 Mac 登录密码。",
    ));
    alert.addButtonWithTitle(&NSString::from_str("保存"));
    alert.addButtonWithTitle(&NSString::from_str("随机生成"));
    alert.addButtonWithTitle(&NSString::from_str("取消"));
    let view: Retained<NSView> = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 32.0)),
    );
    let field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 28.0)),
    );
    field.setPlaceholderString(Some(&NSString::from_str("新连接密码")));
    view.addSubview(&field);
    alert.setAccessoryView(Some(&view));
    loop {
        match alert.runModal() {
            result if result == NSAlertFirstButtonReturn => {
                let password = Zeroizing::new(field.stringValue().to_string());
                if let Err(error) = crate::connection_password::validate(&password) {
                    alert.setInformativeText(&NSString::from_str(&error.to_string()));
                    continue;
                }
                field.setStringValue(&NSString::from_str(""));
                return Ok(Some(password));
            }
            result if result == NSAlertSecondButtonReturn => {
                field.setStringValue(&NSString::from_str(&crate::connection_password::generate()?))
            }
            _ => {
                field.setStringValue(&NSString::from_str(""));
                return Ok(None);
            }
        }
    }
}

pub fn prepare(mtm: MainThreadMarker) -> Result<crate::transport::Config> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let path = path();
    if !path.exists() {
        let password = prompt(mtm)?.context("连接密码设置已取消")?;
        let directory = path.parent().context("连接配置目录无效")?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        crate::transport::Config::create(&password, crate::transport::DEFAULT_PORT)?
            .write_new(&path)?;
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "连接配置必须为私有普通文件"
    );
    crate::transport::Config::read(&path)
}

pub fn change_password(mtm: MainThreadMarker) -> Result<()> {
    if let Some(password) = prompt(mtm)? {
        crate::transport::Config::change_password(&path(), &password)?;
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("连接密码已保存"));
        alert.setInformativeText(&NSString::from_str(
            "下次连接请使用新密码；当前连接不受影响。",
        ));
        alert.runModal();
    }
    Ok(())
}
