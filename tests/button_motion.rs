use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use switchx::headless;

slint::slint! {
    import { QuietButton } from "../ui/components.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    component Sample inherits Rectangle {
        in property <int> variant;
        in property <bool> enabled: true;
        callback clicked();
        public function focus-button() { action.focus(); }
        public function accessible-click() { action.accessible-action-default(); }
        background: Theme.panel;
        border-color: Theme.border;
        border-width: 1px;
        border-radius: 16px;
        height: 150px;

        Text {
            x: 24px; y: 22px;
            text: ["常规操作", "主要操作", "轻量操作", "图标按钮", "侧栏导航", "页签切换"][root.variant];
            color: Theme.muted-text;
            font-size: 12px;
        }
        action := QuietButton {
            x: 24px; y: 66px;
            width: root.variant == 3 ? 44px : parent.width - 48px;
            height: 44px;
            text: ["从连接表单提取", "保存片段", "查看详情", "添加连接", "工作台", "连接配置"][root.variant];
            primary: root.variant == 1;
            ghost: root.variant == 2;
            icon-only: root.variant == 3;
            navigation: root.variant == 4;
            tab: root.variant == 5;
            enabled: root.enabled;
            clicked => { root.clicked(); }
        }
    }

    export component ButtonWindow inherits Window {
        preferred-width: 1200px;
        preferred-height: 820px;
        background: Theme.background;
        in-out property <bool> enabled: true;
        out property <int> clicks;
        public function focus-first() { first.focus-button(); }
        public function accessible-click() { first.accessible-click(); }
        private property <length> sample-width: (root.width - 144px) / 3;

        Text {
            x: 48px; y: 44px;
            text: "SWITCHX / INTERACTION";
            color: Theme.brand;
            font-size: 12px;
            font-weight: 600;
            letter-spacing: 2px;
        }
        Text {
            x: 48px; y: 77px;
            text: "熟悉的轻盈，处处一致。";
            color: Theme.primary-text;
            font-size: 30px;
            font-weight: 600;
        }
        Text {
            x: 48px; y: 127px;
            text: "鼠标经过时，一道柔光轻轻掠过。";
            color: Theme.muted-text;
            font-size: 14px;
        }

        first := Sample {
            x: 48px; y: 184px; width: root.sample-width;
            variant: 0;
            enabled: root.enabled;
            clicked => { root.clicks += 1; }
        }
        for index in 5: Sample {
            x: 48px + (index == 0 || index == 3 ? 1 : index == 1 || index == 4 ? 2 : 0) * (root.sample-width + 24px);
            y: index < 2 ? 184px : 358px;
            width: root.sample-width;
            variant: index + 1;
            enabled: root.enabled;
            clicked => { root.clicks += 1; }
        }

        Rectangle {
            x: 48px; y: 554px; width: root.width - 96px; height: 1px;
            background: Theme.border;
        }
        Text {
            x: 48px; y: 580px;
            text: "保留每种按钮的色彩与形状";
            color: Theme.primary-text;
            font-size: 13px;
        }
        Text {
            x: 48px; y: 607px;
            text: "支持键盘操作 · 跟随系统减少动态效果";
            color: Theme.muted-text;
            font-size: 12px;
        }
    }
}

type Frame = SharedPixelBuffer<Rgb8Pixel>;

