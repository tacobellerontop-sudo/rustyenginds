//! A small settings window: pick a scene, tweak it with a live preview, and
//! start or stop the desktop wallpaper.
//!
//! The wallpaper itself runs as a separate `--background` process that
//! watches the config file, so "Apply" is just a save.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions};

use crate::config::{Config, MonitorSelect, EXAMPLE_CONFIG};
use crate::platform;
use crate::render::{Frame, FrameCtx};
use crate::scene::{self, ParamKind, Scene, SceneInfo, SCENES};

const PREVIEW_W: usize = 480;
const PREVIEW_H: usize = 270;
const PREVIEW_FPS: f32 = 30.0;

pub fn run(config_path: PathBuf) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("rusty-wallpaper")
            .with_inner_size([820.0, 600.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rusty-wallpaper",
        options,
        Box::new(move |cc| Ok(Box::new(SettingsApp::new(&cc.egui_ctx, config_path)))),
    )
    .map_err(|e| anyhow::anyhow!("settings window failed: {e}"))
}

struct SettingsApp {
    config_path: PathBuf,
    config: Config,
    /// Unsaved edits.
    dirty: bool,
    status: String,

    preview: Option<Box<dyn Scene>>,
    preview_error: Option<String>,
    preview_frame: Frame,
    preview_texture: TextureHandle,
    preview_clock: f32,
    last_preview: Instant,

    child: Option<Child>,
    autostart: bool,
}

impl SettingsApp {
    fn new(ctx: &egui::Context, config_path: PathBuf) -> Self {
        let (config, status) = if config_path.is_file() {
            match Config::load(&config_path) {
                Ok(c) => (c, format!("Loaded {}", config_path.display())),
                Err(e) => (
                    Config::default(),
                    format!("Couldn't read the config, starting fresh: {e:#}"),
                ),
            }
        } else {
            (
                Config::parse(EXAMPLE_CONFIG).unwrap_or_default(),
                "No config yet. Pick a scene and press Apply.".to_owned(),
            )
        };
        let texture = ctx.load_texture(
            "preview",
            ColorImage::filled([PREVIEW_W, PREVIEW_H], Color32::BLACK),
            TextureOptions::LINEAR,
        );
        let mut app = Self {
            config_path,
            config,
            dirty: false,
            status,
            preview: None,
            preview_error: None,
            preview_frame: Frame::new(PREVIEW_W, PREVIEW_H),
            preview_texture: texture,
            preview_clock: 0.0,
            last_preview: Instant::now(),
            child: None,
            autostart: platform::autostart_enabled(),
        };
        app.rebuild_preview();
        app
    }

