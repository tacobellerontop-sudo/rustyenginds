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
    SIZE, TRUE,
};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, ScreenToClient, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
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
    SystemParametersInfoW, UpdateLayeredWindow, GWL_EXSTYLE, GWL_STYLE, GW_CHILD, GW_HWNDNEXT, HWND_BOTTOM,
    HWND_TOP, HWND_TOPMOST, LWA_ALPHA, SMTO_NORMAL, SPI_GETDESKWALLPAPER, SPI_SETDESKWALLPAPER,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, ULW_ALPHA, WS_CAPTION, WS_CHILD,
    WS_EX_APPWINDOW, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_STATICEDGE, WS_EX_TOOLWINDOW, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX,
    WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
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
    /// Set when frames are pushed by us (DirectX, UpdateLayeredWindow)
    /// instead of painted through the window's DC by softbuffer.
    presenter: Option<Box<dyn FramePresenter>>,
}

trait FramePresenter {
    fn present(&mut self, pixels: &[u32], width: usize, height: usize) -> Result<()>;
}

impl DesktopGuard {
    /// Present a full-size frame ourselves, if this window needs it.
    /// `None` means "use the regular softbuffer path".
    pub fn present(&mut self, pixels: &[u32], width: usize, height: usize) -> Option<Result<()>> {
        self.presenter.as_mut().map(|p| p.present(pixels, width, height))
    }
}

/// How frames reach the window on the raised (24H2+) desktop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PresentMode {
    /// WS_EX_LAYERED + SetLayeredWindowAttributes, then normal GDI painting
    /// (what Microsoft's guidance describes).
    Gdi,
    /// WS_EX_LAYERED + UpdateLayeredWindow with a bitmap each frame.
    UpdateLayered,
    /// WS_EX_LAYERED + SetLayeredWindowAttributes, frames presented through
    /// a Direct3D 11 swap chain (what Lively's players effectively do).
    DirectX,
}

fn present_mode() -> PresentMode {
    match std::env::var("RUSTY_WALLPAPER_PRESENT").as_deref() {
        Ok("ulw") => PresentMode::UpdateLayered,
        Ok("dx") => PresentMode::DirectX,
        _ => PresentMode::Gdi,
    }
}

/// A 32-bit top-down DIB that frames are copied into and handed to
/// UpdateLayeredWindow.
struct LayeredPresenter {
    hwnd: HWND,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    size: (usize, usize),
}

impl FramePresenter for LayeredPresenter {
    fn present(&mut self, pixels: &[u32], w: usize, h: usize) -> Result<()> {
        LayeredPresenter::present(self, pixels, w, h)
    }
}

impl LayeredPresenter {
    fn new(hwnd: HWND) -> Result<Self> {
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        if dc.is_null() {
            bail!("CreateCompatibleDC failed");
        }
        Ok(Self {
            hwnd,
            dc,
            bitmap: null_mut(),
            old: null_mut(),
            bits: null_mut(),
            size: (0, 0),
        })
    }

