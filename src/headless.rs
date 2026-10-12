//! Headless Slint rendering for tests, probes, and synthetic previews.
//! The desktop app never installs it.

use std::{cell::Cell, io::Write, path::PathBuf, rc::Rc, time::Duration};

use slint::{
    LogicalPosition, Rgb8Pixel, SharedPixelBuffer,
    platform::{
        Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
        software_renderer::MinimalSoftwareWindow,
    },
};

/// Animation time that only moves when advanced, for deterministic frames.
#[derive(Clone, Default)]
pub struct Clock(Rc<Cell<Duration>>);

impl Clock {
    pub fn advance(&self, milliseconds: u64) {
        self.0
            .set(self.0.get() + Duration::from_millis(milliseconds));
    }
}

struct RealTime(Rc<MinimalSoftwareWindow>);

impl Platform for RealTime {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

struct ManualTime(Rc<MinimalSoftwareWindow>, Clock);

impl Platform for ManualTime {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.1.0.get()
    }
}

/// Installs a headless platform on this thread; animations follow real time.
pub fn install() -> Rc<MinimalSoftwareWindow> {
    let window = MinimalSoftwareWindow::new(Default::default());
    slint::platform::set_platform(Box::new(RealTime(window.clone())))
        .expect("a Slint platform is already installed on this thread");
    window
}

/// Installs a headless platform whose animation time moves only through the clock.
pub fn install_with_clock() -> (Rc<MinimalSoftwareWindow>, Clock) {
    let window = MinimalSoftwareWindow::new(Default::default());
    let clock = Clock::default();
    slint::platform::set_platform(Box::new(ManualTime(window.clone(), clock.clone())))
        .expect("a Slint platform is already installed on this thread");
    (window, clock)
}

/// Runs due timers and animations, then renders the whole window.
pub fn render(window: &MinimalSoftwareWindow) -> SharedPixelBuffer<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    pixels
}

/// Writes `<name>.ppm` when `variable` names an absolute output directory.
pub fn save_snapshot(variable: &str, name: &str, pixels: &SharedPixelBuffer<Rgb8Pixel>) {
    let Some(output) = std::env::var_os(variable) else {
        return;
    };
    let output = PathBuf::from(output);
    assert!(output.is_absolute(), "{variable} must be an absolute path");
    std::fs::create_dir_all(&output).unwrap();
    let mut file = std::fs::File::create(output.join(format!("{name}.ppm"))).unwrap();
    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height()).unwrap();
    file.write_all(pixels.as_bytes()).unwrap();
}

/// Moves the pointer to a logical position, then presses and releases it.
pub fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
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

pub fn key(window: &MinimalSoftwareWindow, key: Key) {
    window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
}

pub fn scroll(window: &MinimalSoftwareWindow, x: f32, y: f32, delta_y: f32) {
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(x, y),
        delta_x: 0.0,
        delta_y,
    });
}