#[derive(Clone, Copy)]
struct Bounds {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn bounds(width: u32, variant: usize) -> Bounds {
    let card_width = (width as f32 - 144.0) / 3.0;
    Bounds {
        x: (72.0 + (variant % 3) as f32 * (card_width + 24.0)).round() as u32,
        y: if variant < 3 { 250 } else { 424 },
        width: if variant == 3 {
            44
        } else {
            (card_width - 48.0).round() as u32
        },
        height: 44,
    }
}

fn move_pointer(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
}

fn hover(window: &MinimalSoftwareWindow, area: Bounds) {
    move_pointer(
        window,
        (area.x + area.width / 2) as f32,
        (area.y + area.height / 2) as f32,
    );
}

fn draw(window: &MinimalSoftwareWindow, elapsed: u64) -> Frame {
    headless::advance(elapsed);
    headless::render(window)
}

fn save(name: &str, frame: &Frame) {
    headless::save_snapshot("SWITCHX_BUTTON_SNAPSHOTS", name, frame);
}

fn interior(frame: &Frame, area: Bounds) -> Vec<Rgb8Pixel> {
    (area.y + 3..area.y + area.height - 3)
        .flat_map(|y| {
            (area.x + 3..area.x + area.width - 3)
                .map(move |x| frame.as_slice()[(y * frame.width() + x) as usize])
        })
        .collect()
}

// The background and text finish their hover transitions before these samples.
// Relative to the settled hover image, only the moving highlight should remain.
fn highlight_center(frame: &Frame, settled: &Frame, area: Bounds) -> f64 {
    let mut weight = 0_u64;
    let mut weighted_x = 0_u64;
    for y in area.y + 3..area.y + area.height - 3 {
        for x in area.x + 3..area.x + area.width - 3 {
            let index = (y * frame.width() + x) as usize;
            let pixel = frame.as_slice()[index];
            let baseline = settled.as_slice()[index];
            let delta = u64::from(pixel.r.saturating_sub(baseline.r))
                + u64::from(pixel.g.saturating_sub(baseline.g))
                + u64::from(pixel.b.saturating_sub(baseline.b));
            weight += delta;
            weighted_x += delta * u64::from(x - area.x);
        }
    }
    assert!(weight > 0, "hover must visibly brighten the button");
    weighted_x as f64 / weight as f64
}

// This strip is clear of labels, icons, rounded corners and the border. Its
// uniform fill catches a residual sweep even while disabled opacity is fading.
fn assert_no_highlight(frame: &Frame, area: Bounds) {
    let row = (area.y + 6) * frame.width();
    let pixels =
        &frame.as_slice()[(row + area.x + 10) as usize..(row + area.x + area.width - 10) as usize];
    for channel in [0, 1, 2] {
        let values: Vec<u8> = pixels
            .iter()
            .map(|pixel| [pixel.r, pixel.g, pixel.b][channel])
            .collect();
        assert!(
            values.iter().max().unwrap() - values.iter().min().unwrap() <= 1,
            "disabling motion or the button must hide its sweep immediately"
        );
    }
}

#[test]
fn shared_button_sweep_moves_replays_and_respects_disabled_and_motion_preferences() {
    let window = headless::install_with_manual_time();
    let app = ButtonWindow::new().unwrap();
    app.show().unwrap();

    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let theme = app.global::<Theme>();
            theme.set_dark(dark);
            theme.set_animations_enabled(true);
            theme.set_system_reduced_motion(false);
            app.set_enabled(true);
            move_pointer(&window, 20.0, 20.0);
            draw(&window, 0);
            let name = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            save(&format!("{name}-idle"), &draw(&window, 800));

            if width == 1200 && std::env::var_os("SWITCHX_BUTTON_SNAPSHOTS").is_some() {
                hover(&window, bounds(width, 0));
                save(&format!("motion-{name}-0000"), &draw(&window, 0));
                for frame in 1..=50 {
                    save(
                        &format!("motion-{name}-{:04}", frame * 16),
                        &draw(&window, 16),
                    );
                }
            }

            for variant in 0..6 {
                let area = bounds(width, variant);
                move_pointer(&window, 20.0, 20.0);
                draw(&window, 0);
                draw(&window, 800);
                hover(&window, area);
                draw(&window, 0);
                let middle = draw(&window, 160);
                let later = draw(&window, 40);
                let settled = draw(&window, 600);
                assert!(
                    highlight_center(&later, &settled, area)
                        > highlight_center(&middle, &settled, area),
                    "{name}, variant {variant}: the highlight must move to the right"
                );
                assert!(
                    interior(&settled, area) == interior(&draw(&window, 800), area),
                    "{name}, variant {variant}: hover must finish without looping"
                );
                save(&format!("{name}-{variant}-hover"), &middle);

                move_pointer(&window, 20.0, 20.0);
                draw(&window, 0);
                draw(&window, 800);
                hover(&window, area);
                draw(&window, 0);
                assert!(
                    interior(&middle, area) == interior(&draw(&window, 160), area),
                    "{name}, variant {variant}: re-entering must replay the sweep"
                );

                if variant == 0 {
                    move_pointer(&window, 20.0, 20.0);
                    draw(&window, 0);
                    draw(&window, 40);
                    hover(&window, area);
                    draw(&window, 0);
                    draw(&window, 80);
                    let resumed = draw(&window, 800);
                    assert!(
                        interior(&settled, area) == interior(&resumed, area),
                        "{name}, variant {variant}: an interrupted sweep must settle to the same hover state"
                    );
                    assert!(
                        interior(&resumed, area) == interior(&draw(&window, 800), area),
                        "{name}, variant {variant}: a resumed sweep must finish without looping"
                    );

                    for system_reduced in [false, true] {
                        move_pointer(&window, 20.0, 20.0);
                        draw(&window, 0);
                        draw(&window, 800);
                        hover(&window, area);
                        draw(&window, 0);
                        // The 150 ms background transition is now complete,
                        // while the 650 ms sweep is still visible.
                        draw(&window, 160);
                        theme.set_animations_enabled(system_reduced);
                        theme.set_system_reduced_motion(system_reduced);
                        let stopped = draw(&window, 0);
                        assert_no_highlight(&stopped, area);
                        assert!(
                            interior(&stopped, area) == interior(&draw(&window, 800), area),
                            "{name}, variant {variant}: changing motion preferences must stop an active sweep (system_reduced={system_reduced})"
                        );
                        move_pointer(&window, 20.0, 20.0);
                        draw(&window, 0);
                        theme.set_animations_enabled(true);
                        theme.set_system_reduced_motion(false);
                        draw(&window, 0);
                        draw(&window, 800);
                    }
                }

                move_pointer(&window, 20.0, 20.0);
                draw(&window, 0);
                draw(&window, 800);
                hover(&window, area);
                draw(&window, 0);
                draw(&window, 80);
                app.set_enabled(false);
                assert_no_highlight(&draw(&window, 0), area);
                let disabled = draw(&window, 200);
                assert!(
                    interior(&disabled, area) == interior(&draw(&window, 800), area),
                    "{name}, variant {variant}: a disabled button must remain stationary"
                );
                let clicks = app.get_clicks();
                headless::click(&window, (area.x + 20) as f32, (area.y + 20) as f32);
                headless::key(&window, Key::Return);
                app.invoke_accessible_click();
                assert_eq!(app.get_clicks(), clicks);
                app.set_enabled(true);
            }

            for system_reduced in [false, true] {
                theme.set_animations_enabled(system_reduced);
                theme.set_system_reduced_motion(system_reduced);
                for variant in 0..6 {
                    let area = bounds(width, variant);
                    move_pointer(&window, 20.0, 20.0);
                    draw(&window, 800);
                    hover(&window, area);
                    let immediate = draw(&window, 0);
                    assert!(
                        interior(&immediate, area) == interior(&draw(&window, 200), area),
                        "{name}, variant {variant}: reduced motion must be immediate (system_reduced={system_reduced})"
                    );
                    assert!(
                        interior(&immediate, area) == interior(&draw(&window, 800), area),
                        "{name}, variant {variant}: reduced motion must remain stationary (system_reduced={system_reduced})"
                    );
                }
            }
        }
    }

    app.global::<Theme>().set_animations_enabled(true);
    app.global::<Theme>().set_system_reduced_motion(false);
    app.invoke_focus_first();
    let clicks = app.get_clicks();
    headless::key(&window, Key::Space);
    headless::key(&window, Key::Return);
    app.invoke_accessible_click();
    let first = bounds(1000, 0);
    headless::click(&window, (first.x + 20) as f32, (first.y + 20) as f32);
    assert_eq!(app.get_clicks(), clicks + 4);
}
