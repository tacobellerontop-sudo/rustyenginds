//! The windowing side: one borderless window per monitor, glued to the desktop.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, NamedKey};
use winit::monitor::MonitorHandle;
use winit::window::{Window, WindowId};

use crate::config::{Config, MonitorSelect};
use crate::platform::{self, DesktopGuard};
use crate::render::{Frame, FrameCtx};
use crate::scene::{self, Scene};

const CONFIG_POLL: Duration = Duration::from_secs(1);
const COVER_POLL: Duration = Duration::from_millis(500);

pub struct Options {
    /// Config file to load and watch, if any.
    pub config_path: Option<PathBuf>,
    pub scene: Option<String>,
    pub fps: Option<u32>,
    /// Show in a normal window instead of on the desktop.
    pub windowed: bool,
}

impl Options {
    fn load_config(&self) -> Result<Config> {
        let mut cfg = match &self.config_path {
            Some(p) => Config::load(p)?,
            None => Config::default(),
        };
        if let Some(s) = &self.scene {
            cfg.scene = s.clone();
        }
        if let Some(f) = self.fps {
            cfg.fps = f;
        }
        cfg.sanitize();
        Ok(cfg)
    }
}

struct Output {
    window: Rc<Window>,
    surface: softbuffer::Surface<OwnedDisplayHandle, Rc<Window>>,
    scene: Box<dyn Scene>,
    frame: Frame,
    cursor: Option<(f32, f32)>,
    occluded: bool,
    /// The desktop monitor this output covers (None in windowed mode).
    monitor: Option<MonitorHandle>,
    /// A fullscreen/maximized app is hiding this output.
    covered: bool,
    /// Full-resolution frame for platforms that present it themselves.
    present_buf: Vec<u32>,
    // Declared last so it is dropped after the window: restoring the desktop
    // must happen once our window is gone.
    desktop: Option<DesktopGuard>,
}

struct App {
    opts: Options,
    config: Config,
    config_mtime: Option<SystemTime>,
    last_config_check: Instant,
    last_cover_check: Instant,
    context: Option<softbuffer::Context<OwnedDisplayHandle>>,
    outputs: Vec<Output>,
    clock: f32,
    paused: bool,
    last_tick: Instant,
    next_frame: Instant,
    ctx: FrameCtx,
    quit: Arc<AtomicBool>,
    error: Option<anyhow::Error>,
}

