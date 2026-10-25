//! The settings window: a gallery of wallpapers (built-in scenes plus the
//! user's own videos and images), a live preview of the selected one, and
//! controls to start/stop the desktop wallpaper.
//!
//! The wallpaper itself runs as a separate `--background` process that
//! watches the config file, so applying a change is just a save.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2,
};

use crate::config::{Config, LibraryItem, MonitorSelect, EXAMPLE_CONFIG};
use crate::render::{Frame, FrameCtx};
use crate::scene::{self, expand_tilde, ParamKind, Scene, SceneInfo, SCENES};
use crate::{ffmpeg, platform};

const PREVIEW_W: usize = 480;
const PREVIEW_H: usize = 270;
const PREVIEW_FPS: f32 = 30.0;
const THUMB_W: usize = 320;
const THUMB_H: usize = 180;
const MIN_CARD_W: f32 = 190.0;
const CARD_GAP: f32 = 14.0;
const INSPECTOR_W: f32 = 360.0;

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif"];

mod theme {
    use eframe::egui::Color32;

    pub const BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x15);
    pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x19, 0x20);
    pub const CARD: Color32 = Color32::from_rgb(0x1d, 0x21, 0x2a);
    pub const CARD_HOVER: Color32 = Color32::from_rgb(0x25, 0x2a, 0x35);
    pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x3a);
    pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xee);
    pub const MUTED: Color32 = Color32::from_rgb(0x8b, 0x92, 0xa3);
    pub const ACCENT: Color32 = Color32::from_rgb(0x7c, 0x5c, 0xff);
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x8f, 0x74, 0xff);
    pub const GREEN: Color32 = Color32::from_rgb(0x3e, 0xcf, 0x8e);
    pub const RED: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
}

pub fn run(config_path: PathBuf) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rusty Wallpaper")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rusty-wallpaper",
        options,
        Box::new(move |cc| {
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::new(&cc.egui_ctx, config_path)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("settings window failed: {e}"))
}

fn apply_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        use egui::{FontFamily::Proportional, TextStyle};
        style.text_styles = [
            (TextStyle::Heading, FontId::new(20.0, Proportional)),
            (TextStyle::Body, FontId::new(14.5, Proportional)),
            (TextStyle::Button, FontId::new(14.5, Proportional)),
            (TextStyle::Small, FontId::new(12.0, Proportional)),
            (
                TextStyle::Monospace,
                FontId::new(13.0, egui::FontFamily::Monospace),
            ),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.slider_width = 150.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = theme::BG;
        v.window_fill = theme::PANEL;
        v.extreme_bg_color = theme::BG;
        v.faint_bg_color = theme::CARD;
        v.override_text_color = Some(theme::TEXT);
        v.selection.bg_fill = theme::ACCENT;
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.slider_trailing_fill = true;
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.window_stroke = Stroke::new(1.0, theme::BORDER);
        for (w, fill) in [
            (&mut v.widgets.noninteractive, theme::PANEL),
            (&mut v.widgets.inactive, theme::CARD),
            (&mut v.widgets.hovered, theme::CARD_HOVER),
            (&mut v.widgets.active, theme::ACCENT),
            (&mut v.widgets.open, theme::CARD_HOVER),
        ] {
            w.corner_radius = CornerRadius::same(8);
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = Stroke::NONE;
            w.expansion = 0.0;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, theme::BORDER);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, theme::MUTED);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, theme::TEXT);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    });
}

/// What a library entry plays as.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaKind {
    Video,
    Image,
    Folder,
}

impl MediaKind {
    fn of(path: &Path) -> Self {
        if path.is_dir() {
            MediaKind::Folder
        } else if ffmpeg::is_video(path) {
            MediaKind::Video
        } else {
            MediaKind::Image
        }
    }

    fn scene(self) -> &'static str {
        match self {
            MediaKind::Video => "video",
            MediaKind::Image | MediaKind::Folder => "image",
        }
    }

    fn badge(self) -> &'static str {
        match self {
            MediaKind::Video => "VIDEO",
            MediaKind::Image => "IMAGE",
            MediaKind::Folder => "SLIDESHOW",
        }
    }
}