    fn resize(&mut self, w: usize, h: usize) -> Result<()> {
        unsafe {
            if !self.bitmap.is_null() {
                SelectObject(self.dc, self.old);
                DeleteObject(self.bitmap);
                self.bitmap = null_mut();
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w as i32;
            info.bmiHeader.biHeight = -(h as i32); // top-down
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(self.dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                bail!("CreateDIBSection {w}x{h} failed");
            }
            self.old = SelectObject(self.dc, bitmap);
            self.bitmap = bitmap;
            self.bits = bits.cast();
            self.size = (w, h);
        }
        Ok(())
    }

    fn present(&mut self, pixels: &[u32], w: usize, h: usize) -> Result<()> {
        if self.size != (w, h) {
            self.resize(w, h)?;
        }
        unsafe {
            // Opaque: alpha 0xFF (premultiplied is then the same as straight).
            let dst = std::slice::from_raw_parts_mut(self.bits, w * h);
            for (d, s) in dst.iter_mut().zip(pixels) {
                *d = s | 0xFF00_0000;
            }
            let size = SIZE {
                cx: w as i32,
                cy: h as i32,
            };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            if UpdateLayeredWindow(
                self.hwnd,
                null_mut(),
                null(),
                &size,
                self.dc,
                &src,
                0,
                &blend,
                ULW_ALPHA,
            ) == 0
            {
                bail!("UpdateLayeredWindow failed (error {})", GetLastError());
            }
        }
        Ok(())
    }
}

impl Drop for LayeredPresenter {
    fn drop(&mut self) {
        unsafe {
            if !self.bitmap.is_null() {
                SelectObject(self.dc, self.old);
                DeleteObject(self.bitmap);
            }
            DeleteDC(self.dc);
        }
    }
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

/// Find (and with `create`, ask Explorer to create) the desktop windows.
/// Mirrors Lively's `WinDesktopCore.SetupDesktopLayer`.
fn desktop_layer(create: bool) -> Result<DesktopLayer> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), null());
        if progman.is_null() {
            bail!("couldn't find Progman; is Explorer running?");
        }
        let raised = GetWindowLongPtrW(progman, GWL_EXSTYLE) & WS_EX_NOREDIRECTIONBITMAP as isize != 0;

        // Makes Progman split the wallpaper into a WorkerW behind the icons.
        // Only when attaching: Explorer may reshuffle its windows in response,
        // which a diagnostic dump must not do to an attached wallpaper.
        if create {
            let mut result = 0usize;
            SendMessageTimeoutW(progman, 0x052C, 0xD, 0x1, SMTO_NORMAL, 1000, &mut result);
        }

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
    let layer = desktop_layer(true)?;
    let mode = if layer.raised {
        present_mode()
    } else {
        PresentMode::Gdi
    };
    // Diagnostic switch for testing other placements on real machines.
    let placement = std::env::var("RUSTY_WALLPAPER_ATTACH").unwrap_or_default();
    log::info!(
        "presenting with {mode:?}, placement {:?}",
        if placement.is_empty() {
            "default"
        } else {
            &placement
        }
    );
    let (pos, size) = (monitor.position(), monitor.size());
    if placement == "none" || placement == "none-layered" {
        // Control experiment: an ordinary top-level window over the monitor,
        // optionally layered like the attached one, to tell "layered windows
        // don't paint" apart from "children of Progman aren't composed".
        unsafe {
            if placement == "none-layered" {
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED as isize);
                SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
            }
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                pos.x,
                pos.y,
                size.width as i32,
                size.height as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        return Ok(DesktopGuard {
            raised: true,
            presenter: None,
        });
    }
    log_os_version();
    unsafe {
        // Like Lively's BorderlessWinStyle + RemoveWindowFromTaskbar: a bare
        // child window with no frame, caption or app-window behaviour.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let strip = WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX;
        SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !(strip as isize)) | WS_CHILD as isize);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let strip_ex =
            WS_EX_DLGMODALFRAME | WS_EX_WINDOWEDGE | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE | WS_EX_APPWINDOW;
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            (ex & !(strip_ex as isize)) | (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as isize,
        );

        let parent = if layer.raised {
            // Children of the DirectComposition-backed Progman only show up
            // when layered. This must happen before SetParent.
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED as isize);
            // UpdateLayeredWindow refuses windows that had their attributes set.
            if mode != PresentMode::UpdateLayered {
                SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
            }
            if placement == "worker" {
                layer.worker
            } else {
                layer.progman
            }
        } else {
            layer.worker
        };
        if SetParent(hwnd, parent).is_null() {
            bail!("SetParent onto the desktop failed (error {})", GetLastError());
        }

        // Child windows are positioned in the parent's client coordinates.
        let mut origin = POINT { x: pos.x, y: pos.y };
        ScreenToClient(parent, &mut origin);
        let (insert_after, zflag) = if layer.raised && placement == "progman-top" {
            (HWND_TOP, 0)
        } else if layer.raised && placement != "worker" {
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
        if layer.raised && placement != "worker" {
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
    let presenter: Option<Box<dyn FramePresenter>> = match mode {
        PresentMode::UpdateLayered => Some(Box::new(LayeredPresenter::new(hwnd)?)),
        PresentMode::DirectX => match dx::DxPresenter::new(hwnd) {
            Ok(p) => Some(Box::new(p)),
            Err(e) => {
                log::warn!("Direct3D presenter unavailable, falling back to GDI: {e:#}");
                None
            }
        },
        PresentMode::Gdi => None,
    };
    Ok(DesktopGuard {
        raised: layer.raised,
        presenter,
    })
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
    match desktop_layer(false) {
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
                let mut cloaked = 0u32;
                DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, (&mut cloaked as *mut u32).cast(), 4);
                format!("  <- rusty-wallpaper (cloaked={cloaked})")
            } else {
                String::new()
            },
        )
    }
}

