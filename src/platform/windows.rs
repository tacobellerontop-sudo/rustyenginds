//! Windows: the well-known "WorkerW" trick.
//!
//! Sending the undocumented message `0x052C` to `Progman` makes Explorer
//! split the desktop into the icon layer (`SHELLDLL_DefView`) and a separate
//! `WorkerW` window behind it. Re-parenting our window into that `WorkerW`
//! puts it between the static wallpaper and the icons.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use windows_sys::core::BOOL;
use windows_sys::w;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, FALSE, HWND, LPARAM, POINT, RECT,
    TRUE,
};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
    INFINITE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetForegroundWindow, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, IsZoomed,
    SendMessageTimeoutW, SetLayeredWindowAttributes, SetParent, SetWindowLongPtrW, SetWindowPos,
    SystemParametersInfoW, GWL_EXSTYLE, GWL_STYLE, GW_CHILD, GW_HWNDNEXT, HWND_BOTTOM, LWA_ALPHA,
    SMTO_NORMAL, SPI_GETDESKWALLPAPER, SPI_SETDESKWALLPAPER, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, WS_CHILD, WS_EX_LAYERED, WS_EX_NOREDIRECTIONBITMAP, WS_POPUP,
};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::platform::windows::WindowAttributesExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowAttributes};

/// Repaints the normal wallpaper when we exit; otherwise our last frame
/// stays stuck on the desktop until Explorer redraws.
pub struct DesktopGuard {
    raised: bool,
}

impl Drop for DesktopGuard {
    fn drop(&mut self) {
        // On the raised desktop, resetting the wallpaper destroys the WorkerW
        // Explorer just created (Lively skips it for the same reason), and the
        // shell repaints by itself once our child window is gone.
        if self.raised {
            return;
        }
        let mut path = [0u16; 260];
        unsafe {
            if SystemParametersInfoW(
                SPI_GETDESKWALLPAPER,
                path.len() as u32,
                path.as_mut_ptr().cast(),
                0,
            ) != 0
            {
                SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, path.as_mut_ptr().cast(), 0);
            }
        }
    }
}

pub fn desktop_attributes(_el: &ActiveEventLoop, monitor: &MonitorHandle) -> WindowAttributes {
    // Hidden until it sits behind the icons, so nothing flashes on screen.
    super::covering_attributes(monitor)
        .with_skip_taskbar(true)
        .with_visible(false)
}

/// The shell windows we slot ourselves between.
struct DesktopLayer {
    progman: HWND,
    /// Draws the static wallpaper.
    worker: HWND,
    /// Hosts the desktop icons.
    defview: HWND,
    /// Windows 11 24H2+ "raised desktop": Progman is composed with
    /// DirectComposition, and DefView and WorkerW are its children.
    raised: bool,
}

/// Find (and if needed, ask Explorer to create) the desktop windows.
/// Mirrors Lively's `WinDesktopCore.SetupDesktopLayer`.
fn desktop_layer() -> Result<DesktopLayer> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), null());
        if progman.is_null() {
            bail!("couldn't find Progman; is Explorer running?");
        }
        let raised = GetWindowLongPtrW(progman, GWL_EXSTYLE) & WS_EX_NOREDIRECTIONBITMAP as isize != 0;

        // Makes Progman split the wallpaper into a WorkerW behind the icons.
        let mut result = 0usize;
        SendMessageTimeoutW(progman, 0x052C, 0xD, 0x1, SMTO_NORMAL, 1000, &mut result);

        let mut layer = DesktopLayer {
            progman,
            worker: null_mut(),
            defview: null_mut(),
            raised,
        };
        if raised {
            // Progman
            //   SHELLDLL_DefView   <- icons
            //   WorkerW            <- wallpaper; we go between these two
            layer.defview = FindWindowExW(progman, null_mut(), w!("SHELLDLL_DefView"), null());
            layer.worker = FindWindowExW(progman, null_mut(), w!("WorkerW"), null());
        } else {
            // WorkerW              <- hosts SHELLDLL_DefView (icons)
            // WorkerW              <- wallpaper; we become its child
            // Progman
            EnumWindows(Some(find_worker_proc), &mut layer as *mut DesktopLayer as LPARAM);
        }
        log::info!(
            "desktop layout: {} (Progman {progman:?}, WorkerW {:?}, DefView {:?})",
            if raised {
                "raised, Windows 11 24H2+"
            } else {
                "classic"
            },
            layer.worker,
            layer.defview,
        );
        if layer.worker.is_null() {
            bail!("couldn't find the desktop WorkerW window");
        }
        if raised && layer.defview.is_null() {
            bail!("couldn't find the desktop icon layer (SHELLDLL_DefView)");
        }
        Ok(layer)
    }
}