/// Which gallery card is selected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Selection {
    Builtin(&'static str),
    Library(usize),
    None,
}

enum Thumb {
    Loading,
    Ready(TextureHandle),
    Failed,
}

struct SettingsApp {
    config_path: PathBuf,
    config: Config,
    /// Unsaved edits.
    dirty: bool,
    status: Option<(String, Instant)>,

    preview: Option<Box<dyn Scene>>,
    preview_error: Option<String>,
    preview_frame: Frame,
    preview_texture: TextureHandle,
    preview_clock: f32,
    last_preview: Instant,

    builtin_thumbs: HashMap<&'static str, TextureHandle>,
    thumbs: HashMap<PathBuf, Thumb>,
    thumb_tx: Sender<(PathBuf, Option<ColorImage>)>,
    thumb_rx: Receiver<(PathBuf, Option<ColorImage>)>,

    /// Path typed into the "add" box (for platforms without file dialogs).
    add_path: String,
    child: Option<Child>,
    autostart: bool,
}

impl SettingsApp {
    fn new(ctx: &egui::Context, config_path: PathBuf) -> Self {
        let (config, status) = if config_path.is_file() {
            match Config::load(&config_path) {
                Ok(c) => (c, None),
                Err(e) => (
                    Config::default(),
                    Some(format!("Couldn't read your config, starting fresh: {e:#}")),
                ),
            }
        } else {
            let mut c = Config::parse(EXAMPLE_CONFIG).unwrap_or_default();
            // The example's placeholder paths aren't real files.
            for t in c.scenes.values_mut() {
                t.remove("path");
            }
            (c, None)
        };
        let (thumb_tx, thumb_rx) = mpsc::channel();
        let preview_texture = ctx.load_texture(
            "preview",
            ColorImage::filled([PREVIEW_W, PREVIEW_H], theme::BG),
            TextureOptions::LINEAR,
        );
        let mut app = Self {
            config_path,
            config,
            dirty: false,
            status: status.map(|s| (s, Instant::now())),
            preview: None,
            preview_error: None,
            preview_frame: Frame::new(PREVIEW_W, PREVIEW_H),
            preview_texture,
            preview_clock: 0.0,
            last_preview: Instant::now(),
            builtin_thumbs: render_builtin_thumbs(ctx),
            thumbs: HashMap::new(),
            thumb_tx,
            thumb_rx,
            add_path: String::new(),
            child: None,
            autostart: platform::autostart_enabled(),
        };
        app.adopt_configured_media();
        for item in app.config.library.clone() {
            app.request_thumb(&item.path);
        }
        app.rebuild_preview();
        app
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    // ----- library -------------------------------------------------------

    /// A config written by hand may point the video/image scene at a file
    /// that isn't in the library yet; list it so it shows up as selected.
    fn adopt_configured_media(&mut self) {
        let scene = self.config.scene.clone();
        if !matches!(scene.as_str(), "video" | "image") {
            return;
        }
        if let Some(path) = self.media_path(&scene) {
            if path.exists() && !self.config.library.iter().any(|i| i.path == path) {
                self.config.library.push(LibraryItem {
                    name: display_name(&path),
                    path,
                });
            }
        }
    }

    fn media_path(&self, scene: &str) -> Option<PathBuf> {
        let raw = self.config.scenes.get(scene)?.get("path")?.as_str()?;
        (!raw.trim().is_empty()).then(|| expand_tilde(raw))
    }

    fn add_to_library(&mut self, path: PathBuf) {
        let kind = MediaKind::of(&path);
        let supported = kind != MediaKind::Image
            || path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
        if !path.exists() || !supported {
            self.notify(format!("Can't use {} as a wallpaper", path.display()));
            return;
        }
        let idx = match self.config.library.iter().position(|i| i.path == path) {
            Some(i) => i,
            None => {
                self.config.library.push(LibraryItem {
                    name: display_name(&path),
                    path: path.clone(),
                });
                self.request_thumb(&path);
                self.config.library.len() - 1
            }
        };
        self.select_library(idx);
        self.notify(format!("Added {}", self.config.library[idx].name));
    }

    fn remove_from_library(&mut self, idx: usize) {
        let was_selected = self.selection() == Selection::Library(idx);
        let item = self.config.library.remove(idx);
        self.thumbs.remove(&item.path);
        self.dirty = true;
        if was_selected {
            self.select_builtin(&SCENES[0]);
        }
        self.notify(format!("Removed {}", item.name));
    }

    fn request_thumb(&mut self, path: &Path) {
        if self.thumbs.contains_key(path) {
            return;
        }
        self.thumbs.insert(path.to_path_buf(), Thumb::Loading);
        let (tx, path) = (self.thumb_tx.clone(), path.to_path_buf());
        std::thread::spawn(move || {
            let img = load_thumb(&path);
            let _ = tx.send((path, img));
        });
    }

    fn poll_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, img)) = self.thumb_rx.try_recv() {
            let thumb = match img {
                Some(img) => {
                    Thumb::Ready(ctx.load_texture(path.display().to_string(), img, TextureOptions::LINEAR))
                }
                None => Thumb::Failed,
            };
            self.thumbs.insert(path, thumb);
        }
    }

    // ----- selection & preview ------------------------------------------

    fn selection(&self) -> Selection {
        let scene = self.config.scene.as_str();
        if matches!(scene, "video" | "image") {
            return match self.media_path(scene) {
                Some(p) => self
                    .config
                    .library
                    .iter()
                    .position(|i| i.path == p)
                    .map_or(Selection::None, Selection::Library),
                None => Selection::None,
            };
        }
        SCENES
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(scene))
            .map_or(Selection::None, |s| Selection::Builtin(s.name))
    }

    fn select_builtin(&mut self, info: &'static SceneInfo) {
        self.config.scene = info.name.to_owned();
        self.after_select();
    }

    fn select_library(&mut self, idx: usize) {
        let path = self.config.library[idx].path.clone();
        let scene = MediaKind::of(&path).scene();
        self.config.scene = scene.to_owned();
        self.config
            .scenes
            .entry(scene.to_owned())
            .or_default()
            .insert("path".into(), toml::Value::String(path.display().to_string()));
        self.after_select();
    }

    fn after_select(&mut self) {
        self.dirty = true;
        self.preview_clock = 0.0;
        self.rebuild_preview();
    }

    fn rebuild_preview(&mut self) {
        // Drop the old scene first so a video preview's ffmpeg exits promptly.
        self.preview = None;
        match scene::build(&self.config.scene, &self.config.params_for(&self.config.scene)) {
            Ok(s) => {
                self.preview = Some(s);
                self.preview_error = None;
            }
            Err(e) => self.preview_error = Some(format!("{e:#}")),
        }
        self.preview_frame.pixels.fill(0);
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
        self.preview_texture
            .set(frame_to_image(&self.preview_frame), TextureOptions::LINEAR);
    }

    // ----- engine process ------------------------------------------------

    fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => return true,
                Ok(Some(status)) => {
                    self.child = None;
                    if !status.success() {
                        self.notify(format!(
                            "The wallpaper stopped with an error. Details: {}",
                            log_hint()
                        ));
                    }
                }
                Err(_) => self.child = None,
            }
        }
        platform::is_running()
    }

    fn save(&mut self) -> bool {
        match self.config.save(&self.config_path) {
            Ok(()) => {
                self.dirty = false;
                true
            }
            Err(e) => {
                self.notify(format!("Couldn't save settings: {e:#}"));
                false
            }
        }
    }

    fn start(&mut self) {
        if !self.save() {
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => return self.notify(format!("Couldn't find our own executable: {e}")),
        };
        let mut cmd = Command::new(exe);
        cmd.arg("--background").arg("--config").arg(&self.config_path);
        platform::hide_console_window(&mut cmd);
        match cmd.spawn() {
            Ok(child) => {
                self.child = Some(child);
                self.notify("Wallpaper is live on your desktop");
            }
            Err(e) => self.notify(format!("Couldn't start the wallpaper: {e}")),
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
        self.notify("Wallpaper stopped");
    }

    fn set_autostart(&mut self, enable: bool) {
        let args = format!("--background --config \"{}\"", self.config_path.display());
        match platform::autostart(enable, &args) {
            Ok(()) => {
                self.autostart = enable;
                self.notify(if enable {
                    "Will start when you log in"
                } else {
                    "Won't start at login"
                });
            }
            Err(e) => self.notify(format!("{e:#}")),
        }
    }

    // ----- top bar -------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.is_running();
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new("Rusty Wallpaper").size(19.0).strong());
            if let Some((msg, at)) = &self.status {
                if at.elapsed() < Duration::from_secs(6) {
                    ui.add_space(12.0);
                    ui.label(RichText::new(msg).color(theme::MUTED));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let clicked = if running {
                    ui.add(secondary_button("Stop wallpaper")).clicked()
                } else {
                    ui.add(primary_button("Set as wallpaper")).clicked()
                };
                if clicked {
                    if running {
                        self.stop();
                    } else {
                        self.start();
                    }
                }
                ui.add_space(4.0);
                status_pill(ui, running);
            });
        });
    }

    // ----- gallery -------------------------------------------------------

    fn gallery(&mut self, ui: &mut egui::Ui) {
        let selection = self.selection();
        let width = ui.available_width();
        let cols = ((width + CARD_GAP) / (MIN_CARD_W + CARD_GAP)).floor().max(1.0);
        let card_w = ((width - CARD_GAP * (cols - 1.0)) / cols).floor();

        section_title(ui, "Built-in", None);
        let mut clicked_builtin = None;
        card_grid(ui, |ui| {
            for info in SCENES.iter().filter(|s| !s.is_user_media()) {
                let thumb = self.builtin_thumbs.get(info.name).map(|t| t.id());
                let r = card(
                    ui,
                    card_w,
                    thumb,
                    &title_case(info.name),
                    None,
                    selection == Selection::Builtin(info.name),
                )
                .on_hover_text(info.description);
                if r.clicked() {
                    clicked_builtin = Some(info);
                }
            }
        });
        if let Some(info) = clicked_builtin {
            self.select_builtin(info);
        }

        ui.add_space(18.0);
        section_title(
            ui,
            "Your wallpapers",
            Some("Drop videos or images anywhere in this window"),
        );
        let mut clicked_lib = None;
        let mut remove = None;
        let mut add_clicked = false;
        card_grid(ui, |ui| {
            for (i, item) in self.config.library.iter().enumerate() {
                let kind = MediaKind::of(&item.path);
                let (thumb, missing) = match self.thumbs.get(&item.path) {
                    Some(Thumb::Ready(t)) => (Some(t.id()), false),
                    Some(Thumb::Failed) => (None, true),
                    _ => (None, false),
                };
                let badge = if missing { "MISSING" } else { kind.badge() };
                let r = card(
                    ui,
                    card_w,
                    thumb,
                    &item.name,
                    Some(badge),
                    selection == Selection::Library(i),
                )
                .on_hover_text(item.path.display().to_string());
                if r.clicked() {
                    clicked_lib = Some(i);
                }
                r.context_menu(|ui| {
                    if ui.button("Remove from library").clicked() {
                        remove = Some(i);
                    }
                });
            }
            add_clicked = add_card(ui, card_w).clicked();
        });
        if let Some(i) = clicked_lib {
            self.select_library(i);
        }
        if let Some(i) = remove {
            self.remove_from_library(i);
        }
        if add_clicked {
            self.pick_files();
        }

        // Without native file dialogs, offer a path box instead.
        if !cfg!(windows) {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.add_path)
                        .hint_text("…or paste a path to a video, image or folder")
                        .desired_width(380.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Add").clicked() || enter) && !self.add_path.trim().is_empty() {
                    let p = expand_tilde(self.add_path.trim());
                    self.add_path.clear();
                    self.add_to_library(p);
                }
            });
        }
    }

    #[cfg(windows)]
    fn pick_files(&mut self) {
        let mut exts: Vec<&str> = ffmpeg::VIDEO_EXTENSIONS.to_vec();
        exts.extend_from_slice(IMAGE_EXTENSIONS);
        if let Some(paths) = rfd::FileDialog::new()
            .set_title("Add wallpapers")
            .add_filter("Videos and images", &exts)
            .add_filter("Videos", ffmpeg::VIDEO_EXTENSIONS)
            .add_filter("Images", IMAGE_EXTENSIONS)
            .pick_files()
        {
            for p in paths {
                self.add_to_library(p);
            }
        }
    }

    #[cfg(windows)]
    fn pick_folder(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Add a slideshow folder")
            .pick_folder()
        {
            self.add_to_library(p);
        }
    }

    #[cfg(not(windows))]
    fn pick_files(&mut self) {
        self.notify("Drag files onto the window, or paste a path below");
    }

    // ----- inspector -----------------------------------------------------

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let selection = self.selection();
        let (title, subtitle) = match selection {
            Selection::Builtin(name) => {
                let desc = SCENES
                    .iter()
                    .find(|s| s.name == name)
                    .map_or("", |s| s.description);
                (title_case(name), desc.to_owned())
            }
            Selection::Library(i) => {
                let item = &self.config.library[i];
                let kind = match MediaKind::of(&item.path) {
                    MediaKind::Video => "Video",
                    MediaKind::Image => "Image",
                    MediaKind::Folder => "Slideshow",
                };
                let file = item
                    .path
                    .file_name()
                    .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
                (item.name.clone(), format!("{kind} · {file}"))
            }
            Selection::None => (
                "Nothing selected".to_owned(),
                "Pick a wallpaper from the gallery".to_owned(),
            ),
        };

        // Live preview.
        let w = ui.available_width();
        let size = Vec2::new(w, w * PREVIEW_H as f32 / PREVIEW_W as f32);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter().rect_filled(rect, 10, theme::BG);
        if let Some(err) = &self.preview_error {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                wrap_words(err, 42),
                FontId::proportional(13.0),
                theme::RED,
            );
        } else {
            egui::Image::new((self.preview_texture.id(), size))
                .corner_radius(10)
                .paint_at(ui, rect);
        }

        ui.add_space(4.0);
        ui.label(RichText::new(title).size(20.0).strong());
        ui.label(RichText::new(subtitle).color(theme::MUTED));
        ui.add_space(6.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Selection::Builtin(_) | Selection::Library(_) = selection {
                    caption(ui, "SETTINGS");
                    self.scene_settings(ui);
                    if let Selection::Library(i) = selection {
                        ui.add_space(4.0);
                        #[cfg(windows)]
                        if MediaKind::of(&self.config.library[i].path) == MediaKind::Folder
                            && ui.button("Add another slideshow folder…").clicked()
                        {
                            self.pick_folder();
                        }
                        if ui.add(secondary_button("Remove from library")).clicked() {
                            self.remove_from_library(i);
                        }
                    }
                    ui.add_space(14.0);
                }

                caption(ui, "PERFORMANCE & STARTUP");
                self.general_settings(ui);
                #[cfg(windows)]
                if selection == Selection::None {
                    ui.add_space(8.0);
                    if ui.button("Add a slideshow folder…").clicked() {
                        self.pick_folder();
                    }
                }
            });
    }

    fn apply_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.is_running();
        ui.add_space(4.0);
        let text = if running {
            "Apply to desktop"
        } else {
            "Save changes"
        };
        let size = egui::vec2(ui.available_width(), 38.0);
        let button = if self.dirty {
            primary_button(text)
        } else {
            secondary_button(text)
        }
        .min_size(size);
        if ui.add_enabled(self.dirty, button).clicked() && self.save() {
            self.notify(if running { "Applied" } else { "Saved" });
        }
        if !self.dirty {
            let msg = if running {
                "Your desktop is up to date"
            } else {
                "All changes saved"
            };
            ui.vertical_centered(|ui| ui.label(RichText::new(msg).small().color(theme::MUTED)));
        }
    }

    fn scene_settings(&mut self, ui: &mut egui::Ui) {
        let Some(info) = SCENES
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&self.config.scene))
        else {
            return;
        };
        let table = self.config.scenes.entry(info.name.to_owned()).or_default();
        let mut changed = false;
        egui::Grid::new("scene-params")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                // The file itself is chosen in the gallery.
                for spec in info.params.iter().filter(|p| !matches!(p.kind, ParamKind::Path)) {
                    ui.label(RichText::new(spec.label).color(theme::MUTED));
                    changed |= param_editor(ui, table, spec.key, &spec.kind);
                    ui.end_row();
                }
            });
        if changed {
            self.dirty = true;
            self.rebuild_preview();
        }
    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let mut autostart = self.autostart;
        let c = &mut self.config;
        egui::Grid::new("general")
            .num_columns(2)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Frame rate").color(theme::MUTED));
                changed |= ui
                    .add(egui::Slider::new(&mut c.fps, 5..=60).suffix(" fps"))
                    .changed();
                ui.end_row();

                ui.label(RichText::new("Quality").color(theme::MUTED));
                changed |= ui
                    .add(
                        egui::Slider::new(&mut c.render_scale, 0.25..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                    )
                    .on_hover_text(
                        "Resolution of built-in scenes. Lower uses less CPU. Videos always play sharp.",
                    )
                    .changed();
                ui.end_row();

                ui.label(RichText::new("Speed").color(theme::MUTED));
                changed |= ui
                    .add(egui::Slider::new(&mut c.time_scale, 0.0..=3.0).suffix("×"))
                    .changed();
                ui.end_row();

                ui.label(RichText::new("Monitors").color(theme::MUTED));
                ui.horizontal(|ui| {
                    let mut single = matches!(c.monitor, MonitorSelect::Index(_));
                    changed |= ui.selectable_value(&mut single, false, "All").changed();
                    changed |= ui.selectable_value(&mut single, true, "Only").changed();
                    let mut idx = match c.monitor {
                        MonitorSelect::Index(i) => i + 1,
                        MonitorSelect::All => 1,
                    };
                    changed |= ui
                        .add_enabled(single, egui::DragValue::new(&mut idx).range(1..=16).prefix("#"))
                        .changed();
                    c.monitor = if single {
                        MonitorSelect::Index(idx - 1)
                    } else {
                        MonitorSelect::All
                    };
                });
                ui.end_row();
            });
        ui.add_space(4.0);
        changed |= ui
            .checkbox(&mut c.pause_when_covered, "Pause behind fullscreen apps")
            .changed();
        if cfg!(windows) && ui.checkbox(&mut autostart, "Start with Windows").changed() {
            self.set_autostart(autostart);
        }
        self.dirty |= changed;
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for p in dropped {
            self.add_to_library(p);
        }
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_thumbs(&ctx);
        self.handle_drops(&ctx);
        self.tick_preview();

        let bar = egui::Frame::new()
            .fill(theme::PANEL)
            .inner_margin(Margin::symmetric(20, 12));
        egui::Panel::top("top")
            .frame(bar)
            .show_separator_line(false)
            .show(ui, |ui| self.top_bar(ui));

        let side = egui::Frame::new()
            .fill(theme::PANEL)
            .inner_margin(Margin::same(16));
        egui::Panel::right("inspector")
            .frame(side)
            .resizable(false)
            .exact_size(INSPECTOR_W)
            .show_separator_line(false)
            .show(ui, |ui| {
                egui::Panel::bottom("apply")
                    .frame(egui::Frame::new().fill(theme::PANEL))
                    .show_separator_line(false)
                    .show(ui, |ui| self.apply_bar(ui));
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PANEL))
                    .show(ui, |ui| self.inspector(ui));
            });

        let main = egui::Frame::new().fill(theme::BG).inner_margin(Margin::same(20));
        egui::CentralPanel::default().frame(main).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| self.gallery(ui));
        });

        drop_overlay(&ctx);
        ctx.request_repaint_after(Duration::from_secs_f32(1.0 / PREVIEW_FPS));
    }
}

