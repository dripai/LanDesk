use serde::Deserialize;

pub const PORT: u16 = 17890;
pub const MAX_TEXT_BYTES: usize = 65_536;
pub const UPLOAD_CHUNK_BYTES: usize = 65_536;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello {},
    Heartbeat,
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

pub fn origin_allowed(origin: &str, host: &str) -> bool {
    [format!("127.0.0.1:{PORT}"), format!("localhost:{PORT}")]
        .iter()
        .any(|allowed| host == allowed && origin == format!("http://{allowed}"))
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
    fn origin_blocks_foreign_sites_and_rebinding() {
        assert!(origin_allowed("http://127.0.0.1:17890", "127.0.0.1:17890"));
        assert!(!origin_allowed("http://evil.example", "127.0.0.1:17890"));
        assert!(!origin_allowed(
            "http://evil.example:17890",
            "evil.example:17890"
        ));
        assert!(!origin_allowed("null", "127.0.0.1:17890"));
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
