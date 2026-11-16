//! Window sizes that follow the screen. GPUI reports display bounds in logical pixels —
//! already divided by the Windows scale factor — so a share of the display gives the same
//! proportions on a 14" laptop at 150 % (about 1280×800) as on a 27" monitor at 100 %.
//! A window saved on a bigger monitor comes back shrunk to the screen it opens on.

use gpui::{App, Bounds, Pixels, WindowBounds, point, px, size};

/// Room kept for the taskbar: GPUI reports whole-monitor bounds, not the work area.
const TASKBAR: f32 = 48.;

/// How a kind of window is sized: a share of the work area, within limits.
#[derive(Clone, Copy, Debug)]
pub struct WindowSizing {
    /// Fraction of the work area's width and height.
    pub share: (f32, f32),
    /// Smaller than this doesn't work; if even this doesn't fit, open maximized.
    pub min: (f32, f32),
    /// Larger than this is just empty space (ultrawide and 4K-at-100 % screens).
    pub max: (f32, f32),
}

/// A project window: most of the screen.
pub const PROJECT: WindowSizing = WindowSizing { share: (0.82, 0.86), min: (960., 600.), max: (1920., 1200.) };
/// The Welcome window: a compact dialog.
pub const WELCOME: WindowSizing = WindowSizing { share: (0.55, 0.65), min: (640., 440.), max: (900., 600.) };
/// A detached terminal.
pub const TERMINAL: WindowSizing = WindowSizing { share: (0.6, 0.6), min: (520., 320.), max: (1100., 700.) };

/// The part of `display` windows should stay in (without the taskbar strip).
fn work_area(display: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(display.origin, size(display.size.width, (display.size.height - px(TASKBAR)).max(px(200.))))
}

/// A new window on `display`: its share of the screen, centred; maximized (with 85 % to
/// restore to) when even its minimum size doesn't fit.
pub fn fit_new(display: Bounds<Pixels>, sizing: WindowSizing) -> WindowBounds {
    let area = work_area(display);
    let (aw, ah) = (f32::from(area.size.width), f32::from(area.size.height));
    if sizing.min.0 > aw || sizing.min.1 > ah {
        return WindowBounds::Maximized(Bounds::centered_at(area.center(), size(px(aw * 0.85), px(ah * 0.85))));
    }
    let w = (aw * sizing.share.0).clamp(sizing.min.0, sizing.max.0);
    let h = (ah * sizing.share.1).clamp(sizing.min.1, sizing.max.1);
    WindowBounds::Windowed(Bounds::centered_at(area.center(), size(px(w.round()), px(h.round()))))
}

/// A window put back from the session: no bigger than the screen and fully on it.
pub fn fit_saved(display: Bounds<Pixels>, saved: Bounds<Pixels>) -> Bounds<Pixels> {
    let area = work_area(display);
    let w = saved.size.width.min(area.size.width);
    let h = saved.size.height.min(area.size.height);
    let x = saved.origin.x.max(area.left()).min(area.right() - w);
    let y = saved.origin.y.max(area.top()).min(area.bottom() - h);
    Bounds::new(point(x, y), size(w, h))
}

/// The display a saved window belongs to: the one holding most of it (ties: the primary).
/// With monitors at different scales GPUI's logical coordinates of neighbouring monitors
/// overlap, so "the first display the window touches" can be the wrong one.
pub fn pick_display(displays: &[Bounds<Pixels>], primary: Option<usize>, saved: Bounds<Pixels>) -> Option<usize> {
    let title = Bounds::new(point(saved.origin.x + px(40.), saved.origin.y), size((saved.size.width - px(80.)).max(px(40.)), px(32.)));
    let area = |d: &Bounds<Pixels>| {
        let i = d.intersect(&saved);
        f32::from(i.size.width).max(0.) * f32::from(i.size.height).max(0.)
    };
    displays
        .iter()
        .enumerate()
        // Its title bar must be reachable there (a monitor may have been unplugged since).
        .filter(|(_, d)| d.intersects(&title))
        .max_by(|(a, da), (b, db)| area(da).total_cmp(&area(db)).then((Some(*a) == primary).cmp(&(Some(*b) == primary))))
        .map(|(ix, _)| ix)
}