// ----- widgets ----------------------------------------------------------

fn primary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
        .fill(theme::ACCENT)
        .corner_radius(8)
        .min_size(egui::vec2(0.0, 34.0))
}

fn secondary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(text)
        .fill(theme::CARD)
        .corner_radius(8)
        .min_size(egui::vec2(0.0, 34.0))
}

fn status_pill(ui: &mut egui::Ui, running: bool) {
    let (color, text) = if running {
        (theme::GREEN, "Live")
    } else {
        (theme::MUTED, "Not running")
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(13.0), color);
    let size = egui::vec2(galley.size().x + 34.0, 28.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 14, color.gamma_multiply(0.14));
    p.circle_filled(egui::pos2(rect.left() + 15.0, rect.center().y), 4.0, color);
    p.galley(
        egui::pos2(rect.left() + 25.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

fn section_title(ui: &mut egui::Ui, title: &str, hint: Option<&str>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(17.0).strong());
        if let Some(h) = hint {
            ui.label(RichText::new(h).small().color(theme::MUTED));
        }
    });
    ui.add_space(2.0);
}

fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.5).strong().color(theme::MUTED));
}

fn card_grid(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(CARD_GAP, CARD_GAP);
        ui.horizontal_wrapped(add);
    });
}

fn card_size(w: f32) -> Vec2 {
    egui::vec2(w, w * THUMB_H as f32 / THUMB_W as f32 + 40.0)
}

