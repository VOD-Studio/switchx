use std::{cell::Cell, error::Error, fs, path::Path, rc::Rc};

use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use switchx::{
    headless,
    ui::{AppWindow, Theme},
};

use crate::support::{frame, render, set_theme, snapshot, snapshot_now};

pub(super) fn render_workspace_feedback(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let switches = Rc::new(Cell::new(0));
    let calls = switches.clone();
    app.on_subscription_action(move |action| {
        assert_eq!(
            action, 4,
            "workspace action must keep the existing review flow"
        );
        calls.set(calls.get() + 1);
    });
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.set_active_page(0);
            app.set_error_code("".into());
            app.set_action_message("".into());
            snapshot(window, output, &format!("workspace-workbench-{suffix}"))?;
            // The top-bar workspace entry is removed; exercise the menu directly.
            app.set_workspace_open(true);
            assert!(app.get_workspace_open());
            snapshot(window, output, &format!("workspace-menu-{suffix}"))?;
            headless::click(window, 490.0, 270.0);
            render(window);
            assert_eq!(app.get_active_page(), 5);
            assert!(!app.get_workspace_open());
            app.set_workspace_open(true);
            render(window);
            headless::click(window, 490.0, 314.0);
            render(window);
            assert!(!app.get_workspace_open());
            app.set_active_page(0);
            app.set_error_code("synthetic_conflict".into());
            app.set_error_message("目标配置已在外部修改，操作尚未完成。".into());
            app.set_error_action("请在配置与恢复中检查目标文件，再重新预览。".into());
            snapshot(window, output, &format!("workspace-error-{suffix}"))?;
        }
    }
    assert_eq!(switches.get(), 4);

    // Busy and either OAuth login keep API switching disabled in the relocated menu.
    for pending in 0..3 {
        app.set_busy(pending == 0);
        app.set_chatgpt_login_pending(pending == 1);
        app.set_xai_pending(pending == 2);
        app.set_workspace_open(true);
        render(window);
        headless::click(window, 490.0, 314.0);
        render(window);
        assert_eq!(switches.get(), 4);
        assert!(app.get_workspace_open());
        app.set_workspace_open(false);
    }
    app.set_busy(false);
    app.set_chatgpt_login_pending(false);
    app.set_xai_pending(false);

    app.window().set_size(PhysicalSize::new(1200, 820));
    app.set_error_code("".into());
    app.set_error_message("".into());
    app.set_error_action("".into());
    app.set_active_page(0);
    app.global::<Theme>().set_animations_enabled(true);
    render(window);
    app.set_workspace_open(true);
    let mut elapsed = 0;
    for at in [16, 48, 80, 120, 160, 200, 240, 280, 340, 420] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("workspace-motion-open-{at}ms"))?;
    }
    assert_ne!(
        fs::read(output.join("workspace-motion-open-16ms.ppm"))?,
        fs::read(output.join("workspace-motion-open-420ms.ppm"))?,
        "the workspace menu must animate between captured frames"
    );
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Escape.into(),
    });
    assert!(!app.get_workspace_open());
    frame(420);
    app.global::<Theme>().set_system_reduced_motion(true);
    app.set_workspace_open(true);
    snapshot(window, output, "workspace-reduced-motion")?;
    headless::click(window, 760.0, 400.0);
    assert!(
        !app.get_workspace_open(),
        "outside click must dismiss the menu"
    );
    println!(
        "Synthetic workspace and feedback previews: {}",
        output.display()
    );
    Ok(())
}

// Sidebar item centers without a native titlebar overlay.
pub(super) const SIDEBAR_ITEMS: [(i32, f32); 5] =
    [(0, 111.0), (1, 157.0), (3, 203.0), (4, 275.0), (5, 321.0)];

pub(super) fn sidebar_pixels(pixels: &SharedPixelBuffer<Rgb8Pixel>) -> Vec<u8> {
    let row = pixels.width() as usize * 3;
    pixels
        .as_bytes()
        .chunks(row)
        .flat_map(|line| line[..180 * 3].iter().copied())
        .collect()
}

