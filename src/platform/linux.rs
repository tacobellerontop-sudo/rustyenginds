//! X11: an EWMH `_NET_WM_WINDOW_TYPE_DESKTOP` window, which window managers
//! place below everything, on every workspace, and leave out of the taskbar.
//!
//! Wayland has no portable equivalent (it needs the wlr-layer-shell protocol,
//! which winit does not expose), so there we fall back to a bottom-most
//! fullscreen window and say so.

use anyhow::Result;
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::platform::wayland::ActiveEventLoopExtWayland;
use winit::platform::x11::{WindowAttributesExtX11, WindowType};
use winit::window::{Fullscreen, Window, WindowAttributes};

pub struct DesktopGuard;

impl DesktopGuard {
    /// This platform presents through softbuffer.
    pub fn present(&mut self, _pixels: &[u32], _width: usize, _height: usize) -> Option<Result<()>> {
        None
    }
}

pub fn desktop_attributes(el: &ActiveEventLoop, monitor: &MonitorHandle) -> WindowAttributes {
    let attrs = super::covering_attributes(monitor);
    if el.is_wayland() {
        attrs.with_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone()))))
    } else {
        attrs.with_x11_window_type(vec![WindowType::Desktop])
    }
}

pub fn attach_to_desktop(
    el: &ActiveEventLoop,
    _window: &Window,
    _monitor: &MonitorHandle,
) -> Result<DesktopGuard> {
    if el.is_wayland() {
        log::warn!(
            "Wayland has no standard way to draw behind the desktop; using a fullscreen \
             window kept at the bottom. Your compositor may still show it on top \
             (try running under XWayland with WINIT_UNIX_BACKEND=x11)."
        );
    }
    Ok(DesktopGuard)
}
