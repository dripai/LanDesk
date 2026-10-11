//! Private CoreGraphics API, isolated from capture/transport/UI.
//! Signatures checked against Chromium's virtual_display_util_mac.mm and the
//! macOS 26.5.1 Objective-C runtime. Apple provides no compatibility guarantee.
use crate::platform::{VirtualDisplay, VirtualDisplayProvider, VirtualDisplaySpec};
use anyhow::{Context, Result, ensure};
use objc2::{
    msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyClass, AnyObject},
    sel,
};
use objc2_foundation::{NSArray, NSPoint, NSSize, NSString};
use std::rc::Rc;

unsafe extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut AnyObject;
}

pub struct MacVirtualDisplay {
    // Release on the owning capture thread; not Send or Sync.
    _object: Retained<AnyObject>,
    id: u32,
}
impl VirtualDisplay for MacVirtualDisplay {
    fn id(&self) -> u32 {
        self.id
    }
}
pub struct MacVirtualDisplayProvider;
impl VirtualDisplayProvider for MacVirtualDisplayProvider {
    fn create(&self, spec: VirtualDisplaySpec) -> Result<Rc<dyn VirtualDisplay>> {
        let spec = spec.validate()?;
        let descriptor_class = AnyClass::get(c"CGVirtualDisplayDescriptor")
            .context("此系统缺少 CGVirtualDisplayDescriptor")?;
        let mode_class =
            AnyClass::get(c"CGVirtualDisplayMode").context("此系统缺少 CGVirtualDisplayMode")?;
        let settings_class = AnyClass::get(c"CGVirtualDisplaySettings")
            .context("此系统缺少 CGVirtualDisplaySettings")?;
        let display_class =
            AnyClass::get(c"CGVirtualDisplay").context("此系统缺少 CGVirtualDisplay")?;
        for (class, selectors) in [
            (
                descriptor_class,
                vec![
                    sel!(setQueue:),
                    sel!(setName:),
                    sel!(setMaxPixelsWide:),
                    sel!(setMaxPixelsHigh:),
                    sel!(setSizeInMillimeters:),
                    sel!(setVendorID:),
                    sel!(setProductID:),
                    sel!(setSerialNum:),
                    sel!(setSerialNumber:),
                    sel!(setWhitePoint:),
                    sel!(setRedPrimary:),
                    sel!(setGreenPrimary:),
                    sel!(setBluePrimary:),
                ],
            ),
            (mode_class, vec![sel!(initWithWidth:height:refreshRate:)]),
            (settings_class, vec![sel!(setHiDPI:), sel!(setModes:)]),
            (
                display_class,
                vec![
                    sel!(initWithDescriptor:),
                    sel!(applySettings:),
                    sel!(displayID),
                ],
            ),
        ] {
            for selector in selectors {
                ensure!(
                    class.instance_method(selector).is_some(),
                    "此系统的虚拟显示接口不完整: {selector}"
                );
            }
        }
        // The serial identifies a display across reopens. Different resolutions
        // can coexist briefly while switching, so include the mode in its ID.
        let serial = spec.width.rotate_left(16) ^ spec.height;
        unsafe {
            let descriptor: Retained<AnyObject> = msg_send![descriptor_class, new];
            let name = NSString::from_str("LanDesk Virtual Display");
            let _: () = msg_send![&descriptor, setQueue: dispatch_get_global_queue(2, 0)];
            let _: () = msg_send![&descriptor, setName: &*name];
            let _: () = msg_send![&descriptor, setMaxPixelsWide: spec.width];
            let _: () = msg_send![&descriptor, setMaxPixelsHigh: spec.height];
            let _: () = msg_send![&descriptor, setSizeInMillimeters: NSSize::new(f64::from(spec.width) * 25.4 / 96.0, f64::from(spec.height) * 25.4 / 96.0)];
            let _: () = msg_send![&descriptor, setVendorID: 0x4c44u32];
            let _: () = msg_send![&descriptor, setProductID: 1u32];
            let _: () = msg_send![&descriptor, setSerialNum: serial];
            let _: () = msg_send![&descriptor, setSerialNumber: serial];
            let _: () = msg_send![&descriptor, setWhitePoint: NSPoint::new(0.3127, 0.3290)];
            let _: () = msg_send![&descriptor, setRedPrimary: NSPoint::new(0.64, 0.33)];
            let _: () = msg_send![&descriptor, setGreenPrimary: NSPoint::new(0.30, 0.60)];
            let _: () = msg_send![&descriptor, setBluePrimary: NSPoint::new(0.15, 0.06)];
            let allocated: Allocated<AnyObject> = msg_send![display_class, alloc];
            let display: Option<Retained<AnyObject>> =
                msg_send![allocated, initWithDescriptor: &*descriptor];
            let display = display.context("macOS 拒绝创建虚拟屏幕")?;
            let allocated: Allocated<AnyObject> = msg_send![mode_class, alloc];
            let mode: Option<Retained<AnyObject>> = msg_send![allocated, initWithWidth: spec.width, height: spec.height, refreshRate: 60.0f64];
            let mode = mode.context("macOS 拒绝虚拟屏幕分辨率")?;
            let settings: Retained<AnyObject> = msg_send![settings_class, new];
            let modes = NSArray::from_slice(&[&*mode]);
            let _: () = msg_send![&settings, setHiDPI: 0u32];
            let _: () = msg_send![&settings, setModes: &*modes];
            let applied: bool = msg_send![&display, applySettings: &*settings];
            ensure!(applied, "macOS 无法应用虚拟屏幕分辨率");
            let id: u32 = msg_send![&display, displayID];
            ensure!(id != 0, "macOS 没有返回虚拟屏幕编号");
            Ok(Rc::new(MacVirtualDisplay {
                _object: display,
                id,
            }))
        }
    }
}
