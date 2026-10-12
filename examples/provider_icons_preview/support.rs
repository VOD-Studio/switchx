use std::{error::Error, fs, io::Write, path::Path};

use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{ComponentHandle, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};
use switchx::{
    headless,
    ui::{AppWindow, ProviderIconRow, Theme},
};

pub(super) fn render(window: &MinimalSoftwareWindow) -> SharedPixelBuffer<Rgb8Pixel> {
    headless::render(window);
    headless::advance(250);
    headless::render(window)
}

pub(super) fn snapshot(
    window: &MinimalSoftwareWindow,
    output: &Path,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    let pixels = render(window);
    let mut file = std::io::BufWriter::new(fs::File::create(output.join(format!("{name}.ppm")))?);
    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
    file.write_all(pixels.as_bytes())?;
    file.flush()?;
    Ok(())
}

pub(super) fn set_theme(app: &AppWindow, window: &MinimalSoftwareWindow, dark: bool) {
    app.invoke_set_appearance(dark);
    render(window);
    assert_eq!(app.global::<Theme>().get_dark(), dark);
}

pub(super) fn render_icons(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    icons: &[ProviderIconRow],
) {
    app.set_icon_picker_open(true);
    for row in icons {
        app.set_filtered_provider_icons(ModelRc::new(VecModel::from(vec![row.clone()])));
        render(window);
    }
    app.set_icon_picker_open(false);
}

pub(super) fn frame(step_ms: u64) {
    headless::advance(step_ms);
    slint::platform::update_timers_and_animations();
}

// Unlike `snapshot`, keeps the clock where `frame` left it.
pub(super) fn snapshot_now(
    window: &MinimalSoftwareWindow,
    output: &Path,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    let pixels = headless::render(window);
    let mut file = std::io::BufWriter::new(fs::File::create(output.join(format!("{name}.ppm")))?);
    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
    file.write_all(pixels.as_bytes())?;
    file.flush()?;
    Ok(())
}

// Every `step` milliseconds through `end`, for smooth motion frames.
pub(super) fn every(step: u64, end: u64) -> Vec<u64> {
    (0..=end).step_by(step as usize).collect()
}

// Plays real 16 ms frames so per-frame tweens behave as in the app, saving the listed moments.
pub(super) fn play(
    window: &MinimalSoftwareWindow,
    output: &Path,
    name: &str,
    moments: &[u64],
) -> Result<(), Box<dyn Error>> {
    let end = moments.iter().copied().max().unwrap_or(0);
    let mut at = 0;
    loop {
        if moments.contains(&at) {
            snapshot_now(window, output, &format!("{name}-{at:04}ms"))?;
        } else {
            headless::render(window);
        }
        if at >= end {
            return Ok(());
        }
        frame(16);
        at += 16;
    }
}