// Longest vertical brand run in the marker column just inside the focus ring.
pub(super) fn marker_extent(pixels: &SharedPixelBuffer<Rgb8Pixel>, brand: slint::Color) -> usize {
    let width = pixels.width() as usize;
    let (mut longest, mut run) = (0, 0);
    for y in 0..pixels.height() as usize {
        let pixel = pixels.as_slice()[y * width + 16];
        let close = pixel.r.abs_diff(brand.red()) <= 8
            && pixel.g.abs_diff(brand.green()) <= 8
            && pixel.b.abs_diff(brand.blue()) <= 8;
        run = if close { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

pub(super) fn render_sidebar_motion(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let center = |page: i32| {
        SIDEBAR_ITEMS
            .iter()
            .find(|(item, _)| *item == page)
            .map(|(_, y)| {
                *y + if app.get_native_titlebar_overlay() {
                    18.0
                } else {
                    0.0
                }
            })
            .unwrap()
    };
    let path = [1, 5, 0, 4, 3, 0];
    app.window().set_size(PhysicalSize::new(1200, 820));
    for dark in [false, true] {
        let suffix = if dark { "dark" } else { "light" };
        app.global::<Theme>().set_animations_enabled(false);
        app.set_active_page(0);
        set_theme(app, window, dark);
        let brand = app.global::<Theme>().get_brand();
        let mut settled = std::collections::HashMap::new();
        for page in path {
            headless::click(window, 90.0, center(page));
            assert_eq!(app.get_active_page(), page);
            settled.insert(page, render(window));
        }
        let rest = marker_extent(&settled[&0], brand);

        app.global::<Theme>().set_animations_enabled(true);
        for _ in 0..24 {
            frame(32);
            headless::render(window);
        }
        let mut from = 0;
        for page in path {
            headless::click(window, 90.0, center(page));
            assert_eq!(app.get_active_page(), page);
            let (mut elapsed, mut stretch) = (0, 0);
            for at in [
                0, 16, 32, 48, 64, 96, 128, 160, 200, 240, 300, 340, 420, 600,
            ] {
                frame(at - elapsed);
                elapsed = at;
                let pixels = headless::render(window);
                let sidebar = sidebar_pixels(&pixels);
                if at == 64 {
                    assert_ne!(sidebar, sidebar_pixels(&settled[&from]));
                    assert_ne!(
                        sidebar,
                        sidebar_pixels(&settled[&page]),
                        "the selection must still be travelling"
                    );
                }
                stretch = stretch.max(marker_extent(&pixels, brand));
                snapshot_now(
                    window,
                    output,
                    &format!("sidebar-{from}-to-{page}-{suffix}-{at:03}ms"),
                )?;
            }
            assert!(
                stretch >= rest + 4,
                "the marker must stretch toward page {page}: {stretch} vs {rest}"
            );
            assert_eq!(
                sidebar_pixels(&headless::render(window)),
                sidebar_pixels(&settled[&page]),
                "the selection must settle exactly on page {page}"
            );
            from = page;
        }

        // Retargeting mid-flight still settles on the latest page.
        headless::click(window, 90.0, center(5));
        frame(64);
        headless::render(window);
        headless::click(window, 90.0, center(1));
        frame(600);
        assert_eq!(
            sidebar_pixels(&headless::render(window)),
            sidebar_pixels(&settled[&1])
        );

        // Reduced motion lands on the first frame, without a timer tick.
        app.global::<Theme>().set_system_reduced_motion(true);
        for page in [5, 3] {
            headless::click(window, 90.0, center(page));
            assert_eq!(
                sidebar_pixels(&headless::render(window)),
                sidebar_pixels(&settled[&page]),
                "reduced motion must move the selection immediately"
            );
        }
        app.global::<Theme>().set_system_reduced_motion(false);
    }
    app.global::<Theme>().set_animations_enabled(false);
    println!(
        "Synthetic sidebar transitions and settle checks: {}",
        output.display()
    );
    Ok(())
}