/// A new window of this kind on the primary display.
pub fn new_window(sizing: WindowSizing, cx: &App) -> WindowBounds {
    let display = cx.primary_display().map(|d| d.bounds()).unwrap_or_else(|| Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(720.))));
    fit_new(display, sizing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    fn windowed(b: WindowBounds) -> Bounds<Pixels> {
        match b {
            WindowBounds::Windowed(b) => b,
            other => panic!("expected a window, got {other:?}"),
        }
    }

    fn inside(b: Bounds<Pixels>, display: Bounds<Pixels>) -> bool {
        b.left() >= display.left() && b.right() <= display.right() && b.top() >= display.top() && b.bottom() <= display.bottom() - px(TASKBAR)
    }

    #[test]
    fn project_windows_follow_the_screen() {
        // Full HD at 100 %, 14" laptop at 150 % (1280×800), 1366×768, 4K at 150 % (2560×1440).
        for display in [rect(0., 0., 1920., 1080.), rect(0., 0., 1280., 800.), rect(0., 0., 1366., 768.), rect(0., 0., 2560., 1440.)] {
            let b = windowed(fit_new(display, PROJECT));
            assert!(inside(b, display), "{b:?} on {display:?}");
            assert!(b.size.width >= px(960.) && b.size.height >= px(600.));
        }
        // Proportional, not fixed: a bigger screen gets a bigger window.
        let small = windowed(fit_new(rect(0., 0., 1280., 800.), PROJECT));
        let big = windowed(fit_new(rect(0., 0., 2560., 1440.), PROJECT));
        assert!(big.size.width > small.size.width && big.size.height > small.size.height);
        // Ultrawide: capped instead of stretched across 5120 pixels.
        assert_eq!(windowed(fit_new(rect(0., 0., 5120., 1440.), PROJECT)).size.width, px(1920.));
        // Too small even for the minimum (1024×600 netbook-like): maximized, restoring inside.
        match fit_new(rect(0., 0., 1024., 600.), PROJECT) {
            WindowBounds::Maximized(b) => assert!(inside(b, rect(0., 0., 1024., 600.))),
            other => panic!("{other:?}"),
        }
        // The Welcome window fits the small laptop as a window.
        assert!(inside(windowed(fit_new(rect(0., 0., 1280., 800.), WELCOME)), rect(0., 0., 1280., 800.)));
    }

    #[test]
    fn saved_windows_pick_the_monitor_holding_most_of_them() {
        // Primary 1920×1080 at 100 %, a 125 % laptop screen to its right: GPUI places the
        // laptop at logical x = 1536, overlapping the primary.
        let displays = [rect(1536., 0., 1536., 864.), rect(0., 0., 1920., 1080.)];
        assert_eq!(pick_display(&displays, Some(1), rect(50., 20., 1800., 1000.)), Some(1));
        assert_eq!(pick_display(&displays, Some(1), rect(2000., 100., 900., 600.)), Some(0));
        // Off every screen (monitor unplugged): none.
        assert_eq!(pick_display(&displays, Some(1), rect(5000., 3000., 800., 600.)), None);
    }

    #[test]
    fn saved_windows_come_back_on_screen() {
        let laptop = rect(0., 0., 1280., 800.);
        // Saved on a big monitor: shrunk to the laptop's work area.
        assert_eq!(fit_saved(laptop, rect(100., 80., 1900., 1100.)), rect(0., 0., 1280., 800. - TASKBAR));
        // Hanging off the right / bottom edge: moved back.
        assert_eq!(fit_saved(laptop, rect(900., 600., 800., 500.)), rect(480., 252., 800., 500.));
        // Already fine: untouched. A second monitor to the right keeps its own coordinates.
        assert_eq!(fit_saved(laptop, rect(50., 40., 1000., 600.)), rect(50., 40., 1000., 600.));
        assert_eq!(fit_saved(rect(1280., 0., 1920., 1080.), rect(1400., 100., 1440., 900.)), rect(1400., 100., 1440., 900.));
    }
}
