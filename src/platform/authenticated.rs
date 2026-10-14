//! Desktop HTTP is reachable only through the authenticated SSH transport.
use super::{Capabilities, DesktopControl};
use crate::protocol::ClientMessage;
use anyhow::{Result, ensure};
use futures_util::future::BoxFuture;
use std::sync::Arc;

pub struct AuthenticatedDesktop {
    inner: Arc<dyn DesktopControl>,
    key: String,
}
impl AuthenticatedDesktop {
    pub fn new(inner: Arc<dyn DesktopControl>, key: String) -> Self {
        Self { inner, key }
    }
}
impl DesktopControl for AuthenticatedDesktop {
    fn requires_access_key(&self) -> bool {
        true
    }
    fn authorize(&self, key: Option<&str>) -> Result<()> {
        let key = key.unwrap_or("");
        ensure!(
            key.len() == self.key.len()
                && !key.is_empty()
                && key
                    .bytes()
                    .zip(self.key.bytes())
                    .fold(0, |difference, (a, b)| difference | (a ^ b))
                    == 0,
            "请通过 LanDeskClient 连接"
        );
        Ok(())
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn supports_files(&self) -> bool {
        self.inner.supports_files()
    }
    fn check_permissions(&self) -> Result<()> {
        self.inner.check_permissions()
    }
    fn secure_attention(&self) -> BoxFuture<'_, Result<()>> {
        self.inner.secure_attention()
    }
    fn begin_input(&self, display_id: u32) -> BoxFuture<'_, Result<()>> {
        self.inner.begin_input(display_id)
    }
    fn input(&self, message: ClientMessage) -> BoxFuture<'_, Result<()>> {
        self.inner.input(message)
    }
    fn paste_text(&self, text: String) -> BoxFuture<'_, Result<()>> {
        self.inner.paste_text(text)
    }
    fn paste_image(&self, png: Vec<u8>) -> BoxFuture<'_, Result<()>> {
        self.inner.paste_image(png)
    }
    fn copy_text(&self) -> BoxFuture<'_, Result<String>> {
        self.inner.copy_text()
    }
    fn read_clipboard(&self) -> BoxFuture<'_, Result<String>> {
        self.inner.read_clipboard()
    }
    fn end_session(&self) -> BoxFuture<'_, Result<()>> {
        self.inner.end_session()
    }
    fn restore(&self) {
        self.inner.restore();
    }
}
