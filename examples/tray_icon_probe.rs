//! Check the monochrome SVG and native macOS template using the real Slint tray.
//! Run: cargo run --example tray_icon_probe
//! No provider data, credentials, or Codex configuration are read.

use switchx::ui::{AppWindow, SwitchXTray};

#[cfg(target_os = "macos")]
#[path = "../src/macos.rs"]
pub mod macos;

use std::{error::Error, time::Duration};

fn main() -> Result<(), Box<dyn Error>> {
    let image = slint::Image::load_from_svg_data(include_bytes!("../assets/tray.svg"))?;
    let pixels = image.to_rgba8().ok_or("SVG has no pixels")?;
    assert_eq!((pixels.width(), pixels.height()), (44, 36));
    assert!(pixels.as_slice().iter().any(|pixel| pixel.a == 0));
    assert!(pixels.as_slice().iter().any(|pixel| pixel.a == 255));
    assert!(
        pixels
            .as_slice()
            .iter()
            .filter(|pixel| pixel.a > 0)
            .all(|pixel| (pixel.r, pixel.g, pixel.b) == (0, 0, 0))
    );
    println!("PASS: transparent, pure black 44x36 SVG (22x18pt at Retina scale)");

    let _app = AppWindow::new()?;
    let tray = SwitchXTray::new()?;
    slint::platform::update_timers_and_animations();
    #[cfg(target_os = "macos")]
    {
        assert!(macos::use_template_tray_icon("unrelated-tray").is_err());
        macos::use_template_tray_icon("switchx")?;
        println!("PASS: native NSImage.isTemplate, matched only the SwitchX status button");
    }
    tray.on_quit_app(|| {
        let _ = slint::quit_event_loop();
    });
    slint::Timer::single_shot(Duration::from_secs(2), || {
        let _ = slint::quit_event_loop();
    });
    slint::run_event_loop()?;
    drop(tray);
    println!("PASS: native tray created and removed without opening a window");
    Ok(())
}
