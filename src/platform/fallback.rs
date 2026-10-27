//! macOS and anything else: a borderless window at the bottom of the stack.
//! (Proper desktop-level windows on macOS need `kCGDesktopWindowLevel`.)

use anyhow::Result;
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{Window, WindowAttributes};

pub struct DesktopGuard;

impl DesktopGuard {
    /// This platform presents through softbuffer.
    pub fn present(&mut self, _pixels: &[u32], _width: usize, _height: usize) -> Option<Result<()>> {
        None
    }
}

pub fn desktop_attributes(_el: &ActiveEventLoop, monitor: &MonitorHandle) -> WindowAttributes {
    super::covering_attributes(monitor)
}

pub fn attach_to_desktop(
    _el: &ActiveEventLoop,
    _window: &Window,
    _monitor: &MonitorHandle,
) -> Result<DesktopGuard> {
    Ok(DesktopGuard)
}
