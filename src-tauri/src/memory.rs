//! What the webview holds on to while nobody is looking at it.
//!
//! Closing the window parks this app in the tray rather than tearing the agent
//! down, so the ordinary shape of a long session is a `dsh web` working away
//! behind a window that is not on screen. The webview does not notice, and
//! Windows does not make it: parked for five minutes with nothing done about
//! it, WebView2's six processes held the same 436 MB of working set they held
//! with the window on screen — measured against a build with the two calls
//! below taken out, which is the only way to tell this apart from the trim the
//! OS is assumed to be doing and is not.
//!
//! WebView2 has a knob for exactly this: `MemoryUsageTargetLevel`, which is the
//! app telling the browser that its window is in the background. Set on the way
//! to the tray it takes that 436 MB to 30 MB inside two minutes, and a reveal
//! faults it back — 129 MB a few seconds in, the rest as it is touched.
//!
//! What moves is residency, not commit: the private bytes stay near 255 MB
//! either way, so this hands the RAM back to whatever else is running rather
//! than freeing the pages. On a machine that is not short of memory that is
//! most of what there is to win; the cost is paid at the reveal, out of the
//! pagefile.
//!
//! The important half is what the knob does *not* do. It is not `TrySuspend`,
//! which freezes the page — and a frozen page is no use here, because the whole
//! reason to sit in the tray is that [`crate::signal`] and [`crate::notify`]
//! are scripts running inside that page, watching the agent and raising the
//! toast that says a turn has ended. Those keep running at the low level: a
//! window revealed after two minutes parked came back with its session list
//! already reading two minutes older, which is the page's own clock, not a
//! repaint.
//!
//! The level is dropped on the two ways the window leaves the screen and put
//! back when it comes back, which is [`crate::reveal`] and nowhere else.
//!
//! Not covered: the login item's start, where the window is built hidden and
//! never shown. That is the same idle state and would benefit more than either
//! path here, but it is also a page still loading when the level would be set,
//! so it wants a test of its own rather than a line here.

use tauri::WebviewWindow;

/// The window has gone to the tray. Let the caches go.
pub(crate) fn trim(window: &WebviewWindow) {
    set(window, LOW);
}

/// The window is back on screen. Pay for the caches again.
pub(crate) fn restore(window: &WebviewWindow) {
    set(window, NORMAL);
}

/// `COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL` and `…_LOW`, which is all
/// this needs of that enum.
const NORMAL: i32 = 0;
const LOW: i32 = 1;

/// Windows only, and quietly: every step here is a thing the host may not have
/// — an older WebView2 runtime has no `ICoreWebView2_19` — and none of it is
/// worth a word to the user. A level that could not be set is a session that
/// holds what it held before, which is where this app was until now.
#[cfg(windows)]
fn set(window: &WebviewWindow, level: i32) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL,
    };
    use windows_core::Interface;

    let _ = window.with_webview(move |webview| {
        // SAFETY: the controller is the live one Tauri hands out for this
        // window, and both calls are documented entry points on it — the cast
        // is the documented way to ask an older runtime whether it has the
        // interface at all, and answers `Err` when it does not.
        unsafe {
            let Ok(core) = webview.controller().CoreWebView2() else {
                return;
            };
            let Ok(core) = core.cast::<ICoreWebView2_19>() else {
                return;
            };
            let _ = core.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL(level));
        }
    });
}

/// Everywhere else this is WebView2's own idea and there is nothing to set.
#[cfg(not(windows))]
fn set(_window: &WebviewWindow, _level: i32) {}
