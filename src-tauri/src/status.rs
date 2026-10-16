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
//!
//! ## Which sessions, in the tray menu
//!
//! The counts say *that* something is running or waiting; the menu says
//! *which*. The plugin sends a few sessions by id and title with every count —
//! waiting ones first — and the tray lists them above the recent workspaces,
//! where a click goes to that session through the same door a click on a
//! notification does ([`crate::signal::open`]). A forged row is a menu item
//! that switches to some session, which the page could already do.

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

/// How many sessions the tray lists, and how many of a list off the URL are
/// read. The plugin sends this many; see `LISTED` there.
pub const LIMIT: usize = 5;

/// One session the tray lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub session: String,
    /// dsh's `displayTitle`, which may be empty for a session dsh has not
    /// titled yet.
    pub title: String,
    /// Stopped on the user, as opposed to getting on with it.
    pub waiting: bool,
}

static ROWS: Mutex<Vec<Row>> = Mutex::new(Vec::new());

/// The plugin's latest counts and rows. Repaints the icons only when the look
/// changes, and the menu only when the rows do.
pub fn set(app: &AppHandle, running: u32, waiting: u32, rows: Vec<Row>) {
    // Before the early return below: the look does not change between one
    // running turn and two, and neither does whether to stay awake, but the
    // hold is its own to compare.
    crate::awake::running(app, running);

    if let Ok(mut current) = ROWS.lock() {
        if *current != rows {
            let labels: Vec<_> = rows.iter().map(label).collect();
            eprintln!("dsh-desktop: the tray lists {labels:?}");
            *current = rows;
            drop(current);
            crate::refresh_tray(app);
        }
    }

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
    set(app, 0, 0, Vec::new());
}

/// Repaint the icons from whatever was last set.
///
/// Called when the window comes back from the tray: Windows does not apply a
/// taskbar overlay set while the window is hidden, so the dot that was there
/// when the window was parked is gone by the time it comes back. Repainting
/// unconditionally is cheap — one icon and one overlay — and the alternative
/// is a window that looks idle while a turn is running.
pub fn repaint(app: &AppHandle) {
    let look = LOOK.lock().map(|l| *l).unwrap_or(Look::Idle);
    paint(app, look);
}

/// What the tray lists now.
pub fn rows() -> Vec<Row> {
    ROWS.lock().map(|rows| rows.clone()).unwrap_or_default()
}

/// A click on the row at `index`.
pub fn open(app: &AppHandle, index: usize) {
    let Some(row) = rows().into_iter().nth(index) else {
        return;
    };
    crate::reveal(app);
    crate::signal::open(app, &row.session);
}

/// A row as the menu shows it: what it is doing, then which session.
pub fn label(row: &Row) -> String {
    let title = if row.title.is_empty() {
        t!("未命名会话", "Untitled session").to_string()
    } else {
        crate::recent::label(&row.title)
    };
    if row.waiting {
        t!("在等你：{}", "Waiting for you: {}", title)
    } else {
        t!("正在运行：{}", "Running: {}", title)
    }
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
    // 16 pixels, whatever it is given. Filling all of it put a dot two thirds
    // the size of the 24-pixel icon on the button; this is 10 of the 16, in the
    // same corner.
    #[cfg(windows)]
    if let Some(window) = app.get_webview_window("main") {
        const SIZE: u32 = 32;
        let overlay = look.colour().map(|colour| {
            let blank = vec![0; (SIZE * SIZE * 4) as usize];
            Image::new_owned(dot(&blank, SIZE, SIZE, colour, 0.625), SIZE, SIZE)
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

    /// A row says what the session is doing, keeps a title short and its `&`
    /// literal, and has something to say about a session dsh has not titled.
    #[test]
    fn labels_a_row_by_what_it_is_doing() {
        use super::{label, Row};

        let row = |title: &str, waiting| Row {
            session: "s".into(),
            title: title.into(),
            waiting,
        };
        let waiting = label(&row("R&D", true));
        let running = label(&row("R&D", false));
        assert!(waiting.contains("R&&D") && running.contains("R&&D"));
        assert_ne!(waiting, running);
        assert!(label(&row("长".repeat(60).as_str(), false)).ends_with('…'));
        assert!(label(&row("", true)).chars().count() > label(&row("x", true)).chars().count());
    }

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
