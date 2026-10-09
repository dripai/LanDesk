use crate::protocol::ClientMessage;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::Write;

pub const VERSION: u32 = 1;
pub const MAX_COMMAND: usize = crate::protocol::MAX_TEXT_BYTES * 6 + 1024;
pub const MAX_PACKET: usize = 32 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub action: Action,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Input { message: ClientMessage },
    Paste,
    Resolution { width: u32 },
    Release,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Ready {
        version: u32,
        width: u32,
        height: u32,
    },
    Reply {
        id: u64,
        error: Option<String>,
    },
    Error {
        message: String,
    },
}
pub fn write_packet(out: &mut impl Write, tag: u8, bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= MAX_PACKET, "桌面数据超过限制");
    out.write_all(&(bytes.len() as u32).to_le_bytes())?;
    out.write_all(&[tag])?;
    out.write_all(bytes)?;
    out.flush()?;
    Ok(())
}
pub fn event(out: &mut impl Write, value: Event) -> Result<()> {
    write_packet(out, 0, &serde_json::to_vec(&value)?)
}
pub fn decode_request(bytes: &[u8]) -> Result<Request> {
    ensure!(bytes.len() <= MAX_COMMAND, "桌面命令超过限制");
    Ok(serde_json::from_slice(bytes)?)
}
