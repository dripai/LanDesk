use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use objc2::MainThreadMarker;
use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::NSAlert;
use objc2_app_kit::NSAlertFirstButtonReturn;
use objc2_app_kit::NSAlertSecondButtonReturn;
use objc2_app_kit::NSTextField;
use objc2_app_kit::NSView;
use objc2_foundation::NSHomeDirectory;
use objc2_foundation::NSPoint;
use objc2_foundation::NSRect;
use objc2_foundation::NSSize;
use objc2_foundation::NSString;
use std::path::PathBuf;
use zeroize::Zeroizing;

pub fn path() -> PathBuf {
    PathBuf::from(NSHomeDirectory().to_string())
        .join("Library/Application Support/LanDesk/transport/config.json")
}

pub fn prompt(mtm: MainThreadMarker) -> Result<Option<Zeroizing<String>>> {
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
                if let Err(error) = landesk_core::domain::password::validate(&password) {
                    alert.setInformativeText(&NSString::from_str(&error.to_string()));
                    continue;
                }
                field.setStringValue(&NSString::from_str(""));
                return Ok(Some(password));
            }
            result if result == NSAlertSecondButtonReturn => field.setStringValue(
                &NSString::from_str(&landesk_core::domain::password::generate()?),
            ),
            _ => {
                field.setStringValue(&NSString::from_str(""));
                return Ok(None);
            }
        }
    }
}

pub fn prepare() -> Result<landesk_core::domain::identity::ServerIdentity> {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::PermissionsExt;
    let path = path();
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