    fn scene_info(&self) -> Option<&'static SceneInfo> {
        SCENES
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&self.config.scene))
    }

    fn rebuild_preview(&mut self) {
        match scene::build(&self.config.scene, &self.config.params_for(&self.config.scene)) {
            Ok(s) => {
                self.preview = Some(s);
                self.preview_error = None;
            }
            Err(e) => {
                self.preview = None;
                self.preview_error = Some(format!("{e:#}"));
            }
        }
    }

    fn tick_preview(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_preview).as_secs_f32().min(0.25);
        if dt < 1.0 / PREVIEW_FPS {
            return;
        }
        self.last_preview = now;
        let Some(scene) = &mut self.preview else { return };
        let dt = dt * self.config.time_scale;
        self.preview_clock += dt;
        scene.render(
            &mut self.preview_frame,
            &FrameCtx {
                time: self.preview_clock,
                dt,
                cursor: None,
            },
        );
        let pixels = self
            .preview_frame
            .pixels
            .iter()
            .map(|&p| Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
            .collect();
        self.preview_texture.set(
            ColorImage::new([PREVIEW_W, PREVIEW_H], pixels),
            TextureOptions::LINEAR,
        );
    }

    fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => return true,
                Ok(Some(status)) => {
                    if !status.success() {
                        self.status = format!("The wallpaper stopped with an error; see {}", log_hint());
                    }
                    self.child = None;
                }
                Err(_) => self.child = None,
            }
        }
        platform::is_running()
    }

    fn apply(&mut self) -> bool {
        match self.config.save(&self.config_path) {
            Ok(()) => {
                self.dirty = false;
                self.status = format!("Saved {}", self.config_path.display());
                true
            }
            Err(e) => {
                self.status = format!("Couldn't save: {e:#}");
                false
            }
        }
    }

    fn start(&mut self) {
        if !self.apply() {
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                self.status = format!("Couldn't find our own executable: {e}");
                return;
            }
        };
        let mut cmd = Command::new(exe);
        cmd.arg("--background").arg("--config").arg(&self.config_path);
        platform::hide_console_window(&mut cmd);
        match cmd.spawn() {
            Ok(child) => {
                self.child = Some(child);
                self.status = "Wallpaper started".into();
            }
            Err(e) => self.status = format!("Couldn't start the wallpaper: {e}"),
        }
    }

    fn stop(&mut self) {
        let signalled = platform::signal_stop().unwrap_or(false);
        if let Some(mut child) = self.child.take() {
            if !signalled {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        self.status = "Wallpaper stopped".into();
    }

    fn set_autostart(&mut self, enable: bool) {
        let args = format!("--background --config \"{}\"", self.config_path.display());
        match platform::autostart(enable, &args) {
            Ok(()) => {
                self.autostart = enable;
                self.status = if enable {
                    "Will start at login"
                } else {
                    "Won't start at login"
                }
                .into();
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    fn scene_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.heading("Scenes");
        ui.add_space(4.0);
        for info in SCENES {
            let selected = self.config.scene.eq_ignore_ascii_case(info.name);
            if ui
                .selectable_label(selected, title_case(info.name))
                .on_hover_text(info.description)
                .clicked()
                && !selected
            {
                self.config.scene = info.name.to_owned();
                self.dirty = true;
                self.preview_clock = 0.0;
                self.rebuild_preview();
            }
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width().min(PREVIEW_W as f32);
        let size = egui::vec2(width, width * PREVIEW_H as f32 / PREVIEW_W as f32);
        if let Some(err) = &self.preview_error {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, 6.0, Color32::from_gray(24));
            ui.put(
                rect.shrink(16.0),
                egui::Label::new(RichText::new(err).color(Color32::LIGHT_RED)).wrap(),
            );
        } else {
            ui.add(egui::Image::new((self.preview_texture.id(), size)).corner_radius(6.0));
        }
        if let Some(info) = self.scene_info() {
            ui.label(RichText::new(info.description).weak());
        }
    }

    fn scene_settings(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.scene_info() else { return };
        let table = self.config.scenes.entry(info.name.to_owned()).or_default();
        let (mut changed, mut reload) = (false, false);
        egui::Grid::new("scene-params")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                for spec in info.params {
                    ui.label(spec.label);
                    let edit = param_editor(ui, table, spec.key, &spec.kind);
                    changed |= edit.changed;
                    reload |= edit.reload;
                    ui.end_row();
                }
            });
        self.dirty |= changed;
        if reload {
            self.rebuild_preview();
        }
    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.config;
        let mut changed = false;
        egui::Grid::new("general")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("Frame rate");
                changed |= ui
                    .add(egui::Slider::new(&mut c.fps, 5..=60).suffix(" fps"))
                    .changed();
                ui.end_row();

                ui.label("Quality");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut c.render_scale, 0.25..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                    )
                    .on_hover_text("Internal resolution. Lower uses less CPU.")
                    .changed();
                ui.end_row();

                ui.label("Animation speed");
                changed |= ui
                    .add(egui::Slider::new(&mut c.time_scale, 0.0..=3.0).suffix("×"))
                    .changed();
                ui.end_row();

                ui.label("Monitors");
                ui.horizontal(|ui| {
                    let mut single = matches!(c.monitor, MonitorSelect::Index(_));
                    changed |= ui.radio_value(&mut single, false, "All").changed();
                    changed |= ui.radio_value(&mut single, true, "Only #").changed();
                    let mut idx = match c.monitor {
                        MonitorSelect::Index(i) => i,
                        MonitorSelect::All => 0,
                    };
                    changed |= ui
                        .add_enabled(single, egui::DragValue::new(&mut idx).range(0..=15))
                        .changed();
                    c.monitor = if single {
                        MonitorSelect::Index(idx)
                    } else {
                        MonitorSelect::All
                    };
                });
                ui.end_row();

                ui.label("");
                changed |= ui
                    .checkbox(
                        &mut c.pause_when_covered,
                        "Pause behind fullscreen and maximized apps",
                    )
                    .changed();
                ui.end_row();
            });
        self.dirty |= changed;
    }

    fn action_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.is_running();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let (dot, text) = if running {
                (Color32::from_rgb(80, 200, 120), "Running")
            } else {
                (Color32::GRAY, "Stopped")
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, dot);
            ui.label(text);
            ui.separator();

            if running {
                if ui.button("Stop wallpaper").clicked() {
                    self.stop();
                }
            } else if ui.button(RichText::new("Start wallpaper").strong()).clicked() {
                self.start();
            }
            let apply_text = if running { "Apply (live)" } else { "Apply" };
            if ui
                .add_enabled(self.dirty, egui::Button::new(apply_text))
                .clicked()
            {
                self.apply();
            }

            if cfg!(windows) {
                let mut auto = self.autostart;
                if ui.checkbox(&mut auto, "Start at login").changed() {
                    self.set_autostart(auto);
                }
            }
        });
        ui.label(RichText::new(&self.status).small().weak());
        ui.add_space(4.0);
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.tick_preview();

        egui::Panel::bottom("actions").show(ui, |ui| self.action_bar(ui));
        egui::Panel::left("scenes")
            .resizable(false)
            .exact_size(150.0)
            .show(ui, |ui| self.scene_list(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.preview(ui);
                ui.add_space(10.0);
                ui.heading("Scene settings");
                self.scene_settings(ui);
                ui.add_space(10.0);
                ui.collapsing("General", |ui| self.general_settings(ui));
            });
        });

        ui.ctx()
            .request_repaint_after(Duration::from_secs_f32(1.0 / PREVIEW_FPS));
    }
}

