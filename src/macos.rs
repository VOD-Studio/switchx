//! Route AppKit's Quit (including Command-Q and the Dock) through our recovery flow.

use std::cell::RefCell;

use objc2::{
    class, ffi, msg_send,
    runtime::{AnyObject, Imp, Sel},
    sel,
};

thread_local! {
    static QUIT_HANDLER: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
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