pub fn run(opts: Options) -> Result<()> {
    let config = opts.load_config()?;
    // Fail fast on a bad scene before any window appears.
    scene::build(&config.scene, &config.params_for(&config.scene))?;

    let quit = Arc::new(AtomicBool::new(false));
    {
        let quit = quit.clone();
        ctrlc::set_handler(move || quit.store(true, Ordering::SeqCst))
            .context("installing Ctrl+C handler")?;
    }
    if !opts.windowed {
        platform::single_instance(quit.clone())?;
    }

    let event_loop = EventLoop::new().context("creating event loop")?;
    let now = Instant::now();
    let mut app = App {
        config_mtime: opts.config_path.as_deref().and_then(mtime),
        opts,
        config,
        last_config_check: now,
        last_cover_check: now,
        context: None,
        outputs: Vec::new(),
        clock: 0.0,
        paused: false,
        last_tick: now,
        next_frame: now,
        ctx: FrameCtx {
            time: 0.0,
            dt: 1.0 / 30.0,
            cursor: None,
        },
        quit,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Print the monitors winit can see, in the order `monitor = N` uses.
pub fn list_monitors() -> Result<()> {
    struct Lister;
    impl ApplicationHandler for Lister {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            for (i, m) in el.available_monitors().enumerate() {
                let (s, p) = (m.size(), m.position());
                let primary = el.primary_monitor().is_some_and(|pm| pm == m);
                println!(
                    "{i}: {} {}x{} at ({}, {}) scale {:.2}{}",
                    m.name().unwrap_or_else(|| "<unnamed>".into()),
                    s.width,
                    s.height,
                    p.x,
                    p.y,
                    m.scale_factor(),
                    if primary { " [primary]" } else { "" }
                );
            }
            el.exit();
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
    EventLoop::new()?.run_app(&mut Lister)?;
    Ok(())
}

fn mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl App {
    fn fail(&mut self, el: &ActiveEventLoop, e: anyhow::Error) {
        self.error = Some(e);
        el.exit();
    }

    fn create_outputs(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let context = softbuffer::Context::new(el.owned_display_handle())
            .map_err(|e| anyhow::anyhow!("creating softbuffer context: {e}"))?;

        let targets: Vec<Option<MonitorHandle>> = if self.opts.windowed {
            vec![None]
        } else {
            let monitors: Vec<_> = el.available_monitors().collect();
            if monitors.is_empty() {
                anyhow::bail!("no monitors found");
            }
            match self.config.monitor {
                MonitorSelect::All => monitors.into_iter().map(Some).collect(),
                MonitorSelect::Index(i) => {
                    let n = monitors.len();
                    let m = monitors.into_iter().nth(i).with_context(|| {
                        format!("monitor {i} does not exist ({n} found; see `rusty-wallpaper monitors`)")
                    })?;
                    vec![Some(m)]
                }
            }
        };

        for monitor in targets {
            let (attrs, desktop) = match &monitor {
                None => (
                    Window::default_attributes()
                        .with_title(format!("rusty-wallpaper — {}", self.config.scene))
                        .with_inner_size(LogicalSize::new(1280.0, 720.0)),
                    false,
                ),
                Some(m) => (platform::desktop_attributes(el, m), true),
            };
            let window = Rc::new(el.create_window(attrs).context("creating window")?);
            let guard = match (&monitor, desktop) {
                (Some(m), true) => Some(platform::attach_to_desktop(el, &window, m)?),
                _ => None,
            };
            let surface = softbuffer::Surface::new(&context, window.clone())
                .map_err(|e| anyhow::anyhow!("creating surface: {e}"))?;
            let scene = scene::build(&self.config.scene, &self.config.params_for(&self.config.scene))?;
            log::info!(
                "showing {:?} on {}",
                self.config.scene,
                monitor
                    .as_ref()
                    .map(|m| m.name().unwrap_or_else(|| "monitor".into()))
                    .unwrap_or_else(|| "a window".into())
            );
            window.request_redraw();
            self.outputs.push(Output {
                window,
                surface,
                scene,
                frame: Frame::new(1, 1),
                cursor: None,
                occluded: false,
                monitor,
                covered: false,
                desktop: guard,
                present_buf: Vec::new(),
            });
        }
        self.context = Some(context);
        Ok(())
    }

    fn maybe_reload_config(&mut self) {
        let Some(path) = self.opts.config_path.clone() else {
            return;
        };
        if self.last_config_check.elapsed() < CONFIG_POLL {
            return;
        }
        self.last_config_check = Instant::now();
        let m = mtime(&path);
        if m == self.config_mtime {
            return;
        }
        self.config_mtime = m;
        let cfg = match self.opts.load_config() {
            Ok(c) => c,
            Err(e) => {
                log::error!("config reload failed, keeping the current one: {e:#}");
                return;
            }
        };
        if cfg.monitor != self.config.monitor {
            log::warn!("changing `monitor` needs a restart");
        }
        let params = cfg.params_for(&cfg.scene);
        let mut rebuilt = Vec::with_capacity(self.outputs.len());
        for _ in &self.outputs {
            match scene::build(&cfg.scene, &params) {
                Ok(s) => rebuilt.push(s),
                Err(e) => {
                    log::error!("config reload failed, keeping the current scene: {e:#}");
                    return;
                }
            }
        }
        for (out, s) in self.outputs.iter_mut().zip(rebuilt) {
            out.scene = s;
            if self.opts.windowed {
                out.window.set_title(&format!("rusty-wallpaper — {}", cfg.scene));
            }
        }
        log::info!("config reloaded: scene {:?} at {} fps", cfg.scene, cfg.fps);
        self.config = cfg;
    }

    fn update_covered(&mut self) {
        if self.last_cover_check.elapsed() < COVER_POLL {
            return;
        }
        self.last_cover_check = Instant::now();
        let enabled = self.config.pause_when_covered;
        for o in &mut self.outputs {
            let covered = enabled && o.monitor.as_ref().is_some_and(platform::desktop_covered);
            if covered != o.covered {
                log::debug!(
                    "output {:?} {}",
                    o.window.id(),
                    if covered {
                        "covered, pausing"
                    } else {
                        "visible again"
                    }
                );
                o.covered = covered;
            }
        }
    }

    fn draw(&mut self, id: WindowId) -> Result<()> {
        let scale = self.config.render_scale;
        let ctx = self.ctx;
        let Some(out) = self.outputs.iter_mut().find(|o| o.window.id() == id) else {
            return Ok(());
        };
        let PhysicalSize { width, height } = out.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            return Ok(());
        };
        let scale = if out.scene.native_resolution() { 1.0 } else { scale };
        let rw = ((width as f32 * scale).round() as usize).max(1);
        let rh = ((height as f32 * scale).round() as usize).max(1);
        out.frame.resize(rw, rh);
        out.scene.render(
            &mut out.frame,
            &FrameCtx {
                cursor: out.cursor,
                ..ctx
            },
        );

        if let Some(desktop) = out.desktop.as_mut() {
            out.present_buf.resize(width as usize * height as usize, 0);
            out.frame
                .upscale_into(&mut out.present_buf, width as usize, height as usize);
            if let Some(result) = desktop.present(&out.present_buf, width as usize, height as usize) {
                return result;
            }
        }

        out.surface
            .resize(w, h)
            .map_err(|e| anyhow::anyhow!("resizing surface: {e}"))?;
        let mut buffer = out
            .surface
            .buffer_mut()
            .map_err(|e| anyhow::anyhow!("mapping buffer: {e}"))?;
        out.frame
            .upscale_into(&mut buffer, width as usize, height as usize);
        out.window.pre_present_notify();
        buffer.present().map_err(|e| anyhow::anyhow!("presenting: {e}"))?;
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.outputs.is_empty() {
            if let Err(e) = self.create_outputs(el) {
                self.fail(el, e);
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.draw(id) {
                    self.fail(el, e);
                }
            }
            WindowEvent::Occluded(occluded) => {
                if let Some(o) = self.outputs.iter_mut().find(|o| o.window.id() == id) {
                    o.occluded = occluded;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(o) = self.outputs.iter_mut().find(|o| o.window.id() == id) {
                    let s = o.window.inner_size();
                    o.cursor = Some((
                        position.x as f32 / s.width.max(1) as f32,
                        position.y as f32 / s.height.max(1) as f32,
                    ));
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(o) = self.outputs.iter_mut().find(|o| o.window.id() == id) {
                    o.cursor = None;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } if self.opts.windowed => match logical_key.as_ref() {
                Key::Named(NamedKey::Escape) | Key::Character("q") => el.exit(),
                Key::Named(NamedKey::Space) => self.paused = !self.paused,
                _ => {}
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.quit.load(Ordering::SeqCst) {
            el.exit();
            return;
        }
        self.maybe_reload_config();
        self.update_covered();

        let now = Instant::now();
        let frame_time = Duration::from_secs_f64(1.0 / self.config.fps as f64);
        if now >= self.next_frame {
            let dt = (now - self.last_tick).as_secs_f32().clamp(1e-4, 0.25);
            self.last_tick = now;
            if !self.paused {
                self.clock += dt * self.config.time_scale;
                self.ctx = FrameCtx {
                    time: self.clock,
                    dt: dt * self.config.time_scale,
                    cursor: None,
                };
                // Draw directly rather than waiting for WM_PAINT/expose: a
                // window parented into the Windows desktop may never get one.
                let due: Vec<WindowId> = self
                    .outputs
                    .iter()
                    .filter(|o| !o.occluded && !o.covered)
                    .map(|o| o.window.id())
                    .collect();
                for id in due {
                    if let Err(e) = self.draw(id) {
                        self.fail(el, e);
                        return;
                    }
                }
            }
            self.next_frame += frame_time;
            if self.next_frame < now {
                // We fell behind (slow frame or suspend); don't try to catch up.
                self.next_frame = now + frame_time;
            }
        }
        // Wake periodically even when paused so Ctrl+C and config edits are noticed.
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame.min(now + CONFIG_POLL)));
    }
}