/// Windows reports its real version to a process only if the process's
/// manifest declares it compatible; otherwise it claims 6.2 (Windows 8).
/// Logged so we can tell whether our embedded manifest is in effect.
fn log_os_version() {
    use windows::Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOW};
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    #[allow(deprecated)]
    if unsafe { GetVersionExW(&mut info) }.is_ok() {
        log::info!(
            "process sees Windows {}.{} build {} (10.0 = manifest active, 6.2 = no manifest)",
            info.dwMajorVersion,
            info.dwMinorVersion,
            info.dwBuildNumber
        );
    }
}

mod dx {
    //! Presents frames through a Direct3D 11 blt-model swap chain.

    use anyhow::{Context, Result};
    use windows::core::Interface;
    use windows::Win32::Foundation::{HMODULE, HWND};
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{
        IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT, DXGI_SCALING_STRETCH,
        DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_DISCARD,
        DXGI_USAGE_RENDER_TARGET_OUTPUT,
    };

    pub struct DxPresenter {
        hwnd: HWND,
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        swap: Option<IDXGISwapChain1>,
        size: (usize, usize),
    }

    impl DxPresenter {
        pub fn new(hwnd: super::HWND) -> Result<Self> {
            let (device, context) = create_device()?;
            Ok(Self {
                hwnd: HWND(hwnd),
                device,
                context,
                swap: None,
                size: (0, 0),
            })
        }

        fn swap_chain(&mut self, w: usize, h: usize) -> Result<IDXGISwapChain1> {
            if let Some(swap) = &self.swap {
                if self.size != (w, h) {
                    unsafe {
                        swap.ResizeBuffers(
                            0,
                            w as u32,
                            h as u32,
                            DXGI_FORMAT_UNKNOWN,
                            DXGI_SWAP_CHAIN_FLAG(0),
                        )
                    }
                    .context("resizing swap chain")?;
                    self.size = (w, h);
                }
                return Ok(swap.clone());
            }
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w as u32,
                Height: h as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 1,
                Scaling: DXGI_SCALING_STRETCH,
                // Blt model: presents land in the window's redirection
                // surface, which is what a layered child is composed from.
                SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
                ..Default::default()
            };
            let swap = unsafe {
                let dxgi: IDXGIDevice = self.device.cast()?;
                let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
                factory.CreateSwapChainForHwnd(&self.device, self.hwnd, &desc, None, None)
            }
            .context("creating swap chain")?;
            log::info!("Direct3D 11 swap chain created ({w}x{h})");
            self.size = (w, h);
            self.swap = Some(swap.clone());
            Ok(swap)
        }
    }

    impl super::FramePresenter for DxPresenter {
        fn present(&mut self, pixels: &[u32], w: usize, h: usize) -> Result<()> {
            let swap = self.swap_chain(w, h)?;
            unsafe {
                let back: ID3D11Texture2D = swap.GetBuffer(0)?;
                // 0x00RRGGBB little-endian is B,G,R,0 in memory: BGRA, alpha ignored.
                self.context
                    .UpdateSubresource(&back, 0, None, pixels.as_ptr().cast(), (w * 4) as u32, 0);
                swap.Present(0, DXGI_PRESENT(0)).ok().context("presenting")?;
            }
            Ok(())
        }
    }

    fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
        // Hardware first; WARP (software) keeps it working on GPU-less machines.
        let mut last_err = None;
        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            let mut device = None;
            let mut context = None;
            let r = unsafe {
                D3D11CreateDevice(
                    None,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            match (r, device, context) {
                (Ok(()), Some(d), Some(c)) => return Ok((d, c)),
                (Err(e), _, _) => last_err = Some(e),
                _ => {}
            }
        }
        Err(anyhow::anyhow!("D3D11CreateDevice failed: {last_err:?}"))
    }
}