/// A gallery card: thumbnail, title, optional badge; outlined when selected.
fn card(
    ui: &mut egui::Ui,
    w: f32,
    thumb: Option<egui::TextureId>,
    title: &str,
    badge: Option<&str>,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(card_size(w), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = response.hovered();
    let p = ui.painter();
    p.rect_filled(rect, 10, if hovered { theme::CARD_HOVER } else { theme::CARD });

    let img_rect = egui::Rect::from_min_size(rect.min, egui::vec2(w, w * THUMB_H as f32 / THUMB_W as f32));
    let top_corners = CornerRadius {
        nw: 10,
        ne: 10,
        sw: 0,
        se: 0,
    };
    match thumb {
        Some(id) => {
            let tint = if hovered {
                Color32::WHITE
            } else {
                Color32::from_gray(225)
            };
            egui::Image::new((id, img_rect.size()))
                .corner_radius(top_corners)
                .tint(tint)
                .paint_at(ui, img_rect);
        }
        None => {
            p.rect_filled(img_rect, top_corners, theme::BG);
            p.text(
                img_rect.center(),
                Align2::CENTER_CENTER,
                "…",
                FontId::proportional(22.0),
                theme::MUTED,
            );
        }
    }
    if let Some(b) = badge {
        let galley = p.layout_no_wrap(b.to_owned(), FontId::proportional(10.5), Color32::WHITE);
        let r = egui::Rect::from_min_size(
            img_rect.min + egui::vec2(8.0, 8.0),
            galley.size() + egui::vec2(12.0, 6.0),
        );
        let fill = if b == "MISSING" {
            theme::RED
        } else {
            Color32::from_black_alpha(170)
        };
        p.rect_filled(r, 6, fill);
        p.galley(r.min + egui::vec2(6.0, 3.0), galley, Color32::WHITE);
    }
    let title_pos = egui::pos2(rect.left() + 12.0, img_rect.bottom() + 20.0);
    let title = truncate(title, ((w - 24.0) / 7.5) as usize);
    p.text(
        title_pos,
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(14.0),
        theme::TEXT,
    );

    if selected {
        p.rect_stroke(rect, 10, Stroke::new(2.5, theme::ACCENT), StrokeKind::Outside);
    } else if hovered {
        p.rect_stroke(rect, 10, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
    }
    response
}

fn add_card(ui: &mut egui::Ui, w: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(card_size(w), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let color = if response.hovered() {
        theme::ACCENT_HOVER
    } else {
        theme::MUTED
    };
    let p = ui.painter();
    if response.hovered() {
        p.rect_filled(rect, 10, theme::ACCENT.gamma_multiply(0.08));
    }
    // Dashed outline.
    let r = rect.shrink(1.0);
    let corners = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    for pair in corners.windows(2) {
        p.extend(egui::Shape::dashed_line(
            &[pair[0], pair[1]],
            Stroke::new(1.5, color),
            6.0,
            5.0,
        ));
    }
    p.text(
        rect.center() - egui::vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        "+",
        FontId::proportional(34.0),
        color,
    );
    p.text(
        rect.center() + egui::vec2(0.0, 22.0),
        Align2::CENTER_CENTER,
        "Add video or image",
        FontId::proportional(14.0),
        color,
    );
    response
}

fn drop_overlay(ctx: &egui::Context) {
    if ctx.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let rect = ctx.content_rect();
    let p = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("drop"),
    ));
    p.rect_filled(rect, 0, Color32::from_black_alpha(190));
    p.rect_stroke(
        rect.shrink(16.0),
        16,
        Stroke::new(2.0, theme::ACCENT),
        StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "Drop to add to your wallpapers",
        FontId::proportional(24.0),
        theme::TEXT,
    );
}

/// Draw the right widget for one parameter. Returns whether it changed.
fn param_editor(ui: &mut egui::Ui, table: &mut toml::Table, key: &str, kind: &ParamKind) -> bool {
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
            changed
        }
        ParamKind::Int { min, max, default } => {
            let mut v = table.get(key).and_then(|v| v.as_integer()).unwrap_or(*default);
            let changed = ui.add(egui::Slider::new(&mut v, *min..=*max)).changed();
            if changed {
                table.insert(key.into(), toml::Value::Integer(v));
            }
            changed
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
            changed
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
                        .selectable_value(&mut current, (*opt).to_owned(), title_case(opt))
                        .changed();
                }
            });
            if changed {
                table.insert(key.into(), toml::Value::String(current));
            }
            changed
        }
        // Chosen through the gallery, never edited inline.
        ParamKind::Path => false,
    }
}

