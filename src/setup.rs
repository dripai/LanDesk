//! Shared, testable installer contracts. Platform mutations live in windows/setup.
use anyhow::{Context, Result, ensure};
use quick_xml::{Reader, events::Event};
use std::io::{Seek, Write};

type Undo = (&'static str, Box<dyn FnOnce() -> Result<()>>);
pub(crate) struct Transaction {
    undo: Vec<Undo>,
}
impl Transaction {
    pub(crate) fn new() -> Self {
        Self { undo: Vec::new() }
    }
    pub(crate) fn record(
        &mut self,
        label: &'static str,
        action: impl FnOnce() -> Result<()> + 'static,
    ) {
        self.undo.push((label, Box::new(action)));
    }
    /// Continue every independent restoration, reporting all failures instead of masking them.
    pub(crate) fn rollback(mut self, failure: anyhow::Error) -> anyhow::Error {
        let mut errors = vec![format!("{failure:#}")];
        for (label, action) in self.undo.drain(..).rev() {
            if let Err(error) = action() {
                errors.push(format!("回滚 {label} 失败: {error:#}"));
            }
        }
        anyhow::anyhow!(errors.join("；"))
    }
}

pub(crate) fn validate_display_xml(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).context("显示配置不是 UTF-8")?;
    let mut reader = Reader::from_str(text);
    let mut depth = 0usize;
    let mut root = false;
    loop {
        match reader.read_event().context("显示配置 XML 无效")? {
            Event::DocType(_) => anyhow::bail!("显示配置不允许 DTD"),
            Event::Start(element) => {
                if depth == 0 {
                    ensure!(
                        !root && element.name().as_ref() == b"vdd_settings",
                        "显示配置根元素无效"
                    );
                    root = true;
                }
                depth += 1;
            }
            Event::Empty(element) if depth == 0 => {
                ensure!(
                    !root && element.name().as_ref() == b"vdd_settings",
                    "显示配置根元素无效"
                );
                root = true;
            }
            Event::End(_) => {
                ensure!(depth > 0, "显示配置 XML 无效");
                depth -= 1;
            }
            Event::Text(text) if depth == 0 => ensure!(
                text.as_ref().iter().all(u8::is_ascii_whitespace),
                "显示配置根元素外存在内容"
            ),
            Event::Eof => {
                ensure!(root && depth == 0, "显示配置 XML 不完整");
                return Ok(());
            }
            _ => {}
        }
    }
}

pub(crate) fn diagnostic_name(name: &str) -> bool {
    [
        "install",
        "server",
        "transport",
        "worker",
        "uninstall",
        "uninstall-launcher",
    ]
    .iter()
    .any(|role| name == format!("{role}.log") || name == format!("{role}.previous.log"))
}
pub(crate) fn write_archive<W: Write + Seek>(
    output: W,
    entries: &[(String, Vec<u8>)],
) -> Result<W> {
    let mut zip = zip::ZipWriter::new(output);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in entries {
        ensure!(
            diagnostic_name(name) || name == "status.json",
            "不允许导出文件 {name}"
        );
        zip.start_file(name, options)?;
        zip.write_all(bytes)?;
    }
    Ok(zip.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    #[test]
    fn failed_install_restores_every_completed_step_in_reverse_order() {
        for failed_at in 0..8 {
            let state = Rc::new(RefCell::new(vec!["existing config", "existing driver"]));
            let order = Rc::new(RefCell::new(Vec::new()));
            let mut tx = Transaction::new();
            for index in 0..failed_at {
                state.borrow_mut().push("new component");
                let state = state.clone();
                let order = order.clone();
                tx.record("component", move || {
                    state.borrow_mut().pop();
                    order.borrow_mut().push(index);
                    Ok(())
                });
            }
            assert!(
                tx.rollback(anyhow::anyhow!("injected failure"))
                    .to_string()
                    .contains("injected failure")
            );
            assert_eq!(*state.borrow(), vec!["existing config", "existing driver"]);
            assert_eq!(*order.borrow(), (0..failed_at).rev().collect::<Vec<_>>());
        }
    }
    #[test]
    fn rollback_failure_does_not_skip_other_components() {
        let done = Rc::new(RefCell::new(false));
        let check = done.clone();
        let mut tx = Transaction::new();
        tx.record("file", move || {
            *check.borrow_mut() = true;
            Ok(())
        });
        tx.record("service", || anyhow::bail!("delete failed"));
        let error = tx.rollback(anyhow::anyhow!("install failed")).to_string();
        assert!(*done.borrow());
        assert!(error.contains("install failed") && error.contains("delete failed"));
    }
    #[test]
    fn original_display_xml_is_validated_without_rewriting_it() {
        assert!(validate_display_xml(b"<?xml version=\"1.0\"?><vdd_settings><monitors><count>2</count></monitors></vdd_settings>").is_ok());
        assert!(validate_display_xml(b"<vdd_settings/>").is_ok());
        for bad in [
            "",
            "<other/>",
            "<vdd_settings>",
            "<vdd_settings><x></vdd_settings>",
            "<vdd_settings/><vdd_settings/>",
            "<!DOCTYPE vdd_settings><vdd_settings/>",
            "<vdd_settings/>secret",
        ] {
            assert!(validate_display_xml(bad.as_bytes()).is_err(), "{bad}");
        }
    }
    #[test]
    fn diagnostic_zip_roundtrips_and_cannot_include_credentials() {
        let entries = vec![
            ("install.log".into(), "中文错误 1639".as_bytes().to_vec()),
            ("status.json".into(), b"{}".to_vec()),
        ];
        let output = write_archive(std::io::Cursor::new(Vec::new()), &entries).unwrap();
        let mut archive = zip::ZipArchive::new(output).unwrap();
        assert_eq!(archive.len(), 2);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("install.log").unwrap(), &mut text)
            .unwrap();
        assert_eq!(text, "中文错误 1639");
        for name in [
            "config.json",
            "host-key",
            "ui.log",
            "../install.log",
            "screen.jpg",
        ] {
            assert!(
                write_archive(std::io::Cursor::new(Vec::new()), &[(name.into(), vec![])]).is_err()
            );
        }
    }
}
