#[cfg(target_os = "macos")]
mod native {
    // A standalone test harness runs on the real process main thread, as required
    // by AppKit. It does not create windows or synthesize keyboard/mouse input.
    use landesk::platform::macos::run_loop;

    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_app_kit::NSEventTrackingRunLoopMode;
    use objc2_app_kit::NSModalPanelRunLoopMode;
    use objc2_foundation::NSDate;
    use objc2_foundation::NSDefaultRunLoopMode;
    use objc2_foundation::NSRunLoop;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::mpsc;
    use std::time::Duration;
    use std::time::Instant;

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("test must run on main thread");
        let _app = NSApplication::sharedApplication(mtm);
        let (tx, rx) = mpsc::channel();
        let processed = Rc::new(Cell::new(0));
        let count = processed.clone();
        let timer = run_loop::RunLoopTimer::new(mtm, move || {
            while let Ok(()) = rx.try_recv() {
                count.set(count.get() + 1);
            }
        });
        for (index, mode) in unsafe {
            [
                NSDefaultRunLoopMode,
                NSEventTrackingRunLoopMode,
                NSModalPanelRunLoopMode,
            ]
        }
        .into_iter()
        .enumerate()
        {
            tx.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while processed.get() == index && Instant::now() < deadline {
                NSRunLoop::mainRunLoop()
                    .runMode_beforeDate(mode, &NSDate::dateWithTimeIntervalSinceNow(0.02));
            }
            assert_eq!(
                processed.get(),
                index + 1,
                "command queue stalled in {mode}"
            );
        }
        drop(timer);
        tx.send(())
            .expect_err("dropping timer must release its callback and command receiver");
        println!("native run-loop: default, mouse tracking, modal and cleanup passed");
    }
}
#[cfg(target_os = "macos")]
fn main() {
    native::run()
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS native harness: not applicable to this target");
}