unsafe extern "system" fn find_worker_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let defview = FindWindowExW(hwnd, null_mut(), w!("SHELLDLL_DefView"), null());
    if !defview.is_null() {
        let layer = &mut *(lparam as *mut DesktopLayer);
        layer.defview = defview;
        layer.worker = FindWindowExW(null_mut(), hwnd, w!("WorkerW"), null());
    }
    TRUE
}

pub fn attach_to_desktop(
    _el: &ActiveEventLoop,
    window: &Window,
    monitor: &MonitorHandle,
) -> Result<DesktopGuard> {
    let hwnd = match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(h) => h.hwnd.get() as HWND,
        _ => bail!("expected a Win32 window handle"),
    };
    let layer = desktop_layer()?;
    let (pos, size) = (monitor.position(), monitor.size());
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (style & !(WS_POPUP as isize)) | WS_CHILD as isize,
        );

        let parent = if layer.raised {
            // Children of the DirectComposition-backed Progman only show up
            // when layered. This must happen before SetParent.
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED as isize);
            SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
            layer.progman
        } else {
            layer.worker
        };
        if SetParent(hwnd, parent).is_null() {
            bail!("SetParent onto the desktop failed (error {})", GetLastError());
        }

        // Child windows are positioned in the parent's client coordinates.
        let mut origin = POINT { x: pos.x, y: pos.y };
        ScreenToClient(parent, &mut origin);
        let (insert_after, zflag) = if layer.raised {
            (layer.defview, 0) // directly beneath the icons
        } else {
            (null_mut(), SWP_NOZORDER)
        };
        SetWindowPos(
            hwnd,
            insert_after,
            origin.x,
            origin.y,
            size.width as i32,
            size.height as i32,
            SWP_NOACTIVATE | SWP_SHOWWINDOW | zflag,
        );
        if layer.raised {
            // The static wallpaper must stay underneath us.
            SetWindowPos(
                layer.worker,
                HWND_BOTTOM,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        log::info!(
            "attached to the desktop at ({}, {}) {}x{}",
            origin.x,
            origin.y,
            size.width,
            size.height
        );
    }
    log::info!("desktop windows after attaching:\n{}", desktop_report());
    Ok(DesktopGuard { raised: layer.raised })
}

/// True when the foreground window is fullscreen or maximized on `monitor`,
/// i.e. nobody can see the wallpaper there, so rendering it is wasted work.
pub fn desktop_covered(monitor: &MonitorHandle) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(fg, &mut pid);
        if pid == GetCurrentProcessId() {
            return false;
        }
        // Clicking the desktop or taskbar focuses one of these shell windows.
        let mut class = [0u16; 64];
        let len = GetClassNameW(fg, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return false;
        }
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(fg, &mut r) == 0 {
            return false;
        }
        let (pos, size) = (monitor.position(), monitor.size());
        let (ml, mt) = (pos.x, pos.y);
        let (mr, mb) = (ml + size.width as i32, mt + size.height as i32);
        let fullscreen = r.left <= ml && r.top <= mt && r.right >= mr && r.bottom >= mb;
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let on_monitor = cx >= ml && cx < mr && cy >= mt && cy < mb;
        fullscreen || (IsZoomed(fg) != 0 && on_monitor)
    }
}

const STOP_EVENT: windows_sys::core::PCWSTR = w!("Local\\rusty-wallpaper-stop");

/// Claim the single-instance lock: a named event that `rusty-wallpaper stop`
/// signals. A listener thread turns that signal into `quit`.
pub fn single_instance(quit: Arc<AtomicBool>) -> Result<()> {
    unsafe {
        let event = CreateEventW(null(), TRUE, FALSE, STOP_EVENT);
        if event.is_null() {
            bail!("CreateEventW failed (error {})", GetLastError());
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            bail!("rusty-wallpaper is already running (use `rusty-wallpaper stop` first)");
        }
        // Raw handles aren't Send; the event lives for the whole process anyway.
        let event = event as usize;
        std::thread::spawn(move || {
            WaitForSingleObject(event as _, INFINITE);
            quit.store(true, Ordering::SeqCst);
        });
    }
    Ok(())
}

