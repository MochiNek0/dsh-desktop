//! Where the window was, so the next launch opens it there.
//!
//! This was tried once before and taken out: the window was built at the
//! default size and moved to the saved one afterwards, in front of the user.
//! What is different now is that [`crate::build_window`] builds the window
//! hidden anyway — for the background colour — so there is a moment between
//! the window existing and the window being on screen, and that is where
//! [`restore`] puts it. Nothing here moves a window anyone can see.
//!
//! ## What is saved
//!
//! `GetWindowPlacement`'s normal rectangle and whether the window is
//! maximised, rather than the position and size Tauri reports. The normal
//! rectangle is the one the window goes back to when it is restored, and
//! Windows keeps it while the window is maximised or minimised — a position
//! read off a maximised window is the monitor's corner, and remembering that
//! would open the next launch maximised-looking but unmaximised. It is also in
//! the same coordinates `SetWindowPlacement` takes back, so nothing in between
//! has to know about taskbars or scale factors.
//!
//! It is saved when the window goes to the tray and when the app quits, which
//! between them are every way the window stops being looked at. Not on every
//! move: that is a file write per frame of a drag.
//!
//! ## Windows only
//!
//! Every release is the NSIS installer. Elsewhere the window opens centred at
//! the default size, as it always has.

use serde_json::{json, Value};
use tauri::{AppHandle, WebviewWindow};

/// The window's normal rectangle and whether it was maximised.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Placement {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    maximized: bool,
}

impl Placement {
    fn to_json(self) -> Value {
        json!({
            "left": self.left,
            "top": self.top,
            "right": self.right,
            "bottom": self.bottom,
            "maximized": self.maximized,
        })
    }

    /// `None` for anything that is not a rectangle with some area to it —
    /// a file a person edited, or one a later build wrote differently.
    fn from_json(value: &Value) -> Option<Self> {
        let side = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok())
        };
        let placement = Self {
            left: side("left")?,
            top: side("top")?,
            right: side("right")?,
            bottom: side("bottom")?,
            maximized: value
                .get("maximized")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        (placement.right > placement.left && placement.bottom > placement.top).then_some(placement)
    }

    fn center(self) -> (i32, i32) {
        (
            self.left + (self.right - self.left) / 2,
            self.top + (self.bottom - self.top) / 2,
        )
    }
}

/// Put the window where it was last time. Call it while the window is still
/// hidden; see the module docs.
///
/// Nothing happens on a first launch, on a file that holds no usable
/// rectangle, or when the middle of the saved one is on no monitor — the
/// monitor it was on has been unplugged, or the layout changed. Those keep the
/// default the builder gave the window.
pub(crate) fn restore(app: &AppHandle, window: &WebviewWindow) {
    let Some(placement) = crate::settings::window_placement(app)
        .as_ref()
        .and_then(Placement::from_json)
    else {
        return;
    };
    platform::restore(window, placement);
}

/// Remember where the window is now.
pub(crate) fn save(app: &AppHandle, window: &WebviewWindow) {
    let Some(placement) = platform::read(window) else {
        return;
    };
    let unchanged = crate::settings::window_placement(app)
        .as_ref()
        .and_then(Placement::from_json)
        == Some(placement);
    if !unchanged {
        crate::settings::set_window_placement(app, placement.to_json());
    }
}

#[cfg(windows)]
mod platform {
    use super::Placement;
    use tauri::WebviewWindow;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, IsIconic, IsZoomed, SetWindowPlacement, SW_HIDE, WINDOWPLACEMENT,
        WPF_RESTORETOMAXIMIZED,
    };

    pub(super) fn read(window: &WebviewWindow) -> Option<Placement> {
        let hwnd = window.hwnd().ok()?;
        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        // SAFETY: a live window handle and a correctly sized struct, which is
        // all the call asks for.
        unsafe { GetWindowPlacement(hwnd, &mut placement) }.ok()?;
        let rect = placement.rcNormalPosition;
        // Minimised counts as whatever it would come back as.
        let maximized = unsafe {
            IsZoomed(hwnd).as_bool()
                || (IsIconic(hwnd).as_bool() && placement.flags.contains(WPF_RESTORETOMAXIMIZED))
        };
        Some(Placement {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
            maximized,
        })
    }

    pub(super) fn restore(window: &WebviewWindow, saved: Placement) {
        let (x, y) = saved.center();
        // SAFETY: no handle involved; a null answer is the "nowhere" this asks
        // about. The rectangle is in workspace coordinates and the monitor
        // lookup is in screen ones; they differ by a taskbar along the top or
        // left edge, which does not decide whether a window's middle is on a
        // monitor.
        if unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) }.is_invalid() {
            return;
        }

        let Ok(hwnd) = window.hwnd() else {
            return;
        };
        let placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            // Stays hidden: this only sets where it will be when it is shown.
            showCmd: SW_HIDE.0 as u32,
            rcNormalPosition: RECT {
                left: saved.left,
                top: saved.top,
                right: saved.right,
                bottom: saved.bottom,
            },
            ..Default::default()
        };
        // Onto a monitor with another scale factor as well: the rectangle
        // comes back exactly as saved (checked 100% to 150%).
        // SAFETY: as in `read`.
        let _ = unsafe { SetWindowPlacement(hwnd, &placement) };

        if saved.maximized {
            // Tauri maximises a hidden window by showing it maximised and then
            // hiding it again, and a window shown for that long can make it to
            // the screen. Cloaked, the compositor draws nothing of it at all.
            cloak(hwnd, true);
            let _ = window.maximize();
            cloak(hwnd, false);
        }
    }

    fn cloak(hwnd: HWND, on: bool) {
        let value = BOOL::from(on);
        // SAFETY: a live handle, and a BOOL of the size the attribute takes.
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_CLOAK,
                &value as *const BOOL as *const _,
                std::mem::size_of::<BOOL>() as u32,
            )
        };
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Placement;
    use tauri::WebviewWindow;

    pub(super) fn read(_window: &WebviewWindow) -> Option<Placement> {
        None
    }

    pub(super) fn restore(_window: &WebviewWindow, _saved: Placement) {}
}

#[cfg(test)]
mod tests {
    use super::Placement;
    use serde_json::json;

    #[test]
    fn round_trips() {
        let placement = Placement {
            left: -1700,
            top: 40,
            right: -200,
            bottom: 980,
            maximized: true,
        };
        assert_eq!(Placement::from_json(&placement.to_json()), Some(placement));
    }

    #[test]
    fn refuses_what_is_not_a_rectangle() {
        assert_eq!(Placement::from_json(&json!(null)), None);
        assert_eq!(Placement::from_json(&json!({ "left": 0, "top": 0 })), None);
        assert_eq!(
            Placement::from_json(&json!({ "left": 10, "top": 0, "right": 10, "bottom": 90 })),
            None
        );
        assert_eq!(
            Placement::from_json(&json!({ "left": 0, "top": 0, "right": 1e12, "bottom": 90 })),
            None
        );
    }

    #[test]
    fn a_missing_flag_is_not_maximised() {
        let placement =
            Placement::from_json(&json!({ "left": 0, "top": 0, "right": 800, "bottom": 600 }));
        assert_eq!(placement.map(|p| p.maximized), Some(false));
    }
}
