use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, sel,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes, NSTimer};

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Box<dyn Fn()>]
    struct Tick;
    unsafe impl NSObjectProtocol for Tick {}
    impl Tick {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            (self.ivars())();
        }
    }
);

// Common modes include AppKit mouse tracking and modal panels. Draining input
// only outside sendEvent deadlocks when a control waits for remote mouse-up.
pub struct RunLoopTimer(Retained<NSTimer>);
impl RunLoopTimer {
    pub fn new(mtm: MainThreadMarker, callback: impl Fn() + 'static) -> Self {
        let target: Retained<Tick> = unsafe {
            let target = Tick::alloc(mtm).set_ivars(Box::new(callback));
            msg_send![super(target), init]
        };
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                0.01,
                &target,
                sel!(tick:),
                None,
                true,
            )
        };
        unsafe {
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        Self(timer)
    }
}
impl Drop for RunLoopTimer {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}
