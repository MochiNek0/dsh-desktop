//! The workspaces used last, for the tray menu and the Windows jump list.
//!
//! dsh knows the list and this app does not, so the plugin works it out — see
//! `watchRecent` in `plugin/lib/client.js` — and sends it here whenever it
//! changes. Kept for the rest of the run, a reload of dsh's page included: the
//! paths are no less valid while dsh restarts, and a click in the meantime
//! waits in [`crate::folder`] like any other folder.
//!
//! Not written to disk. Until the plugin first reports, the tray simply has no
//! recent rows; the jump list is the one copy that outlives the process, and
//! Windows keeps that one itself.
//!
//! A click opens the path through the same door `dsh-desktop <path>` does, and
//! a jump-list entry is literally that command line. The list arrives on the
//! channel anything in the window can navigate to, so a forged one is a menu
//! row that opens some folder as a workspace — which the page could already do
//! without the user's click.

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::AppHandle;

/// How many rows the tray and the jump list show, and how many of a list off
/// the URL are read. The plugin sends this many; see `RECENT` there.
pub const LIMIT: usize = 5;

/// How much of a title a menu row shows. A path-length title would make the
/// whole menu that wide.
const TITLE: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub title: String,
    pub path: String,
}

static RECENT: Mutex<Vec<Workspace>> = Mutex::new(Vec::new());

/// The plugin's latest list. Redraws the tray and the jump list only when it
/// differs from the one already shown.
pub fn set(app: &AppHandle, list: Vec<Workspace>) {
    let Ok(mut recent) = RECENT.lock() else { return };
    if *recent == list {
        return;
    }
    *recent = list.clone();
    drop(recent);

    crate::refresh_tray(app);
    #[cfg(windows)]
    jump::write(app, list);
}

/// Before the window is built; see `jump::claim`.
pub fn init(app: &AppHandle) {
    #[cfg(windows)]
    jump::claim(app);
    #[cfg(not(windows))]
    let _ = app;
}

/// What the tray shows now.
pub fn list() -> Vec<Workspace> {
    RECENT.lock().map(|recent| recent.clone()).unwrap_or_default()
}

/// A click on the row at `index`.
pub fn open(app: &AppHandle, index: usize) {
    let Some(workspace) = list().into_iter().nth(index) else {
        return;
    };
    crate::reveal(app);
    crate::folder::open(app, PathBuf::from(workspace.path));
}

/// A title as a menu row shows it: shortened, and with `&` doubled, because a
/// single one is a mnemonic marker to the menu on every platform.
pub fn label(title: &str) -> String {
    let short = match title.char_indices().nth(TITLE) {
        Some((cut, _)) => format!("{}…", &title[..cut]),
        None => title.to_string(),
    };
    short.replace('&', "&&")
}

/// The Windows jump list: the same rows on the taskbar button's right-click
/// menu, each a shortcut to `dsh-desktop.exe "<path>"`.
#[cfg(windows)]
mod jump {
    use std::sync::Mutex;

