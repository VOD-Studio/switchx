use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use std::{cell::Cell, io::Write, rc::Rc, time::Duration};

slint::slint! {
    import { QuietButton, SurfaceCard } from "../ui/components.slint";
    import { AccountCard } from "../ui/pages/account-card.slint";
    import { AccountRow } from "../ui/view-models.slint";
    export { AccountRow, CodexQuotaRow, XaiQuotaRow } from "../ui/view-models.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component AccountCardWindow inherits Window {
        preferred-width: 820px;
        preferred-height: 500px;
        background: Theme.background;
        in-out property <AccountRow> account;
        in-out property <bool> expanded: false;
        in-out property <bool> removing: false;
        out property <length> card-height: card.height;
        card := AccountCard {
            x: 30px; y: 30px; width: 760px;
            account: root.account;
            expanded: root.expanded;
            removing: root.removing;
            toggle-details(id) => { root.expanded = !root.expanded; }
        }
    }

    export component SurfaceWindow inherits Window {
        width: 400px;
        height: 240px;
        background: Theme.background;
        in-out property <bool> enabled: true;
        in-out property <bool> selected: false;
        in-out property <color> card-fill: Theme.panel;
        in-out property <color> card-border: Theme.border;
        in-out property <length> card-border-width: 2px;
        out property <int> clicks;
        out property <string> action-label: action.accessible-label;
        out property <bool> action-enabled: action.accessible-enabled;
        public function focus-action() { action.focus(); }
        public function accessible-click() { action.accessible-action-default(); }

        SurfaceCard {
            x: 20px; y: 20px; width: 360px; height: 200px;
            background: root.card-fill;
            border-color: root.card-border;
            border-width: root.card-border-width;
            border-radius: 12px;
            Rectangle { x: 16px; y: 150px; width: 40px; height: 20px; background: Theme.brand; }
            action := QuietButton {
                x: 24px; y: 30px; width: 160px; height: 36px;
                text: "保存连接";
                enabled: root.enabled;
                selected: root.selected;
                clicked => { root.clicks += 1; }
            }
            QuietButton {
                x: 210px; y: 30px; width: 100px; height: 36px;
                text: "主要操作";
                primary: true;
            }
            QuietButton {
                x: 24px; y: 90px; width: 36px;
                text: "添加连接";
                icon-only: true;
                enabled: root.enabled;
            }
            QuietButton {
                x: 90px; y: 90px; width: 120px;
                text: "导航操作";
                navigation: true;
                selected: true;
            }
        }
    }
}

thread_local! { static PREVIEW_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) }; }

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }

    fn duration_since_start(&self) -> Duration {
        PREVIEW_TIME.with(Cell::get)
    }
}

fn advance(milliseconds: u64) {
    PREVIEW_TIME.with(|time| time.set(time.get() + Duration::from_millis(milliseconds)));
    slint::platform::update_timers_and_animations();
}

fn draw(window: &MinimalSoftwareWindow, name: &str) -> SharedPixelBuffer<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    if let Some(output) = std::env::var_os("SWITCHX_SURFACE_SNAPSHOTS") {
        let output = std::path::PathBuf::from(output);
        assert!(output.is_absolute());
        std::fs::create_dir_all(&output).unwrap();
        let mut file = std::fs::File::create(output.join(format!("{name}.ppm"))).unwrap();
        write!(file, "P6\n{} {}\n255\n", size.width, size.height).unwrap();
        file.write_all(pixels.as_bytes()).unwrap();
    }
    pixels
}