// ----- helpers ----------------------------------------------------------

fn frame_to_image(frame: &Frame) -> ColorImage {
    let pixels = frame
        .pixels
        .iter()
        .map(|&p| Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
        .collect();
    ColorImage::new([frame.width, frame.height], pixels)
}

/// Render each built-in scene once for its gallery card.
fn render_builtin_thumbs(ctx: &egui::Context) -> HashMap<&'static str, TextureHandle> {
    let mut out = HashMap::new();
    for info in SCENES.iter().filter(|s| !s.is_user_media()) {
        let Ok(mut scene) = scene::build(info.name, &Default::default()) else {
            continue;
        };
        let mut frame = Frame::new(THUMB_W, THUMB_H);
        // A few frames in, so stateful scenes (starfield) have settled.
        for i in 0..20 {
            let dt = 1.0 / 30.0;
            scene.render(
                &mut frame,
                &FrameCtx {
                    time: 6.0 + i as f32 * dt,
                    dt,
                    cursor: None,
                },
            );
        }
        out.insert(
            info.name,
            ctx.load_texture(info.name, frame_to_image(&frame), TextureOptions::LINEAR),
        );
    }
    out
}

/// Thumbnail for a library entry (runs on a worker thread).
fn load_thumb(path: &Path) -> Option<ColorImage> {
    match MediaKind::of(path) {
        MediaKind::Video => {
            let rgba = ffmpeg::thumbnail(path, THUMB_W, THUMB_H)
                .map_err(|e| log::warn!("{e:#}"))
                .ok()?;
            Some(ColorImage::from_rgba_unmultiplied([THUMB_W, THUMB_H], &rgba))
        }
        MediaKind::Image => image_thumb(path),
        MediaKind::Folder => {
            let mut entries: Vec<_> = std::fs::read_dir(path)
                .ok()?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .collect();
            entries.sort();
            entries
                .iter()
                .filter(|p| MediaKind::of(p) == MediaKind::Image)
                .find_map(|p| image_thumb(p))
        }
    }
}

fn image_thumb(path: &Path) -> Option<ColorImage> {
    let img = image::open(path).ok()?;
    let img = img.resize_to_fill(
        THUMB_W as u32,
        THUMB_H as u32,
        image::imageops::FilterType::Triangle,
    );
    let rgba = img.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [THUMB_W, THUMB_H],
        rgba.as_raw(),
    ))
}

fn display_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .or(path.file_name())
        .map(|s| s.to_string_lossy().into_owned());
    let stem = stem.unwrap_or_else(|| path.display().to_string());
    title_case(&stem.replace(['_', '-'], " "))
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max.max(4) {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max.max(4) - 1).collect::<String>())
    }
}

fn wrap_words(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = 0;
    for word in s.split_whitespace() {
        if line > 0 && line + word.len() > width {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += word.len();
    }
    out
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