    use tauri::AppHandle;
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Storage::EnhancedStorage::PKEY_Title;
    use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
        SHStrDupW, ShellLink,
    };

    use super::Workspace;

    /// Put the window under the id the jump list is written for, before there
    /// is a window.
    ///
    /// A process started from the installed shortcut is under that shortcut's
    /// id already; one started any other way — the login item, or
    /// `dsh-desktop .` in a terminal — would be under one Windows derives from
    /// the exe path, and its taskbar button would show no jump list at all.
    pub fn claim(app: &AppHandle) {
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

        if let Some(id) = crate::toast::app_id(app) {
            if let Err(error) = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(id)) } {
                eprintln!("dsh-desktop: could not set the AppUserModelID: {error}");
            }
        }
    }

    /// One write at a time: two lists committed at once is a list Windows
    /// interleaves.
    static WRITING: Mutex<()> = Mutex::new(());

    /// Replace the list. On a thread of its own, because it is COM calls into
    /// the shell and the caller is the webview's navigation callback.
    pub fn write(app: &AppHandle, list: Vec<Workspace>) {
        let id = crate::toast::app_id(app);
        // `std`'s rather than tauri's: that one is canonicalized to `\\?\C:\…`, which
        // `IShellLinkW::SetPath` refuses as an invalid argument.
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        std::thread::spawn(move || {
            let Ok(_one) = WRITING.lock() else { return };
            unsafe {
                if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                    return;
                }
                if let Err(error) = commit(id.as_deref(), &exe, &list) {
                    eprintln!("dsh-desktop: could not write the jump list: {error}");
                }
                CoUninitialize();
            }
        });
    }

    unsafe fn commit(
        id: Option<&str>,
        exe: &std::path::Path,
        list: &[Workspace],
    ) -> windows::core::Result<()> {
        let destinations: ICustomDestinationList =
            CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
        // The same id the taskbar button is under; see `crate::toast::app_id`.
        if let Some(id) = id {
            destinations.SetAppID(&HSTRING::from(id)).map_err(at("SetAppID"))?;
        }

        let mut slots = 0;
        let _removed: IObjectArray = destinations.BeginList(&mut slots).map_err(at("BeginList"))?;

        if list.is_empty() {
            return destinations.CommitList();
        }

        let items: IObjectCollection =
            CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
        for workspace in list {
            items.AddObject(&link(exe, workspace).map_err(at("link"))?)?;
        }

        let category = HSTRING::from(t!("最近工作区", "Recent workspaces"));
        destinations.AppendCategory(&category, &items.cast::<IObjectArray>()?).map_err(at("AppendCategory"))?;
        destinations.CommitList().map_err(at("CommitList"))
    }

    fn at(step: &'static str) -> impl Fn(windows::core::Error) -> windows::core::Error {
        move |error| windows::core::Error::new(error.code(), format!("{step}: {}", error.message()))
    }

    /// One row: a shortcut to this exe with the path as its argument, titled
    /// with the workspace's name.
    unsafe fn link(exe: &std::path::Path, workspace: &Workspace) -> windows::core::Result<IShellLinkW> {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str())).map_err(at("SetPath"))?;
        link.SetArguments(&HSTRING::from(super::argument(&workspace.path))).map_err(at("SetArguments"))?;
        link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0).map_err(at("SetIconLocation"))?;
        link.SetDescription(&HSTRING::from(workspace.path.as_str())).map_err(at("SetDescription"))?;

        // A jump-list row is titled by `System.Title` and nothing else; a link
        // without one is not drawn. Built the way `InitPropVariantFromString`
        // does — a header-only helper, so not in the bindings — with the string
        // in COM's allocator, because `PropVariantClear` frees it there.
        let mut value = PROPVARIANT::default();
        (*value.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*value.Anonymous.Anonymous).Anonymous.pwszVal =
            SHStrDupW(&HSTRING::from(workspace.title.as_str()))?;
        let store: IPropertyStore = link.cast()?;
        let set = store.SetValue(&PKEY_Title, &value).map_err(at("SetValue"));
        let _ = PropVariantClear(&mut value);
        set?;
        store.Commit().map_err(at("Commit"))?;

        Ok(link)
    }
}

/// A path as one command-line argument, the way Windows' own parser splits
/// them: quoted, with a trailing backslash doubled so it does not escape the
/// closing quote. `C:\` would otherwise arrive as `C:"`.
#[cfg_attr(not(windows), allow(dead_code))]
fn argument(path: &str) -> String {
    let trailing = path.len() - path.trim_end_matches('\\').len();
    format!("\"{path}{}\"", "\\".repeat(trailing))
}

#[cfg(test)]
mod tests {
    use super::{argument, label};

    #[test]
    fn a_menu_row_shows_an_ampersand_and_a_short_title() {
        assert_eq!(label("R&D"), "R&&D");
        let long = "长".repeat(60);
        let shown = label(&long);
        assert_eq!(shown.chars().count(), 41);
        assert!(shown.ends_with('…'));
        assert_eq!(label("dsh-desktop"), "dsh-desktop");
    }

    #[test]
    fn a_path_is_one_argument_even_at_a_drive_root() {
        assert_eq!(argument(r"C:\work\my project"), r#""C:\work\my project""#);
        assert_eq!(argument(r"C:\"), r#""C:\\""#);
    }
}