fn key(window: &MinimalSoftwareWindow, key: Key) {
    window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

#[test]
fn account_card_reverses_disclosure_and_removal_without_losing_quota_details() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = AccountCardWindow::new().unwrap();
    let mut account = AccountRow {
        id: "synthetic-account".into(),
        label: "preview@example.invalid".into(),
        workspace: "synthetic-workspace".into(),
        codex_quota: CodexQuotaRow {
            has_value: true,
            primary_window: XaiQuotaRow {
                has_value: true,
                remaining_percent: 78.0,
                period_label: "每周额度".into(),
                reset_label: "5 天后重置".into(),
                reset_detail: "重置于 10月15日 09:30".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    app.set_account(account.clone());
    app.window().set_size(PhysicalSize::new(820, 500));
    app.show().unwrap();
    draw(&window, "account-entry-start");
    advance(16);
    draw(&window, "account-entry-ready");
    advance(300);
    draw(&window, "account-ready");
    let collapsed = app.get_card_height();
    assert!(collapsed > 100.0);
    click(&window, 692.0, 65.0);
    assert!(app.get_expanded());
    draw(&window, "account-details-start");
    advance(60);
    draw(&window, "account-details-partial");
    let partial = app.get_card_height();
    advance(300);
    draw(&window, "account-details-open");
    let expanded = app.get_card_height();
    assert!(partial > collapsed && partial < expanded);
    assert!(expanded > collapsed + 60.0);

    app.set_expanded(false);
    draw(&window, "account-details-closing");
    advance(50);
    app.set_expanded(true);
    draw(&window, "account-details-reversing");
    advance(300);
    account.codex_quota.primary_window.remaining_percent = 62.0;
    app.set_account(account);
    draw(&window, "account-quota-updated");
    assert!(app.get_expanded());
    assert!((app.get_card_height() - expanded).abs() < 1.0);

    app.set_removing(true);
    draw(&window, "account-removal-start");
    advance(60);
    draw(&window, "account-removal-partial");
    let removal_partial = app.get_card_height();
    assert!(removal_partial > 0.0 && removal_partial < expanded);
    app.set_removing(false);
    draw(&window, "account-removal-reversed");
    advance(300);
    draw(&window, "account-removal-restored");
    assert!((app.get_card_height() - expanded).abs() < 1.0);

    app.global::<Theme>().set_system_reduced_motion(true);
    app.set_expanded(false);
    draw(&window, "account-reduced-collapsed");
    assert!((app.get_card_height() - collapsed).abs() < 1.0);
    app.set_expanded(true);
    draw(&window, "account-reduced-expanded");
    assert!((app.get_card_height() - expanded).abs() < 1.0);
    app.set_removing(true);
    draw(&window, "account-reduced-removed");
    assert_eq!(app.get_card_height(), 0.0);
}

#[test]
fn account_card_quota_fill_grows_on_entry_and_pulses_only_while_refreshing() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = AccountCardWindow::new().unwrap();
    let mut account = AccountRow {
        id: "synthetic-account".into(),
        label: "preview@example.invalid".into(),
        initial: "P".into(),
        codex_quota: CodexQuotaRow {
            has_value: true,
            primary_window: XaiQuotaRow {
                has_value: true,
                remaining_percent: 80.0,
                period_label: "每周额度".into(),
                reset_label: "5 天后重置".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    app.set_account(account.clone());
    app.window().set_size(PhysicalSize::new(820, 500));
    app.show().unwrap();
    draw(&window, "quota-fill-start");
    // The card enters after 16 ms and the fill starts growing roughly 120 ms later.
    // Draw every step like a running window does; Slint only animates a property
    // that was already rendered before its value changed.
    for _ in 0..6 {
        advance(if app.get_card_height() == 0.0 { 16 } else { 60 });
        draw(&window, "quota-fill-step");
    }
    let growing = draw(&window, "quota-fill-growing");
    advance(1200);
    let settled = draw(&window, "quota-fill-settled");
    // Card top + padding + header + gap + well padding + half the 20px row: the 80% fill
    // spans x = 146..474 once settled.
    let bar_row = (30 + 16 + 40 + 14 + 13 + 10) * 820;
    assert_eq!(
        growing.as_slice()[bar_row + 200],
        settled.as_slice()[bar_row + 200]
    );
    assert_ne!(
        growing.as_slice()[bar_row + 450],
        settled.as_slice()[bar_row + 450],
        "The fill must still be growing and not yet reach its final width"
    );
    advance(450);
    assert_eq!(
        draw(&window, "quota-fill-idle").as_bytes(),
        settled.as_bytes(),
        "An idle quota fill must stay still"
    );

    account.codex_quota.loading = true;
    app.set_account(account.clone());
    draw(&window, "quota-pulse-start");
    advance(450);
    let dimmed = draw(&window, "quota-pulse-mid");
    assert_ne!(dimmed.as_bytes(), settled.as_bytes());
    advance(450);
    let returned = draw(&window, "quota-pulse-peak");
    assert_ne!(returned.as_bytes(), dimmed.as_bytes());

    account.codex_quota.loading = false;
    app.set_account(account);
    advance(16);
    let finished = draw(&window, "quota-pulse-finished");
    advance(450);
    let still = draw(&window, "quota-pulse-still");
    let header = 0..60 * 820 * 3;
    assert_eq!(
        &finished.as_bytes()[header.clone()],
        &still.as_bytes()[header],
        "The header must not change once the refresh ends"
    );
    assert_eq!(
        finished.as_slice()[bar_row + 200],
        still.as_slice()[bar_row + 200]
    );
    assert_eq!(
        finished.as_slice()[bar_row + 200],
        settled.as_slice()[bar_row + 200]
    );
}

#[test]
fn shared_surfaces_preserve_styling_motion_and_accessible_button_actions() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = SurfaceWindow::new().unwrap();
    app.window().set_size(PhysicalSize::new(400, 240));
    app.show().unwrap();

    for dark in [false, true] {
        let theme = app.global::<Theme>();
        theme.set_dark(dark);
        theme.set_animations_enabled(true);
        app.set_card_fill(theme.get_panel());
        app.set_card_border(theme.get_border());
        app.set_card_border_width(2.0);
        app.set_enabled(true);
        app.set_selected(false);
        advance(250);
        let suffix = if dark { "dark" } else { "light" };
        draw(&window, &format!("enabled-{suffix}"));
        assert_eq!(app.get_action_label(), "保存连接");
        assert!(app.get_action_enabled());

        let clicks = app.get_clicks();
        click(&window, 80.0, 68.0);
        key(&window, Key::Space);
        key(&window, Key::Return);
        app.invoke_accessible_click();
        assert_eq!(app.get_clicks(), clicks + 4);
        advance(200);
        draw(&window, &format!("focused-{suffix}"));

        app.set_enabled(false);
        draw(&window, &format!("disabled-start-{suffix}"));
        advance(40);
        let middle = draw(&window, &format!("disabled-mid-{suffix}"));
        advance(200);
        let disabled = draw(&window, &format!("disabled-end-{suffix}"));
        assert_ne!(middle.as_bytes(), disabled.as_bytes());
        assert!(!app.get_action_enabled());
        click(&window, 80.0, 68.0);
        key(&window, Key::Space);
        app.invoke_accessible_click();
        assert_eq!(app.get_clicks(), clicks + 4);

        app.set_enabled(true);
        app.invoke_focus_action();
        key(&window, Key::Return);
        assert_eq!(app.get_clicks(), clicks + 5);
        app.set_selected(true);
        advance(250);
        let start = draw(&window, &format!("card-start-{suffix}"));
        app.set_card_fill(slint::Color::from_rgb_u8(60, 100, 140));
        app.set_card_border(slint::Color::from_rgb_u8(180, 100, 40));
        draw(&window, &format!("card-transition-start-{suffix}"));
        advance(60);
        let middle = draw(&window, &format!("card-mid-{suffix}"));
        advance(250);
        let end = draw(&window, &format!("card-end-{suffix}"));
        assert_ne!(start.as_bytes(), middle.as_bytes());
        assert_ne!(middle.as_bytes(), end.as_bytes());
        assert_eq!(
            end.as_slice()[150 * 400 + 200],
            Rgb8Pixel::new(60, 100, 140)
        );

        for reduced_motion in [false, true] {
            theme.set_animations_enabled(reduced_motion);
            theme.set_system_reduced_motion(reduced_motion);
            app.set_card_fill(slint::Color::from_rgb_u8(140, 100, 60));
            app.set_enabled(false);
            let immediate = draw(&window, &format!("reduced-start-{suffix}-{reduced_motion}"));
            assert_eq!(
                immediate.as_slice()[150 * 400 + 200],
                Rgb8Pixel::new(140, 100, 60)
            );
            advance(250);
            let settled = draw(&window, &format!("reduced-end-{suffix}-{reduced_motion}"));
            assert_eq!(immediate.as_bytes(), settled.as_bytes());
            app.set_card_fill(slint::Color::from_rgb_u8(60, 100, 140));
            draw(&window, "reset-card");
        }
        theme.set_system_reduced_motion(false);
    }
}
