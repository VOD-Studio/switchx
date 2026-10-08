//! Verify native macOS window chrome across fullscreen, hiding, and resizing.
//! Run: cargo run --example window_chrome_probe
//! Creates only a UI window; reads no provider data, credentials, or Codex files.

#[cfg(target_os = "macos")]
#[path = "../src/macos.rs"]
pub mod macos;

#[cfg(target_os = "macos")]
fn verify_chrome(phase: &str) {
    use objc2::{class, msg_send, runtime::AnyObject};
    use std::ffi::CStr;

    // SAFETY: called from the UI thread while AppKit owns these live windows.
    unsafe {
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut AnyObject = msg_send![application, windows];
        let count: usize = msg_send![windows, count];
        for index in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: index];
            let title: *mut AnyObject = msg_send![window, title];
            let title: *const std::ffi::c_char = msg_send![title, UTF8String];
            if title.is_null() || CStr::from_ptr(title).to_bytes() != b"switchx" {
                continue;
            }
            let transparent: bool = msg_send![window, titlebarAppearsTransparent];
            let visibility: isize = msg_send![window, titleVisibility];
            let style: usize = msg_send![window, styleMask];
            assert!(transparent, "{phase}: titlebar background returned");
            assert_eq!(visibility, 1, "{phase}: window title became visible");
            assert_ne!(style & (1 << 15), 0, "{phase}: full-size content lost");
            assert_ne!(style & 1, 0, "{phase}: native window decorations lost");
            for kind in [0usize, 1, 2] {
                let button: *mut AnyObject = msg_send![window, standardWindowButton: kind];
                assert!(!button.is_null(), "{phase}: native button {kind} missing");
                let hidden: bool = msg_send![button, isHidden];
                assert!(!hidden, "{phase}: native button {kind} hidden");
            }
            println!(
                "PASS: {phase}, transparent titlebar, hidden title, full-size content, native buttons"
            );
            return;
        }
        panic!("{phase}: native SwitchX window missing");
    }
}

#[cfg(target_os = "macos")]
fn step(app: switchx::ui::AppWindow, phase: u8) {
    use slint::{ComponentHandle, winit_030::WinitWindowAccessor};

    match phase {
        0 => {
            verify_chrome("first show");
            app.window()
                .with_winit_window(|window| {
                    window.set_fullscreen(Some(
                        slint::winit_030::winit::window::Fullscreen::Borderless(None),
                    ));
                })
                .unwrap();
        }
        1 => {
            app.window()
                .with_winit_window(|window| {
                    assert!(
                        window.fullscreen().is_some(),
                        "native fullscreen was not entered"
                    );
                    window.set_fullscreen(None);
                })
                .unwrap();
        }
        2 => {
            app.window()
                .with_winit_window(|window| {
                    assert!(
                        window.fullscreen().is_none(),
                        "native fullscreen did not exit"
                    );
                    assert!(window.is_resizable(), "native window resizing was lost");
                })
                .unwrap();
            verify_chrome("fullscreen exit");
            app.window()
                .dispatch_event(slint::platform::WindowEvent::CloseRequested);
            assert!(
                !app.window().is_visible(),
                "close request did not hide the window"
            );
        }
        3 => app.show().unwrap(),
        4 => {
            verify_chrome("reopen after close");
            app.window().set_size(slint::LogicalSize::new(1000., 680.));
        }
        5 => {
            app.window()
                .with_winit_window(|window| {
                    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
                    assert_eq!((size.width, size.height), (1000., 680.));
                })
                .unwrap();
            verify_chrome("minimum size");
            slint::quit_event_loop().unwrap();
            return;
        }
        _ => unreachable!(),
    }
    slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
        step(app, phase + 1)
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        use slint::ComponentHandle;

        macos::configure_window()?;
        let app = switchx::ui::AppWindow::new()?;
        app.set_native_titlebar_overlay(true);
        app.set_loading(false);
        app.window()
            .on_close_requested(|| slint::CloseRequestResponse::HideWindow);
        app.show()?;
        slint::Timer::single_shot(std::time::Duration::from_secs(1), move || step(app, 0));
        slint::run_event_loop_until_quit()?;
    }
    #[cfg(not(target_os = "macos"))]
    println!("SKIP: native window chrome probe requires macOS");
    Ok(())
}
