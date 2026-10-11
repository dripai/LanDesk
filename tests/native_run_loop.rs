// A standalone test harness runs on the real process main thread, as required
// by AppKit. It does not create windows or synthesize keyboard/mouse input.
#[path = "../src/run_loop.rs"]
mod run_loop;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEventTrackingRunLoopMode, NSModalPanelRunLoopMode};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};
use std::{
    cell::Cell,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

fn main() {
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
