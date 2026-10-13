//! What dsh is doing, on the icons: the tray's, and on Windows the taskbar
//! button's overlay.
//!
//! Three looks. Idle is the icon as shipped. A turn running puts a blue dot in
//! its corner, and a session waiting on the user an orange one, which wins
//! over running — a turn that has stopped to ask is the one thing worth
//! interrupting the user for. Still images: a spinning tray icon is a timer
//! redrawing it for as long as a turn runs, and a dot says "busy" as well.
//!
//! The counts come from the plugin (`status` in `plugin/lib/client.js`) and go
//! back to idle whenever the window starts loading a page, because the plugin
//! that reported them goes with the document. A page that loads mid-turn
//! reports again at once. Like every signal, anything in the window can forge
//! one, and what a forged one buys is a wrong dot.

use std::sync::Mutex;

use tauri::image::Image;
use tauri::AppHandle;
#[cfg(windows)]
use tauri::Manager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    Idle,
    Running,
    Waiting,
}

impl Look {
    fn of(running: u32, waiting: u32) -> Look {
        if waiting > 0 {
            Look::Waiting
        } else if running > 0 {
            Look::Running
        } else {
            Look::Idle
        }
    }

    /// The dot's colour, as RGB.
    fn colour(self) -> Option<[u8; 3]> {
        match self {
            Look::Idle => None,
            Look::Running => Some([0x3b, 0x82, 0xf6]),
            Look::Waiting => Some([0xf5, 0x9e, 0x0b]),
        }
    }
}

static LOOK: Mutex<Look> = Mutex::new(Look::Idle);

/// The plugin's latest counts. Repaints only when the look changes.
pub fn set(app: &AppHandle, running: u32, waiting: u32) {
    let look = Look::of(running, waiting);
    let Ok(mut current) = LOOK.lock() else { return };
    if *current == look {
        return;
    }
    *current = look;
    drop(current);
    paint(app, look);
}

/// A navigation has started: whatever the plugin said was about the document
/// being left.
pub fn page_started(app: &AppHandle) {
    set(app, 0, 0);
}

/// What the tray's tooltip adds, if anything.
pub fn describe() -> Option<&'static str> {
    match LOOK.lock().map(|look| *look).unwrap_or(Look::Idle) {
        Look::Idle => None,
        Look::Running => Some(t!("正在运行", "Running")),
        Look::Waiting => Some(t!("在等你回应", "Waiting for you")),
    }
}

fn paint(app: &AppHandle, look: Look) {
    if let (Some(tray), Some(base)) = (app.tray_by_id(crate::TRAY), app.default_window_icon()) {
        let icon = match look.colour() {
            None => base.clone(),
            Some(colour) => {
                let (width, height) = (base.width(), base.height());
                // Big enough to read at the 16 pixels a tray draws it at.
                Image::new_owned(dot(base.rgba(), width, height, colour, 0.46), width, height)
            }
        };
        if let Err(error) = tray.set_icon(Some(icon)) {
            eprintln!("dsh-desktop: could not repaint the tray icon: {error}");
        }
    }
    crate::refresh_tooltip(app);

    // Windows draws the overlay over the bottom-right of the taskbar button at
    // 16 pixels, whatever it is given; this is the same dot, filling it.
    #[cfg(windows)]
    if let Some(window) = app.get_webview_window("main") {
        const SIZE: u32 = 32;
        let overlay = look.colour().map(|colour| {
            let blank = vec![0; (SIZE * SIZE * 4) as usize];
            Image::new_owned(dot(&blank, SIZE, SIZE, colour, 1.0), SIZE, SIZE)
        });
        if let Err(error) = window.set_overlay_icon(overlay) {
            eprintln!("dsh-desktop: could not set the taskbar overlay: {error}");
        }
    }
}

/// Paint a dot of `colour`, with a white ring so it reads on a dark taskbar
/// and over the icon alike, `share` of the image across, into its
/// bottom-right corner. Edges are blended by coverage, so it is round at 16
/// pixels rather than a cross.
fn dot(rgba: &[u8], width: u32, height: u32, colour: [u8; 3], share: f32) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let size = width.min(height) as f32;
    let outer = size * share / 2.0;
    let ring = (size / 16.0).max(1.0);
    let centre = (width as f32 - outer, height as f32 - outer);

    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 + 0.5 - centre.0;
            let dy = y as f32 + 0.5 - centre.1;
            let distance = (dx * dx + dy * dy).sqrt();
            // How much of this pixel each disc covers, as a one-pixel ramp.
            let disc = (outer - distance + 0.5).clamp(0.0, 1.0);
            if disc == 0.0 {
                continue;
            }
            let fill = (outer - ring - distance + 0.5).clamp(0.0, 1.0);
            let paint = [0, 1, 2].map(|c| colour[c] as f32 * fill + 255.0 * (1.0 - fill));

            let at = ((y * width + x) * 4) as usize;
            let under_alpha = out[at + 3] as f32 / 255.0;
            let alpha = disc + under_alpha * (1.0 - disc);
            for c in 0..3 {
                let under = out[at + c] as f32 * under_alpha * (1.0 - disc);
                out[at + c] = ((paint[c] * disc + under) / alpha).round() as u8;
            }
            out[at + 3] = (alpha * 255.0).round() as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{dot, Look};

    #[test]
    fn waiting_wins_over_running() {
        assert_eq!(Look::of(0, 0), Look::Idle);
        assert_eq!(Look::of(3, 0), Look::Running);
        assert_eq!(Look::of(3, 1), Look::Waiting);
        assert_eq!(Look::of(0, 1), Look::Waiting);
    }

    /// The corner gets the colour, the opposite corner is left as it was, and
    /// the dot's edge is white.
    #[test]
    fn paints_a_ringed_dot_in_the_corner() {
        let size = 32;
        let base = vec![10; (size * size * 4) as usize];
        let out = dot(&base, size, size, [0x3b, 0x82, 0xf6], 0.46);
        let pixel = |x: u32, y: u32| {
            let at = ((y * size + x) * 4) as usize;
            [out[at], out[at + 1], out[at + 2], out[at + 3]]
        };

        // The dot's centre, `outer` in from both edges.
        let centre = (size as f32 - size as f32 * 0.23) as u32;
        assert_eq!(pixel(centre, centre), [0x3b, 0x82, 0xf6, 255]);
        assert_eq!(pixel(0, 0), [10, 10, 10, 10]);
        // Just inside the dot's left edge, on its horizontal axis: the ring.
        let edge = (size as f32 - size as f32 * 0.46 + 1.0) as u32;
        let [r, g, b, a] = pixel(edge, centre);
        assert!(r > 200 && g > 200 && b > 200 && a == 255, "{:?}", [r, g, b, a]);
    }
}