/// Ask a running instance to quit. Returns false if none is running.
pub fn signal_stop() -> Result<bool> {
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, FALSE, STOP_EVENT);
        if event.is_null() {
            return Ok(false);
        }
        let ok = SetEvent(event);
        CloseHandle(event);
        if ok == 0 {
            bail!("SetEvent failed (error {})", GetLastError());
        }
    }
    Ok(true)
}

/// Whether a desktop instance currently holds the single-instance lock.
pub fn is_running() -> bool {
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, FALSE, STOP_EVENT);
        if event.is_null() {
            return false;
        }
        CloseHandle(event);
        true
    }
}

/// True when we own our console, i.e. we were started by double-clicking
/// the exe rather than from a terminal.
pub fn launched_from_explorer() -> bool {
    let mut pids = [0u32; 2];
    unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) == 1 }
}

/// Start `cmd` without flashing a console window.
pub fn hide_console_window(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Close the console window we were started with (for `--background`).
pub fn detach_console() {
    unsafe {
        FreeConsole();
    }
}

const RUN_KEY: windows_sys::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: windows_sys::core::PCWSTR = w!("rusty-wallpaper");

pub fn autostart_enabled() -> bool {
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            null_mut(),
        ) == 0
    }
}

/// Add or remove ourselves from `HKCU\...\Run` so we start at login.
pub fn autostart(enable: bool, args: &str) -> Result<()> {
    unsafe {
        let status = if enable {
            let exe = std::env::current_exe().context("finding our own executable")?;
            let command = format!("\"{}\" {args}", exe.display());
            let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ,
                wide.as_ptr().cast(),
                (wide.len() * 2) as u32,
            )
        } else {
            match RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE) {
                ERROR_FILE_NOT_FOUND => 0,
                other => other,
            }
        };
        if status != 0 {
            bail!("updating the registry Run key failed (error {status})");
        }
    }
    Ok(())
}

/// A dump of the shell's desktop windows, for `rusty-wallpaper desktop-info`
/// and the log: which layout we detected and where our window ended up.
pub fn desktop_report() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    match desktop_layer() {
        Ok(l) => {
            let _ = writeln!(
                out,
                "layout: {}\nProgman {:?}  WorkerW {:?}  DefView {:?}",
                if l.raised {
                    "raised (Windows 11 24H2+)"
                } else {
                    "classic"
                },
                l.progman,
                l.worker,
                l.defview
            );
        }
        Err(e) => {
            let _ = writeln!(out, "layout: error: {e:#}");
        }
    }
    let mut tops: Vec<HWND> = Vec::new();
    unsafe {
        EnumWindows(Some(collect_proc), &mut tops as *mut Vec<HWND> as LPARAM);
    }
    for top in tops {
        let class = class_name(top);
        if class != "Progman" && class != "WorkerW" {
            continue;
        }
        let _ = writeln!(out, "{}", describe(top, 0));
        unsafe {
            let mut child = GetWindow(top, GW_CHILD);
            while !child.is_null() {
                let _ = writeln!(out, "{}", describe(child, 1));
                child = GetWindow(child, GW_HWNDNEXT);
            }
        }
    }
    out
}

unsafe extern "system" fn collect_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    (*(lparam as *mut Vec<HWND>)).push(hwnd);
    TRUE
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..len])
}

fn describe(hwnd: HWND, depth: usize) -> String {
    unsafe {
        let mut title = [0u16; 128];
        let tlen = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
        let mut r: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut r);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        format!(
            "{}{hwnd:?} {:?} {:?} visible={} rect=({},{})-({},{}) style={:#x} exstyle={:#x}{}",
            "  ".repeat(depth),
            class_name(hwnd),
            String::from_utf16_lossy(&title[..tlen]),
            IsWindowVisible(hwnd) != 0,
            r.left,
            r.top,
            r.right,
            r.bottom,
            GetWindowLongPtrW(hwnd, GWL_STYLE),
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE),
            if pid == GetCurrentProcessId() {
                "  <- rusty-wallpaper"
            } else {
                ""
            },
        )
    }
}
