//! AppKit tray appearance and Quit recovery (including Command-Q and the Dock).

use std::{cell::RefCell, ffi::CStr};

use objc2::{
    MainThreadMarker, class, ffi, msg_send,
    runtime::{AnyObject, Imp, Sel},
    sel,
};

thread_local! {
    static QUIT_HANDLER: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
}

/// Read on the UI thread; the appearance timer also picks up changes made while
/// SwitchX is open, without changing the user's animation preference.
pub fn prefers_reduced_motion() -> bool {
    let Some(_main_thread) = MainThreadMarker::new() else {
        return false;
    };
    // SAFETY: NSWorkspace's shared instance is valid for the process lifetime,
    // and this accessor has been available since macOS 10.12.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        msg_send![workspace, accessibilityDisplayShouldReduceMotion]
    }
}

unsafe extern "C-unwind" fn should_terminate(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _application: *mut AnyObject,
) -> usize {
    QUIT_HANDLER.with(|handler| {
        if let Some(handler) = handler.borrow().as_ref() {
            handler();
        }
    });
    // NSTerminateCancel: our worker ends the event loop after successful recovery.
    0
}

pub fn install_quit_handler(handler: impl Fn() + 'static) -> Result<(), String> {
    // SAFETY: called on the main thread after Slint creates its AppKit backend.
    // Preserve Winit's delegate and add its currently absent optional termination
    // method. Refuse an existing implementation instead of replacing its behavior.
    unsafe {
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![application, delegate];
        let delegate = delegate
            .as_ref()
            .ok_or("macOS application delegate is unavailable")?;
        let class = delegate.class();
        let selector = sel!(applicationShouldTerminate:);
        if class.instance_method(selector).is_some() {
            return Err("macOS application already has a termination handler".into());
        }
        let implementation: Imp = std::mem::transmute(
            should_terminate
                as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize,
        );
        // NSApplicationTerminateReply is NSUInteger on the supported 64-bit Macs.
        if !ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            selector,
            implementation,
            c"Q@:@".as_ptr(),
        )
        .as_bool()
        {
            return Err("could not install macOS termination handler".into());
        }
    }
    QUIT_HANDLER.with(|slot| *slot.borrow_mut() = Some(Box::new(handler)));
    Ok(())
}

pub fn clear_quit_handler() {
    QUIT_HANDLER.with(|slot| *slot.borrow_mut() = None);
}

/// Slint 1.18.1 does not expose NSImage's template flag or a native tray handle.
/// Find only our status button through AppKit's public window/view APIs.
pub fn use_template_tray_icon(tooltip: &str) -> Result<(), String> {
    let _main_thread = MainThreadMarker::new().ok_or("tray icon requires the main thread")?;
    // SAFETY: AppKit owns these live objects; all access stays on the main thread.
    unsafe {
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut AnyObject = msg_send![application, windows];
        let count: usize = msg_send![windows, count];
        for index in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: index];
            let view: *mut AnyObject = msg_send![window, contentView];
            if let Some(button) = find_status_button(view, tooltip) {
                let image: *mut AnyObject = msg_send![button, image];
                if image.is_null() {
                    return Err("macOS tray icon has no image".into());
                }
                let () = msg_send![image, setTemplate: true];
                let () = msg_send![button, setNeedsDisplay: true];
                let is_template: bool = msg_send![image, isTemplate];
                return if is_template {
                    Ok(())
                } else {
                    Err("could not enable macOS tray template image".into())
                };
            }
        }
    }
    Err("macOS tray icon is unavailable".into())
}

unsafe fn find_status_button(view: *mut AnyObject, tooltip: &str) -> Option<*mut AnyObject> {
    if view.is_null() {
        return None;
    }
    // SAFETY: the caller supplies a live NSView from AppKit on the main thread.
    unsafe {
        let is_status_button: bool = msg_send![view, isKindOfClass: class!(NSStatusBarButton)];
        if is_status_button {
            let title: *mut AnyObject = msg_send![view, toolTip];
            if !title.is_null() {
                let title: *const std::ffi::c_char = msg_send![title, UTF8String];
                if !title.is_null() && CStr::from_ptr(title).to_bytes() == tooltip.as_bytes() {
                    return Some(view);
                }
            }
        }
        let subviews: *mut AnyObject = msg_send![view, subviews];
        let count: usize = msg_send![subviews, count];
        for index in 0..count {
            let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
            if let Some(button) = find_status_button(child, tooltip) {
                return Some(button);
            }
        }
    }
    None
}