struct Edit {
    /// The stored value changed (the config needs saving).
    changed: bool,
    /// The preview should be rebuilt.
    reload: bool,
}

impl From<bool> for Edit {
    fn from(changed: bool) -> Self {
        Self {
            changed,
            reload: changed,
        }
    }
}

/// Draw the right widget for one parameter.
fn param_editor(ui: &mut egui::Ui, table: &mut toml::Table, key: &str, kind: &ParamKind) -> Edit {
    match kind {
        ParamKind::Float { min, max, default } => {
            let mut v = match table.get(key) {
                Some(toml::Value::Float(f)) => *f as f32,
                Some(toml::Value::Integer(i)) => *i as f32,
                _ => *default,
            };
            let changed = ui.add(egui::Slider::new(&mut v, *min..=*max)).changed();
            if changed {
                // Round so the saved file stays readable.
                table.insert(
                    key.into(),
                    toml::Value::Float(((v * 1000.0).round() / 1000.0) as f64),
                );
            }
            changed.into()
        }
        ParamKind::Int { min, max, default } => {
            let mut v = table.get(key).and_then(|v| v.as_integer()).unwrap_or(*default);
            let changed = ui.add(egui::Slider::new(&mut v, *min..=*max)).changed();
            if changed {
                table.insert(key.into(), toml::Value::Integer(v));
            }
            changed.into()
        }
        ParamKind::Color { default } => {
            let hex = table.get(key).and_then(|v| v.as_str()).unwrap_or(default);
            let mut rgb = scene::parse_hex_color(hex).unwrap_or([1.0, 1.0, 1.0]);
            let changed = ui.color_edit_button_rgb(&mut rgb).changed();
            if changed {
                let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                let hex = format!("#{:02x}{:02x}{:02x}", c(rgb[0]), c(rgb[1]), c(rgb[2]));
                table.insert(key.into(), toml::Value::String(hex));
            }
            changed.into()
        }
        ParamKind::Choice { options, default } => {
            let mut current = table
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(default)
                .to_owned();
            let mut changed = false;
            ui.horizontal(|ui| {
                for opt in *options {
                    changed |= ui
                        .radio_value(&mut current, (*opt).to_owned(), title_case(opt))
                        .changed();
                }
            });
            if changed {
                table.insert(key.into(), toml::Value::String(current));
            }
            changed.into()
        }
        ParamKind::Path => {
            let old = table
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            let mut path = old.clone();
            let mut reload = false;
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut path).desired_width(260.0));
                // Loading images hits the disk, so only reload once typing is done.
                reload = edit.lost_focus();
                #[cfg(windows)]
                {
                    if ui.button("Folder…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            path = p.display().to_string();
                            reload = true;
                        }
                    }
                    if ui.button("File…").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
                            .pick_file()
                        {
                            path = p.display().to_string();
                            reload = true;
                        }
                    }
                }
            });
            let changed = path != old;
            if changed {
                table.insert(key.into(), toml::Value::String(path));
            }
            Edit { changed, reload }
        }
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn log_hint() -> String {
    dirs::data_local_dir()
        .map(|d| {
            d.join("rusty-wallpaper")
                .join("rusty-wallpaper.log")
                .display()
                .to_string()
        })
        .unwrap_or_else(|| "the log file".into())
}
