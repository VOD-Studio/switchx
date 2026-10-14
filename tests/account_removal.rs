use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize};
use switchx::headless;

slint::slint! {
    import { Drawer } from "../ui/components.slint";
    import { AccountRemovalConfirm } from "../ui/pages/account-dialogs.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component RemovalWindow inherits Window {
        preferred-width: 1000px;
        preferred-height: 683px;
        background: Theme.background;
        in-out property <bool> open;
        in-out property <bool> grok;
        in-out property <bool> linked;
        in-out property <string> references;
        in-out property <int> removed;
        in-out property <int> cancelled;
        // The action row is the dialog's last 38px; its two buttons split the width.
        out property <length> actions-y: dialog.absolute-position.y + dialog.height - 19px;
        out property <length> cancel-x: dialog.absolute-position.x + (dialog.width - 10px) / 4;
        out property <length> confirm-x: dialog.absolute-position.x + dialog.width - (dialog.width - 10px) / 4;
        Drawer {
            open: root.open;
            centered: true;
            title: "移除订阅账号";
            subtitle: root.references == "" ? "从 SwitchX 中移除保存的登录。" : "此账号仍在使用中，暂时无法移除。";
            panel-width: 480px;
            dismissed => { root.open = false; }
            dialog := AccountRemovalConfirm {
                open: root.open;
                grok: root.grok;
                label: root.grok ? "synthetic-grok-account" : "preview@example.invalid";
                initial: root.grok ? "S" : "P";
                codex-linked: root.linked;
                references: root.references;
                busy: false;
                can-remove: true;
                remove => { root.removed += 1; root.open = false; }
                cancel => { root.cancelled += 1; root.open = false; }
            }
        }
    }
}

fn frame(window: &MinimalSoftwareWindow, ms: u64) {
    for _ in 0..ms.div_ceil(16) {
        headless::advance(16);
        headless::render(window);
    }
}

fn snapshot(window: &MinimalSoftwareWindow, name: &str) {
    let pixels = headless::render(window);
    headless::save_snapshot("SWITCHX_ACCOUNT_REMOVAL_SNAPSHOTS", name, &pixels);
}

fn confirm(app: &RemovalWindow) -> (f32, f32) {
    (app.get_confirm_x(), app.get_actions_y())
}

fn cancel(app: &RemovalWindow) -> (f32, f32) {
    (app.get_cancel_x(), app.get_actions_y())
}

#[test]
fn removal_modal_previews_detachment_confirms_once_and_explains_blocked_accounts() {
    let window = headless::install_with_manual_time();
    let app = RemovalWindow::new().unwrap();
    app.window().set_size(PhysicalSize::new(1000, 683));
    app.show().unwrap();
    // Slint animates only properties already rendered before they change.
    headless::render(&window);

    for (dark, motion, reduced) in [
        (true, true, false),
        (false, true, false),
        (true, false, false),
        (false, true, true),
    ] {
        let theme = app.global::<Theme>();
        theme.set_dark(dark);
        theme.set_animations_enabled(motion);
        theme.set_system_reduced_motion(reduced);
        let suffix = format!("{}-{motion}-{reduced}", if dark { "dark" } else { "light" });
        let shot = |name: &str| snapshot(&window, &format!("{name}-{suffix}"));

        // A Codex-linked ChatGPT account explains that the login file goes with it.
        app.set_grok(false);
        app.set_linked(true);
        app.set_references("".into());
        app.set_open(true);
        headless::render(&window);
        frame(&window, 160);
        shot("chatgpt-entering");
        frame(&window, 1000);
        shot("chatgpt-ready");

        let (x, y) = confirm(&app);
        window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(x, y),
        });
        frame(&window, 480);
        shot("chatgpt-hover");

        let before = app.get_removed();
        headless::click(&window, x, y);
        assert_eq!(app.get_removed(), before + 1);
        assert!(!app.get_open());
        if motion && !reduced {
            frame(&window, 160);
            shot("chatgpt-detaching");
        }
        // The closing modal ignores repeated clicks on the faded button.
        headless::click(&window, x, y);
        assert_eq!(app.get_removed(), before + 1);
        frame(&window, 800);
        window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(4.0, 4.0),
        });

        // A reopened modal starts attached again, and Escape keeps the account.
        app.set_grok(true);
        app.set_linked(false);
        app.set_open(true);
        headless::render(&window);
        frame(&window, 1200);
        shot("grok-ready");
        headless::key(&window, Key::Escape);
        frame(&window, 600);
        assert!(!app.get_open());
        assert_eq!(app.get_removed(), before + 1);

        // Upstream references block removal and name the bound connections.
        app.set_grok(false);
        app.set_references("合成连接 A、合成连接 B".into());
        app.set_open(true);
        headless::render(&window);
        frame(&window, 1200);
        shot("blocked-ready");
        let (x, y) = confirm(&app);
        headless::click(&window, x, y);
        assert_eq!(app.get_removed(), before + 1);
        assert!(app.get_open());
        let cancelled = app.get_cancelled();
        let (x, y) = cancel(&app);
        headless::click(&window, x, y);
        assert_eq!(app.get_cancelled(), cancelled + 1);
        assert!(!app.get_open());
        frame(&window, 600);
    }
}
