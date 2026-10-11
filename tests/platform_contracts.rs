use anyhow::{Result, bail};
use landesk::{
    capture::{CaptureService, Change, FrameEvent},
    platform::{
        CaptureBackend, DisplayTarget, FrameSource, VirtualDisplay, VirtualDisplayProvider,
        VirtualDisplaySpec, VirtualDisplays,
    },
};
use std::{cell::Cell, rc::Rc, sync::Arc};
use tokio::sync::watch;

struct Display {
    id: u32,
    destroyed: Rc<Cell<u32>>,
}
impl VirtualDisplay for Display {
    fn id(&self) -> u32 {
        self.id
    }
}
impl Drop for Display {
    fn drop(&mut self) {
        self.destroyed.set(self.destroyed.get() + 1);
    }
}
struct Provider {
    count: Rc<Cell<u32>>,
    destroyed: Rc<Cell<u32>>,
}
impl VirtualDisplayProvider for Provider {
    fn create(&self, spec: VirtualDisplaySpec) -> Result<Rc<dyn VirtualDisplay>> {
        if spec.width == 13 {
            bail!("simulated OS failure");
        }
        self.count.set(self.count.get() + 1);
        Ok(Rc::new(Display {
            id: self.count.get(),
            destroyed: self.destroyed.clone(),
        }))
    }
}

#[test]
fn virtual_display_transaction_preserves_last_working_lease() {
    let count = Rc::new(Cell::new(0));
    let destroyed = Rc::new(Cell::new(0));
    let mut displays = VirtualDisplays::new(Provider {
        count: count.clone(),
        destroyed: destroyed.clone(),
    });
    let first_spec = VirtualDisplaySpec {
        width: 1920,
        height: 1080,
    };
    let first = displays.prepare(first_spec).unwrap();
    displays.commit(first_spec, first.clone());
    assert_eq!(displays.prepare(first_spec).unwrap().id(), first.id());
    assert_eq!(count.get(), 1, "refresh must not create another display");
    assert!(
        displays
            .prepare(VirtualDisplaySpec {
                width: 13,
                height: 100
            })
            .is_err()
    );
    assert_eq!(displays.prepare(first_spec).unwrap().id(), first.id());
    let candidate = displays
        .prepare(VirtualDisplaySpec {
            width: 1280,
            height: 720,
        })
        .unwrap();
    drop(candidate); // Capture failed: do not commit a display with no image.
    assert_eq!(destroyed.get(), 1);
    assert_eq!(displays.prepare(first_spec).unwrap().id(), first.id());
    drop(displays);
    assert_eq!(destroyed.get(), 1, "capture still holds its lease");
    drop(first);
    assert_eq!(destroyed.get(), 2);
}

struct Source {
    id: u32,
    rx: watch::Receiver<Option<FrameEvent>>,
    _tx: watch::Sender<Option<FrameEvent>>,
}
impl FrameSource for Source {
    fn frames(&self) -> &watch::Receiver<Option<FrameEvent>> {
        &self.rx
    }
    fn frames_mut(&mut self) -> &mut watch::Receiver<Option<FrameEvent>> {
        &mut self.rx
    }
    fn info(&self, _: Option<u32>) -> serde_json::Value {
        serde_json::json!({"display_id":self.id,"width":1920,"height":1080})
    }
    fn display_id(&self) -> u32 {
        self.id
    }
    fn native_width(&self) -> u32 {
        1920
    }
    fn geometry(&self) -> [f64; 4] {
        [0.0, 0.0, 1920.0, 1080.0]
    }
    fn set_resolution(&mut self, _: Option<u32>) -> Result<()> {
        Ok(())
    }
}
struct Backend {
    selected: u32,
}
impl CaptureBackend for Backend {
    fn open(&mut self, target: Option<DisplayTarget>) -> Result<Box<dyn FrameSource>> {
        let selected = match target {
            Some(DisplayTarget::Existing { id }) => id.unwrap_or(1),
            Some(DisplayTarget::Virtual { width: 13, .. }) => bail!("simulated capture failure"),
            Some(DisplayTarget::Virtual { .. }) => 42,
            None => self.selected,
        };
        let (tx, rx) = watch::channel(None);
        tx.send_replace(Some(FrameEvent::Frame {
            jpeg: Arc::new(vec![1, 2, 3]),
            sequence: 1,
        }));
        self.selected = selected;
        Ok(Box::new(Source {
            id: selected,
            rx,
            _tx: tx,
        }))
    }
}

#[tokio::test]
async fn capture_switch_failure_preserves_session_and_reconnect_uses_selected_source() {
    let service = CaptureService::new(|| Box::new(Backend { selected: 1 })).unwrap();
    let mut session = service.start(None).await.unwrap();
    session
        .frames
        .wait_for(|frame| frame.is_some())
        .await
        .unwrap();
    assert_eq!(session.info.borrow()["display_id"], 1);
    assert!(
        session
            .configure(Change::Source(DisplayTarget::Virtual {
                width: 13,
                height: 10
            }))
            .await
            .is_err()
    );
    assert_eq!(session.info.borrow()["display_id"], 1);
    let info = session
        .configure(Change::Source(DisplayTarget::Virtual {
            width: 1920,
            height: 1080,
        }))
        .await
        .unwrap();
    assert_eq!(info["display_id"], 42);
    drop(session);
    let next = service.start(None).await.unwrap();
    assert_eq!(next.info.borrow()["display_id"], 42);
}

#[test]
fn protocol_accepts_virtual_source_before_a_desktop_exists() {
    let message: landesk::protocol::ClientMessage = serde_json::from_str(
        r#"{"type":"hello","display":{"kind":"virtual","width":1920,"height":1080}}"#,
    )
    .unwrap();
    assert!(matches!(
        message,
        landesk::protocol::ClientMessage::Hello {
            display: Some(DisplayTarget::Virtual {
                width: 1920,
                height: 1080
            })
        }
    ));
    assert!(
        VirtualDisplaySpec {
            width: u32::MAX,
            height: u32::MAX
        }
        .validate()
        .is_err()
    );
    assert!(
        VirtualDisplaySpec {
            width: 0,
            height: 1080
        }
        .validate()
        .is_err()
    );
}
