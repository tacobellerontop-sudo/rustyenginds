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
    GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, FALSE, HWND, LPARAM, RECT, TRUE,
};
use windows_sys::Win32::System::Console::FreeConsole;
use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
    INFINITE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetForegroundWindow, GetSystemMetrics,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsZoomed, SendMessageTimeoutW, SetParent,
    SetWindowLongPtrW, SetWindowPos, SystemParametersInfoW, GWL_STYLE, SMTO_NORMAL, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SPI_GETDESKWALLPAPER, SPI_SETDESKWALLPAPER, SWP_NOACTIVATE, SWP_NOZORDER,
    SWP_SHOWWINDOW, WS_CHILD, WS_POPUP,
};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::platform::windows::WindowAttributesExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowAttributes};

/// Repaints the normal wallpaper when we exit; otherwise our last frame
/// stays stuck on the desktop until Explorer redraws.
pub struct DesktopGuard;

impl Drop for DesktopGuard {
    fn drop(&mut self) {
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
    super::covering_attributes(monitor).with_skip_taskbar(true)
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
    let worker = find_worker_w()?;
    let pos = monitor.position();
    let size = monitor.size();
    unsafe {
        // Child windows use the parent's client coordinates, and WorkerW spans
        // the whole virtual screen, whose origin can be negative.
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (style & !(WS_POPUP as isize)) | WS_CHILD as isize,
        );
        if SetParent(hwnd, worker).is_null() {
            bail!("SetParent onto the desktop WorkerW failed");
        }
        SetWindowPos(
            hwnd,
            null_mut(),
            pos.x - vx,
            pos.y - vy,
            size.width as i32,
            size.height as i32,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    Ok(DesktopGuard)
}

fn find_worker_w() -> Result<HWND> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), null());
        if progman.is_null() {
            bail!("couldn't find Progman; is Explorer running?");
        }
        let mut result = 0usize;
        SendMessageTimeoutW(progman, 0x052C, 0xD, 0x1, SMTO_NORMAL, 1000, &mut result);
        SendMessageTimeoutW(progman, 0x052C, 0, 0, SMTO_NORMAL, 1000, &mut result);

        // Classic layout: the WorkerW we want is the top-level sibling right
        // after the window that hosts SHELLDLL_DefView.
        let mut worker: HWND = null_mut();
        EnumWindows(Some(find_worker_proc), &mut worker as *mut HWND as LPARAM);

        // Windows 11 24H2+: WorkerW became a child of Progman.
        if worker.is_null() {
            worker = FindWindowExW(progman, null_mut(), w!("WorkerW"), null());
        }
        if worker.is_null() {
            bail!("couldn't find the desktop WorkerW window");
        }
        Ok(worker)
    }
}

unsafe extern "system" fn find_worker_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let shell = FindWindowExW(hwnd, null_mut(), w!("SHELLDLL_DefView"), null());
    if !shell.is_null() {
        let out = lparam as *mut HWND;
        *out = FindWindowExW(null_mut(), hwnd, w!("WorkerW"), null());
    }
    TRUE
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
        if SetEvent(event) == 0 {
            bail!("SetEvent failed (error {})", GetLastError());
        }
    }
    Ok(true)
}

/// Close the console window we were started with (for `--background`).
pub fn detach_console() {
    unsafe {
        FreeConsole();
    }
}

const RUN_KEY: windows_sys::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: windows_sys::core::PCWSTR = w!("rusty-wallpaper");

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
