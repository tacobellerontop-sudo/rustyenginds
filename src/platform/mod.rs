//! Putting a window *behind* the desktop icons is different on every OS.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{
    attach_to_desktop, autostart, autostart_enabled, desktop_attributes, desktop_covered, detach_console,
    hide_console_window, is_running, launched_from_explorer, signal_stop, single_instance, DesktopGuard,
};

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(all(unix, not(target_os = "macos")))]
pub use self::linux::{attach_to_desktop, desktop_attributes, DesktopGuard};

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
mod fallback;
#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
pub use self::fallback::{attach_to_desktop, desktop_attributes, DesktopGuard};

#[cfg(not(windows))]
pub use self::portable::*;

use winit::monitor::MonitorHandle;
use winit::window::{Window, WindowAttributes, WindowLevel};

/// A borderless window covering `monitor`, kept below everything else.
/// The starting point every backend refines.
#[allow(dead_code)]
fn covering_attributes(monitor: &MonitorHandle) -> WindowAttributes {
    Window::default_attributes()
        .with_title("rusty-wallpaper")
        .with_decorations(false)
        .with_resizable(false)
        .with_position(monitor.position())
        .with_inner_size(monitor.size())
        .with_window_level(WindowLevel::AlwaysOnBottom)
}

/// Stand-ins for the Windows-only conveniences.
#[cfg(not(windows))]
mod portable {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use anyhow::{bail, Result};
    use winit::monitor::MonitorHandle;

    /// Whether a fullscreen or maximized app hides the desktop on `monitor`.
    pub fn desktop_covered(_monitor: &MonitorHandle) -> bool {
        false
    }

    /// Hold the single-instance lock and listen for `rusty-wallpaper stop`.
    pub fn single_instance(_quit: Arc<AtomicBool>) -> Result<()> {
        Ok(())
    }

    pub fn signal_stop() -> Result<bool> {
        bail!("`stop` is only supported on Windows; use Ctrl+C or kill the process")
    }

    pub fn detach_console() {}

    pub fn is_running() -> bool {
        false
    }

    pub fn launched_from_explorer() -> bool {
        false
    }

    pub fn hide_console_window(_cmd: &mut std::process::Command) {}

    pub fn autostart_enabled() -> bool {
        false
    }

    pub fn autostart(_enable: bool, _args: &str) -> Result<()> {
        bail!("`autostart` is only supported on Windows; use your desktop's autostart settings")
    }
}
