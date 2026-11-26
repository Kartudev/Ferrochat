//! Notification-area (system tray) icon on Windows, straight on the Win32
//! shell API (`Shell_NotifyIconW`): no GUI toolkit, no new crate.
//!
//! The icon lives on its own thread with a hidden window and a message loop.
//! Left click toggles the window, right click opens the same menu as Linux
//! ([`TrayModel::menu`]). Model changes from [`super::publish`] are stored
//! and the tray thread is poked with a private window message. When Explorer
//! restarts (taskbar recreated) the icon is added again.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON,
    LR_DEFAULTCOLOR, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WM_APP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};

use super::{Attention, MenuEntry, TrayCmd, TrayModel};
use crate::ext::{self, External};
use crate::icon;

/// Shell callback for clicks on the icon.
const WM_TRAY: u32 = WM_APP + 1;
/// "The model changed": re-read [`latest`] and refresh tip and icon.
const WM_REFRESH: u32 = WM_APP + 2;
/// Our icon's id within the window.
const ICON_ID: u32 = 1;

/// The tray window (an `HWND` as an integer; 0 = none).
static HWND_VALUE: AtomicIsize = AtomicIsize::new(0);

fn latest() -> &'static Mutex<TrayModel> {
    static M: OnceLock<Mutex<TrayModel>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(TrayModel::default()))
}

fn current_model() -> TrayModel {
    latest().lock().map(|m| m.clone()).unwrap_or_default()
}

/// What the tray thread owns.
struct State {
    hwnd: HWND,
    normal: HICON,
    attention: HICON,
    taskbar_created: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An `HICON` from the app's SVG, as a PNG icon resource (Vista+).
fn make_icon(svg: &[u8], size: u32) -> HICON {
    let Some(png) = icon::png(svg, size) else {
        return std::ptr::null_mut();
    };
    let Ok(len) = u32::try_from(png.len()) else {
        return std::ptr::null_mut();
    };
    let side = i32::try_from(size).unwrap_or(16);
    // SAFETY: `png` is a complete PNG image of `len` bytes that outlives the call.
    unsafe {
        CreateIconFromResourceEx(
            png.as_ptr(),
            len,
            1,
            0x0003_0000,
            side,
            side,
            LR_DEFAULTCOLOR,
        )
    }
}

fn icon_data(state: &State, model: &TrayModel) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW is plain data; all-zero is its documented empty value.
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = state.hwnd;
    nid.uID = ICON_ID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = match model.attention() {
        Attention::Idle => state.normal,
        Attention::NeedsAttention => state.attention,
    };
    let tip: Vec<u16> = model.tooltip().encode_utf16().take(127).collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    nid
}

fn notify(op: u32, state: &State) -> bool {
    let nid = icon_data(state, &current_model());
    // SAFETY: `nid` is a fully initialised NOTIFYICONDATAW for our window.
    unsafe { Shell_NotifyIconW(op, &nid) != 0 }
}

fn send(cmd: TrayCmd) {
    ext::send(External::Tray(cmd));
}

