#![cfg(target_os = "macos")]
use landesk::platform::macos::virtual_display::MacVirtualDisplayProvider;
use landesk::platform::{VirtualDisplaySpec, VirtualDisplays};
use std::time::{Duration, Instant};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetOnlineDisplayList(capacity: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGMainDisplayID() -> u32;
}

fn online(id: u32) -> bool {
    let mut displays = [0u32; 128];
    let mut count = 0;
    assert_eq!(
        unsafe { CGGetOnlineDisplayList(displays.len() as u32, displays.as_mut_ptr(), &mut count) },
        0
    );
    displays[..count as usize].contains(&id)
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "display registration/removal did not complete"
        );
        objc2_foundation::NSRunLoop::mainRunLoop().runMode_beforeDate(
            unsafe { objc2_foundation::NSDefaultRunLoopMode },
            &objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.02),
        );
    }
}

fn main() {
    if std::env::var("LANDESK_TEST_VIRTUAL_DISPLAY").as_deref() != Ok("1") {
        println!(
            "virtual display hardware test skipped; set LANDESK_TEST_VIRTUAL_DISPLAY=1 to run"
        );
        return;
    }
    let mtm = objc2::MainThreadMarker::new().expect("hardware test requires the main thread");
    let _app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let id = objc2::rc::autoreleasepool(|_| {
        let main = unsafe { CGMainDisplayID() };
        let mut displays = VirtualDisplays::new(MacVirtualDisplayProvider);
        let spec = VirtualDisplaySpec {
            width: 1920,
            height: 1080,
        };
        let first = displays.prepare(spec).unwrap();
        let id = first.id();
        println!("created display {id}, waiting for registration");
        wait_for(|| online(id));
        assert_eq!(unsafe { CGDisplayPixelsWide(id) }, 1920);
        assert_eq!(unsafe { CGDisplayPixelsHigh(id) }, 1080);
        assert_eq!(
            unsafe { CGMainDisplayID() },
            main,
            "must not replace the current primary display"
        );
        displays.commit(spec, first.clone());
        let second = displays.prepare(spec).unwrap();
        assert_eq!(second.id(), id, "reconnect must reuse the display");
        drop(first);
        drop(second);
        assert!(online(id), "manager owns the display between sessions");
        drop(displays);
        id
    });
    println!("display {id} validated, waiting for removal after draining autorelease pool");
    wait_for(|| !online(id));
    println!("virtual display {id}: 1920x1080, reconnect reuse, primary unchanged, removal passed");
    if std::env::var("LANDESK_TEST_VIRTUAL_CAPTURE").as_deref() == Ok("1") {
        use landesk::platform::{CaptureBackend, DisplayTarget};
        assert!(
            landesk::platform::macos::native::permissions().0,
            "test executable has no Screen Recording permission; capture not tested"
        );
        objc2::rc::autoreleasepool(|_| {
            let mut backend = landesk::platform::macos::capture::MacCaptureBackend::default();
            let capture = backend
                .open(Some(DisplayTarget::Virtual {
                    width: 1920,
                    height: 1080,
                }))
                .unwrap();
            wait_for(|| capture.frames().borrow().is_some());
            let frame = capture.frames().borrow().clone().unwrap();
            match frame {
                landesk::capture::FrameEvent::Frame { jpeg, .. } => {
                    let image = image::load_from_memory(&jpeg).unwrap();
                    assert_eq!((image.width(), image.height()), (1920, 1080));
                    println!("ScreenCaptureKit virtual display: decoded 1920x1080 JPEG frame");
                }
                landesk::capture::FrameEvent::Error(error) => panic!("capture failed: {error}"),
            }
            let original_id = capture.display_id();
            let replacement = backend
                .open(Some(DisplayTarget::Virtual {
                    width: 1280,
                    height: 720,
                }))
                .unwrap();
            assert_ne!(replacement.display_id(), original_id);
            assert!(
                replacement.info(None)["displays"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|display| display["id"] != original_id)
            );
            drop(capture);
            wait_for(|| replacement.frames().borrow().is_some());
            match replacement.frames().borrow().clone().unwrap() {
                landesk::capture::FrameEvent::Frame { jpeg, .. } => {
                    let image = image::load_from_memory(&jpeg).unwrap();
                    assert_eq!((image.width(), image.height()), (1280, 720));
                }
                landesk::capture::FrameEvent::Error(error) => panic!("replacement failed: {error}"),
            }
            let replacement_id = replacement.display_id();
            drop(replacement);
            let reconnected = backend.open(None).unwrap();
            assert_eq!(reconnected.display_id(), replacement_id);
            println!(
                "virtual capture: 1280x720 replacement, stale display omitted, reconnect reused display"
            );
        });
    }
}
