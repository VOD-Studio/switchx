use std::{cell::RefCell, rc::Rc};

use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use switchx::{
    headless,
    ui::{AppWindow, Theme},
};

const HOME: &str = "/isolated/synthetic-codex";
const HOME_LABEL: &str = "Codex 配置目录绝对路径";

fn advance(window: &MinimalSoftwareWindow, milliseconds: u64) {
    headless::advance(milliseconds);
    headless::render(window);
}

fn settle(window: &MinimalSoftwareWindow) {
    headless::render(window);
    advance(window, 800);
    advance(window, 800);
}

fn draw(window: &MinimalSoftwareWindow, name: &str) -> SharedPixelBuffer<Rgb8Pixel> {
    let pixels = headless::render(window);
    headless::save_snapshot("SWITCHX_SETTINGS_SNAPSHOTS", name, &pixels);
    pixels
}

fn control_any(window: &MinimalSoftwareWindow, app: &AppWindow, labels: &[&str]) -> ElementHandle {
    let height = app.window().size().height as f32;
    for attempt in 0..16 {
        let item = labels.iter().find_map(|label| {
            let mut matches = ElementHandle::find_by_accessible_label(app, label);
            let item = matches.next();
            assert!(
                matches.next().is_none(),
                "ambiguous settings control: {label}"
            );
            item
        });
        let mut delta = if attempt == 0 { 10_000.0 } else { -150.0 };
        if let Some(item) = item {
            let y = item.absolute_position().y + item.size().height / 2.0;
            if y > 175.0 && y < height - 30.0 {
                return item;
            }
            delta = if y < 175.0 { 150.0 } else { -150.0 };
        }
        headless::scroll(window, 700.0, height - 100.0, delta);
        settle(window);
    }
    panic!("settings controls are not reachable: {labels:?}");
}

fn control(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str) -> ElementHandle {
    control_any(window, app, &[label])
}

fn click(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str) {
    let item = control(window, app, label);
    let position = item.absolute_position();
    let size = item.size();
    headless::click(
        window,
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    );
    headless::render(window);
}

fn reset(window: &MinimalSoftwareWindow, app: &AppWindow) {
    app.set_active_page(0);
    headless::render(window);
    app.set_busy(false);
    app.set_config_managed(false);
    app.set_recovery_only(false);
    app.set_chatgpt_login_pending(false);
    app.set_xai_pending(false);
    app.set_direct_drawer_open(false);
    app.set_config_home(HOME.into());
    app.set_route_port("18731".into());
    app.set_config_status("合成配置 · 由 Codex 管理登录".into());
    app.set_active_page(5);
    settle(window);
    let disclosure = control_any(window, app, &["收起配置与恢复", "展开配置与恢复"]);
    if disclosure.accessible_expanded() == Some(true) {
        click(window, app, "收起配置与恢复");
        settle(window);
    }
    headless::scroll(
        window,
        700.0,
        app.window().size().height as f32 - 100.0,
        10_000.0,
    );
    settle(window);
}

fn enabled(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str, expected: bool) {
    assert_eq!(
        control(window, app, label).accessible_enabled(),
        Some(expected),
        "{label}"
    );
}

fn content(pixels: &SharedPixelBuffer<Rgb8Pixel>) -> Vec<u8> {
    let stride = pixels.width() as usize * 3;
    pixels
        .as_bytes()
        .chunks(stride)
        .skip(175)
        .flat_map(|row| row[210 * 3..].iter().copied())
        .collect()
}

