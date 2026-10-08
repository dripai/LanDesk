use serde::Deserialize;
use subtle::ConstantTimeEq;

pub const PORT: u16 = 17890;
pub const MAX_TEXT_BYTES: usize = 65_536;
pub const UPLOAD_CHUNK_BYTES: usize = 65_536;

pub fn connection_code() -> anyhow::Result<String> {
    const RANGE: u32 = 1_000_000;
    let ceiling = u32::MAX - u32::MAX % RANGE;
    loop {
        let value = getrandom::u32().map_err(|e| anyhow::anyhow!("无法生成连接码: {e}"))?;
        // Reject the incomplete final range so all six-digit codes are equally likely.
        if value < ceiling {
            return Ok(format!("{:06}", value % RANGE));
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello {
        code: String,
    },
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
    ReadClipboard {
        id: u32,
    },
    SetResolution {
        width: Option<u32>,
    },
    ListDirectory {
        id: u32,
        path: String,
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

pub fn code_matches(expected: &str, entered: &str) -> bool {
    let entered = entered.trim();
    valid_code(entered) && bool::from(expected.as_bytes().ct_eq(entered.as_bytes()))
}

pub fn valid_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit())
}

pub fn valid_code_edit(code: &str) -> bool {
    code.len() <= 6 && code.bytes().all(|byte| byte.is_ascii_digit())
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
    fn code_requires_six_digits_and_preserves_leading_zeroes() {
        assert!(code_matches("001234", "001234"));
        assert!(code_matches("001234", " 001234 "));
        for entered in [
            "",
            "1234",
            "001235",
            "00-1234",
            "00 1234",
            "ABCDEF",
            "００１２３４",
        ] {
            assert!(!code_matches("001234", entered));
        }
    }
    #[test]
    fn generated_codes_have_exactly_six_ascii_digits() {
        let code = connection_code().unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
        assert!(code_matches(&code, &code));
    }
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
