pub mod desktop;
use serde::Deserialize;

pub use crate::protocol::desktop::INTERNAL_HOST;
pub const MAX_TEXT_BYTES: usize = 65_536;
pub const UPLOAD_CHUNK_BYTES: usize = 65_536;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello {
        #[serde(default)]
        display: Option<crate::domain::DisplayTarget>,
    },
    Heartbeat,
    SecureAttention,
    Pointer {
        x: f64,
        y: f64,
    },
    Button {
        button: u8,
        down: bool,
    },
    Wheel {
        x: i32,
        y: i32,
    },
    Key {
        key: String,
        down: bool,
    },
    Text {
        text: String,
    },
    PasteText {
        text: String,
    },
    CopyClipboard {
        id: u32,
    },
    ReadClipboard {
        id: u32,
    },
    PasteImageChunk {
        id: u32,
        offset: usize,
        total: usize,
        data: String,
    },
    PasteImageCancel {
        id: u32,
    },
    SetResolution {
        width: Option<u32>,
    },
    SetDisplay {
        display_id: u32,
    },
    SetDisplaySource {
        display: crate::domain::DisplayTarget,
    },
    ListDirectory {
        id: u32,
        path: String,
        #[serde(default)]
        cursor: String,
    },
    UploadStart {
        id: u32,
        path: String,
        name: String,
        size: u64,
    },
    UploadFinish {
        id: u32,
    },
    UploadCancel {
        id: u32,
    },
    ReleaseAll,
    Disconnect,
}

impl TryFrom<ClientMessage> for crate::domain::InputEvent {
    type Error = anyhow::Error;
    fn try_from(message: ClientMessage) -> anyhow::Result<Self> {
        Ok(match message {
            ClientMessage::Pointer { x, y } => Self::Pointer { x, y },
            ClientMessage::Button { button, down } => Self::Button { button, down },
            ClientMessage::Wheel { x, y } => Self::Wheel { x, y },
            ClientMessage::Key { key, down } => Self::Key { key, down },
            ClientMessage::Text { text } => Self::Text { text },
            ClientMessage::ReleaseAll => Self::ReleaseAll,
            _ => anyhow::bail!("不是键鼠消息"),
        })
    }
}

impl TryFrom<ClientMessage> for crate::domain::FileCommand {
    type Error = anyhow::Error;
    fn try_from(message: ClientMessage) -> anyhow::Result<Self> {
        Ok(match message {
            ClientMessage::ListDirectory { id, path, cursor } => {
                Self::ListDirectory { id, path, cursor }
            }
            ClientMessage::UploadStart {
                id,
                path,
                name,
                size,
            } => Self::UploadStart {
                id,
                path,
                name,
                size,
            },
            ClientMessage::UploadFinish { id } => Self::UploadFinish { id },
            ClientMessage::UploadCancel { id } => Self::UploadCancel { id },
            _ => anyhow::bail!("不是文件消息"),
        })
    }
}

pub fn display_message(info: crate::domain::CaptureInfo) -> anyhow::Result<serde_json::Value> {
    let mut value = serde_json::to_value(info)?;
    value["type"] = "display_state".into();
    Ok(value)
}

pub fn origin_allowed(origin: &str, host: &str) -> bool {
    host == INTERNAL_HOST && origin == crate::protocol::desktop::INTERNAL_ORIGIN
}

pub fn pointer_position(x: f64, y: f64, width: i32, height: i32) -> anyhow::Result<(i32, i32)> {
    anyhow::ensure!(x.is_finite() && y.is_finite(), "鼠标坐标无效");
    anyhow::ensure!(
        (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y),
        "鼠标坐标越界"
    );
    anyhow::ensure!(width > 0 && height > 0, "显示器尺寸无效");
    Ok((
        (x * f64::from(width - 1)).round() as i32,
        (y * f64::from(height - 1)).round() as i32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_commands_accept_only_their_own_operations() {
        use crate::domain::FileCommand;
        use crate::domain::InputEvent;
        let decode = |text| serde_json::from_str::<ClientMessage>(text).unwrap();
        assert!(
            matches!(InputEvent::try_from(decode(r#"{"type":"key","key":"Control","down":true}"#)).unwrap(), InputEvent::Key { key, down: true } if key == "Control")
        );
        assert!(matches!(
            FileCommand::try_from(decode(r#"{"type":"upload_cancel","id":42}"#)).unwrap(),
            FileCommand::UploadCancel { id: 42 }
        ));
        assert!(InputEvent::try_from(ClientMessage::UploadFinish { id: 1 }).is_err());
        assert!(FileCommand::try_from(ClientMessage::ReleaseAll).is_err());
        assert!(InputEvent::try_from(ClientMessage::Hello { display: None }).is_err());
    }

    #[test]
    fn capture_metadata_keeps_the_browser_wire_shape() {
        use crate::domain::CaptureInfo;
        use crate::domain::DisplayInfo;
        let info = CaptureInfo {
            width: 1280,
            height: 720,
            native_width: 1920,
            native_height: 1080,
            requested_width: Some(1280),
            display_id: 7,
            displays: vec![DisplayInfo {
                id: 7,
                width: 1920,
                height: 1080,
                main: false,
                virtual_display: true,
            }],
        };
        assert_eq!(
            display_message(info).unwrap(),
            serde_json::json!({
                "type":"display_state", "width":1280, "height":720, "native_width":1920, "native_height":1080,
                "requested_width":1280, "display_id":7,
                "displays":[{"id":7,"width":1920,"height":1080,"main":false,"virtual":true}]
            })
        );
    }
    #[test]
    fn origin_blocks_foreign_sites_and_rebinding() {
        assert!(origin_allowed(
            "http://landesk.internal",
            "landesk.internal"
        ));
        assert!(!origin_allowed("http://evil.example", "landesk.internal"));
        assert!(!origin_allowed(
            "http://evil.example:17890",
            "evil.example:17890"
        ));
        assert!(!origin_allowed("null", "landesk.internal"));
    }
    #[test]
    fn pointer_edges_are_inside_display() {
        assert_eq!(pointer_position(1.0, 1.0, 1440, 900).unwrap(), (1439, 899));
        assert!(pointer_position(f64::NAN, 0.5, 1440, 900).is_err());
        assert!(pointer_position(-0.1, 0.5, 1440, 900).is_err());
    }
    #[test]
    fn malformed_and_extra_fields_are_rejected() {
        assert!(
            serde_json::from_str::<ClientMessage>(
                r#"{"type":"privacy","enabled":true,"admin":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<ClientMessage>(r#"{"type":"execute","command":"whoami"}"#)
                .is_err()
        );
    }
}