#[test]
fn settings_preserve_actions_bindings_and_disabled_states_in_both_layouts() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    app.set_loading(false);
    app.global::<Theme>().set_animations_enabled(false);
    let actions = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let calls = actions.clone();
    app.on_inspect_config(move |home| {
        calls
            .borrow_mut()
            .push(("inspect".into(), home.to_string()))
    });
    let calls = actions.clone();
    app.on_import_current(move |home| calls.borrow_mut().push(("import".into(), home.to_string())));
    let calls = actions.clone();
    app.on_check_login(move |home| calls.borrow_mut().push(("login".into(), home.to_string())));
    let calls = actions.clone();
    app.on_restore_config(move |home| {
        calls
            .borrow_mut()
            .push(("restore".into(), home.to_string()))
    });
    let calls = actions.clone();
    app.on_open_common_config(move || calls.borrow_mut().push(("common".into(), String::new())));
    app.show().unwrap();

    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.invoke_set_appearance(dark);
            reset(&window, &app);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            draw(&window, &format!("settings-{suffix}"));
            assert_eq!(
                control(&window, &app, "展开配置与恢复").accessible_expanded(),
                Some(false)
            );
            click(&window, &app, "深色主题");
            assert!(app.global::<Theme>().get_dark());
            click(&window, &app, "浅色主题");
            assert!(!app.global::<Theme>().get_dark());
            app.invoke_set_appearance(dark);

            for (label, action) in [
                ("读取配置", "inspect"),
                ("导入当前上游", "import"),
                ("检查 Codex 登录", "login"),
            ] {
                click(&window, &app, label);
                assert_eq!(
                    actions.borrow().last(),
                    Some(&(action.to_string(), HOME.to_string()))
                );
            }
            click(&window, &app, "编辑通用配置");
            assert_eq!(
                actions.borrow().last(),
                Some(&("common".into(), String::new()))
            );

            click(&window, &app, HOME_LABEL);
            headless::key(&window, "-edited");
            assert!(
                app.get_config_home().contains("-edited"),
                "workspace input must retain its binding"
            );
            app.set_config_home(HOME.into());
            click(&window, &app, "本地路由端口");
            headless::key(&window, Key::Backspace);
            assert_ne!(
                app.get_route_port(),
                "18731",
                "port input must retain its binding"
            );
            app.set_route_port("18731".into());

            click(&window, &app, "展开配置与恢复");
            assert_eq!(
                control(&window, &app, "收起配置与恢复").accessible_expanded(),
                Some(true)
            );
            enabled(&window, &app, "恢复原配置", false);
            click(&window, &app, "查看直连配置");
            assert!(app.get_direct_drawer_open());
            app.set_direct_drawer_open(false);
            settle(&window);
            app.set_config_managed(true);
            settle(&window);
            for label in [HOME_LABEL, "本地路由端口", "编辑通用配置"] {
                enabled(&window, &app, label, false);
            }
            enabled(&window, &app, "恢复原配置", true);
            click(&window, &app, "恢复原配置");
            assert_eq!(
                actions.borrow().last(),
                Some(&("restore".into(), HOME.into()))
            );
            draw(&window, &format!("settings-managed-{suffix}"));

            for pending in 0..3 {
                app.set_config_managed(false);
                app.set_busy(pending == 0);
                app.set_chatgpt_login_pending(pending == 1);
                app.set_xai_pending(pending == 2);
                settle(&window);
                let before = actions.borrow().len();
                for label in [HOME_LABEL, "读取配置", "导入当前上游", "检查 Codex 登录"]
                {
                    enabled(&window, &app, label, false);
                }
                for label in ["读取配置", "导入当前上游", "检查 Codex 登录"] {
                    click(&window, &app, label);
                }
                assert_eq!(
                    actions.borrow().len(),
                    before,
                    "disabled actions must not dispatch"
                );
                enabled(&window, &app, "本地路由端口", pending != 0);
                enabled(&window, &app, "编辑通用配置", pending != 0);
            }
            reset(&window, &app);
            app.set_config_managed(true);
            app.set_recovery_only(true);
            settle(&window);
            control(&window, &app, "收起配置与恢复");
            click(&window, &app, "恢复原配置");
            assert_eq!(
                actions.borrow().last(),
                Some(&("restore".into(), HOME.into()))
            );
            draw(&window, &format!("settings-conflict-{suffix}"));
        }
    }

    reset(&window, &app);
    click(&window, &app, "启用界面动画");
    assert!(app.global::<Theme>().get_animations_enabled());
    headless::key(&window, Key::Space);
    assert!(!app.global::<Theme>().get_animations_enabled());
    app.global::<Theme>().set_system_reduced_motion(true);
    enabled(&window, &app, "启用界面动画", false);
    click(&window, &app, "启用界面动画");
    assert!(!app.global::<Theme>().get_animations_enabled());
}

#[test]
fn settings_motion_animates_reverses_and_respects_reduced_motion() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    app.set_loading(false);
    app.window().set_size(PhysicalSize::new(1200, 820));
    app.show().unwrap();
    app.global::<Theme>().set_animations_enabled(false);
    reset(&window, &app);
    app.global::<Theme>().set_animations_enabled(true);
    settle(&window);

    click(&window, &app, "播放动效预览");
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(190.0, 70.0),
    });
    let initial = content(&draw(&window, "motion-start"));
    advance(&window, 96);
    let partial = content(&draw(&window, "motion-096ms"));
    assert!(initial != partial, "preview must visibly animate");
    settle(&window);
    settle(&window);

    click(&window, &app, "展开配置与恢复");
    let first = content(&draw(&window, "recovery-opening"));
    advance(&window, 80);
    let moving = content(&draw(&window, "recovery-080ms"));
    assert!(
        first != moving,
        "disclosure must have intermediate animation frames"
    );
    click(&window, &app, "收起配置与恢复");
    advance(&window, 48);
    click(&window, &app, "展开配置与恢复");
    settle(&window);
    control(&window, &app, "收起配置与恢复");
    enabled(&window, &app, "查看直连配置", true);
    click(&window, &app, "查看直连配置");
    assert!(
        app.get_direct_drawer_open(),
        "interrupted disclosure must settle with reachable actions"
    );
    app.set_direct_drawer_open(false);

    for reduced in [false, true] {
        app.global::<Theme>().set_animations_enabled(reduced);
        app.global::<Theme>().set_system_reduced_motion(reduced);
        reset(&window, &app);
        click(&window, &app, "展开配置与恢复");
        window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(190.0, 70.0),
        });
        let immediate = content(&draw(&window, "motion-disabled-immediate"));
        settle(&window);
        assert!(
            immediate == content(&draw(&window, "motion-disabled-settled")),
            "disabled motion must land on the first frame"
        );
        click(&window, &app, "播放动效预览");
        window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(190.0, 70.0),
        });
        let immediate = content(&draw(&window, "preview-disabled-immediate"));
        settle(&window);
        assert!(
            immediate == content(&draw(&window, "preview-disabled-settled")),
            "disabled preview must change its position without a transition"
        );
        enabled(&window, &app, "启用界面动画", !reduced);
    }
}