/// Shows the context menu at the cursor and runs the picked entry.
fn show_menu(hwnd: HWND) {
    let entries = current_model().menu();
    // SAFETY: plain Win32 menu calls on a menu we create and destroy here;
    // the label buffers live until TrackPopupMenu returns.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let mut labels = Vec::new();
        let mut cmds = Vec::new();
        for e in &entries {
            match e {
                MenuEntry::Separator => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                }
                MenuEntry::Item { label, cmd } | MenuEntry::Check { label, cmd, .. } => {
                    let checked = matches!(e, MenuEntry::Check { checked: true, .. });
                    let flags = MF_STRING | if checked { MF_CHECKED } else { 0 };
                    labels.push(wide(label));
                    cmds.push(*cmd);
                    let id = cmds.len();
                    if let Some(l) = labels.last() {
                        AppendMenuW(menu, flags, id, l.as_ptr());
                    }
                }
            }
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Without this the menu does not close when clicking elsewhere.
        SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        if let Ok(i) = usize::try_from(picked)
            && i > 0
            && let Some(cmd) = cmds.get(i - 1)
        {
            send(*cmd);
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => {
            // Legacy (version 0) callback: the mouse message is in lParam.
            match lparam as u32 {
                WM_LBUTTONUP => send(TrayCmd::Toggle),
                WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_REFRESH => {
            STATE.with(|s| {
                if let Some(state) = s.borrow().as_ref() {
                    notify(NIM_MODIFY, state);
                }
            });
            0
        }
        _ => {
            let recreated = STATE.with(|s| {
                s.borrow().as_ref().and_then(|state| {
                    (state.taskbar_created != 0 && msg == state.taskbar_created)
                        .then(|| notify(NIM_ADD, state))
                })
            });
            if recreated.is_some() {
                return 0;
            }
            // SAFETY: default handling for every other message.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// Creates the window and the icon on this thread. `None` when the shell
/// refuses the icon (no taskbar).
fn create() -> Option<()> {
    let class = wide("FerrochatTrayWindow");
    // SAFETY: Win32 window creation with static, NUL-terminated strings; the
    // class name buffer outlives the window.
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: class.as_ptr(),
            // SAFETY: the remaining fields are optional and zero means "none".
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let title = wide("Ferrochat tray");
        // A hidden top-level window (not message-only): the menu needs one
        // that can take the foreground.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return None;
        }
        let size = u32::try_from(GetSystemMetrics(SM_CXSMICON))
            .ok()
            .filter(|&s| s > 0)
            .unwrap_or(16);
        let taskbar = wide("TaskbarCreated");
        let state = State {
            hwnd,
            normal: make_icon(icon::NORMAL_SVG, size),
            attention: make_icon(icon::ATTENTION_SVG, size),
            taskbar_created: RegisterWindowMessageW(taskbar.as_ptr()),
        };
        if !notify(NIM_ADD, &state) {
            DestroyIcon(state.normal);
            DestroyIcon(state.attention);
            return None;
        }
        HWND_VALUE.store(hwnd as isize, Ordering::SeqCst);
        STATE.with(|s| *s.borrow_mut() = Some(state));
        Some(())
    }
}

/// Adds the tray icon. Returns `false` when the shell has no notification
/// area (the app then must not hide to the tray). Keeps the icon in sync
/// with [`super::publish`].
pub async fn start(initial: TrayModel) -> bool {
    if let Ok(mut m) = latest().lock() {
        *m = initial;
    }
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            let ok = create().is_some();
            let _ = tx.send(ok);
            if !ok {
                return;
            }
            // SAFETY: a standard message loop for this thread's windows.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        });
    if spawned.is_err() {
        return false;
    }
    let up = rx.await.unwrap_or(false);
    if !up {
        tracing::info!("no notification area available");
        return false;
    }
    let mut changes = super::subscribe();
    tokio::spawn(async move {
        loop {
            let model = changes.borrow_and_update().clone();
            if let Ok(mut m) = latest().lock() {
                *m = model;
            }
            let hwnd = HWND_VALUE.load(Ordering::SeqCst);
            if hwnd != 0 {
                // SAFETY: posting to our own window; a stale handle just fails.
                unsafe { PostMessageW(hwnd as HWND, WM_REFRESH, 0, 0) };
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
    true
}

/// Removes the icon (at quit), so no dead icon stays in the notification area.
pub fn shutdown() {
    let hwnd = HWND_VALUE.swap(0, Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    // SAFETY: NIM_DELETE only needs our window and icon id.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd as HWND;
        nid.uID = ICON_ID;
        Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adds, updates and removes the icon. A desktop without a taskbar
    /// (some CI sessions) refuses the icon: then only `false` is checked.
    #[tokio::test]
    async fn icon_round_trip() {
        let model = TrayModel {
            unread_chats: 2,
            ..Default::default()
        };
        if !start(model.clone()).await {
            eprintln!("skipped: no notification area in this session");
            return;
        }
        assert_ne!(HWND_VALUE.load(Ordering::SeqCst), 0);
        super::super::publish(TrayModel::default());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(current_model().unread_chats, 0);
        shutdown();
        assert_eq!(HWND_VALUE.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn icons_render_for_the_shell() {
        assert!(icon::png(icon::NORMAL_SVG, 16).is_some());
        assert!(icon::png(icon::ATTENTION_SVG, 32).is_some());
    }
}
