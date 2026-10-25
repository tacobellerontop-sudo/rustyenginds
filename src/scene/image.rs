//! Image wallpaper / slideshow with crossfades and a slow Ken Burns zoom.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use image::RgbImage;

use crate::render::math::*;
use crate::render::{pack3, Frame, FrameCtx};
use crate::scene::{expand_tilde, Params, Scene};

/// Larger sources are downscaled on load; nobody needs a 50MP wallpaper in RAM.
const MAX_SOURCE_DIM: u32 = 3840;
const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif"];

#[derive(Clone, Copy, PartialEq)]
enum Fit {
    Cover,
    Contain,
}

struct Shown {
    image: Arc<RgbImage>,
    since: f32,
    /// Alternate zoom direction between slides so it doesn't feel mechanical.
    zoom_out: bool,
    /// Normalized pan target for the Ken Burns drift.
    pan: (f32, f32),
}

pub struct ImageScene {
    paths: Vec<PathBuf>,
    next_index: usize,
    interval: f32,
    fade: f32,
    zoom: f32,
    fit: Fit,
    current: Option<Shown>,
    previous: Option<Shown>,
    pending: Option<Receiver<Result<RgbImage>>>,
    shown_count: u32,
}

impl ImageScene {
    pub fn new(p: &Params) -> Result<Self> {
        let Some(raw) = p.str("path") else {
            bail!("the image scene needs `path` set under [scenes.image]")
        };
        let path = expand_tilde(raw);
        let paths = collect_images(&path)?;
        let fit = match p.str("fit").unwrap_or("cover") {
            "contain" => Fit::Contain,
            "cover" => Fit::Cover,
            other => {
                log::warn!("unknown fit {other:?}, using \"cover\"");
                Fit::Cover
            }
        };
        log::info!("image scene: {} image(s) from {}", paths.len(), path.display());
        Ok(Self {
            paths,
            next_index: 0,
            interval: p.f32("interval", 60.0).max(1.0),
            fade: p.f32("fade", 2.0).max(0.0),
            zoom: p.f32("zoom", 0.08).clamp(0.0, 1.0),
            fit,
            current: None,
            previous: None,
            pending: None,
            shown_count: 0,
        })
    }

    fn request_next(&mut self) {
        let path = self.paths[self.next_index].clone();
        self.next_index = (self.next_index + 1) % self.paths.len();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_image(&path));
        });
        self.pending = Some(rx);
    }

    fn poll_loader(&mut self, now: f32) {
        let Some(rx) = &self.pending else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("image loader thread died")),
        };
        self.pending = None;
        match result {
            Ok(img) => {
                let n = self.shown_count;
                self.shown_count += 1;
                self.previous = self.current.take();
                self.current = Some(Shown {
                    image: Arc::new(img),
                    since: now,
                    zoom_out: n % 2 == 1,
                    pan: (hash_u32(n * 2) - 0.5, hash_u32(n * 2 + 1) - 0.5),
                });
            }
            Err(e) => {
                log::error!("{e:#}");
                // Try the next one on the following frame (single bad file in a folder).
                if self.paths.len() == 1 {
                    // Nothing else to try; avoid hammering the disk every frame.
                    self.paths.clear();
                }
            }
        }
    }
}

impl Scene for ImageScene {
    fn is_ready(&self) -> bool {
        self.current.is_some() || self.paths.is_empty()
    }

    fn native_resolution(&self) -> bool {
        true
    }

    fn render(&mut self, frame: &mut Frame, ctx: &FrameCtx) {
        let now = ctx.time;
        self.poll_loader(now);
        if self.pending.is_none() && !self.paths.is_empty() {
            let due = match &self.current {
                None => true,
                Some(c) => self.paths.len() > 1 && now - c.since >= self.interval,
            };
            if due {
                self.request_next();
            }
        }

        let Some(cur) = &self.current else {
            frame.pixels.fill(0x101014);
            return;
        };
        let fade_t = if self.fade > 0.0 {
            ((now - cur.since) / self.fade).clamp(0.0, 1.0)
        } else {
            1.0
        };
        if fade_t >= 1.0 {
            self.previous = None;
        }

        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let a = self.mapping(cur, fw, fh, now);
        let b = self.previous.as_ref().map(|p| (p, self.mapping(p, fw, fh, now)));
        let fade = smoothstep(0.0, 1.0, fade_t);

        frame.par_shade(|x, y| {
            let ca = a.sample(&cur.image, x, y);
            let c = match &b {
                Some((prev, m)) if fade < 1.0 => mix3(m.sample(&prev.image, x, y), ca, fade),
                _ => ca,
            };
            pack3(c)
        });
    }
}

/// Screen -> source pixel transform: `src = (screen - centre) * scale + origin`.
struct Mapping {
    scale: f32,
    centre: (f32, f32),
    origin: (f32, f32),
}

impl ImageScene {
    fn mapping(&self, s: &Shown, fw: f32, fh: f32, now: f32) -> Mapping {
        let (iw, ih) = (s.image.width() as f32, s.image.height() as f32);
        let fit = match self.fit {
            Fit::Cover => (iw / fw).min(ih / fh),
            Fit::Contain => (iw / fw).max(ih / fh),
        };
        // Progress through this slide's lifetime drives the zoom and pan.
        let life = if self.paths.len() > 1 {
            self.interval + self.fade
        } else {
            120.0
        };
        let mut p = ((now - s.since) / life).clamp(0.0, 1.0);
        if self.paths.len() <= 1 {
            // Single image: gently breathe back and forth forever.
            p = 0.5 - 0.5 * ((now - s.since) / life * std::f32::consts::PI).cos();
        }
        let p = if s.zoom_out { 1.0 - p } else { p };
        let zoom = 1.0 + self.zoom * p;
        let scale = fit / zoom;
        // Pan only within the slack the zoom creates so edges never show.
        let slack_x = (iw - fw * scale).max(0.0) * 0.5;
        let slack_y = (ih - fh * scale).max(0.0) * 0.5;
        Mapping {
            scale,
            centre: (fw * 0.5, fh * 0.5),
            origin: (iw * 0.5 + s.pan.0 * slack_x * p, ih * 0.5 + s.pan.1 * slack_y * p),
        }
    }
}

impl Mapping {
    fn sample(&self, img: &RgbImage, x: f32, y: f32) -> [f32; 3] {
        let sx = (x - self.centre.0) * self.scale + self.origin.0 - 0.5;
        let sy = (y - self.centre.1) * self.scale + self.origin.1 - 0.5;
        let (w, h) = (img.width() as f32, img.height() as f32);
        if sx < -0.5 || sy < -0.5 || sx > w - 0.5 || sy > h - 0.5 {
            return [0.0, 0.0, 0.0]; // letterbox for "contain"
        }
        let sx = sx.clamp(0.0, w - 1.0);
        let sy = sy.clamp(0.0, h - 1.0);
        let (x0, y0) = (sx as u32, sy as u32);
        let (x1, y1) = ((x0 + 1).min(img.width() - 1), (y0 + 1).min(img.height() - 1));
        let (tx, ty) = (sx - x0 as f32, sy - y0 as f32);
        let px = |x, y| {
            let p = img.get_pixel(x, y).0;
            [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]
        };
        mix3(
            mix3(px(x0, y0), px(x1, y0), tx),
            mix3(px(x0, y1), px(x1, y1), tx),
            ty,
        )
    }
}

fn load_image(path: &Path) -> Result<RgbImage> {
    let img = image::open(path).with_context(|| format!("loading {}", path.display()))?;
    let img = if img.width().max(img.height()) > MAX_SOURCE_DIM {
        img.resize(
            MAX_SOURCE_DIM,
            MAX_SOURCE_DIM,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    Ok(img.to_rgb8())
}

fn collect_images(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        bail!("image path {} does not exist", path.display());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(path)
        .with_context(|| format!("listing {}", path.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    paths.sort();
    if paths.is_empty() {
        bail!(
            "no images ({}) found in {}",
            EXTENSIONS.join(", "),
            path.display()
        );
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rusty-wallpaper-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slideshow_loads_and_shows_image() {
        let dir = scratch_dir("slideshow");
        RgbImage::from_pixel(40, 20, image::Rgb([255, 0, 0]))
            .save(dir.join("a.png"))
            .unwrap();
        RgbImage::from_pixel(40, 20, image::Rgb([0, 0, 255]))
            .save(dir.join("b.png"))
            .unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

        let table: toml::Table = toml::from_str(&format!(
            "path = {:?}\ninterval = 1.0\nfade = 0.0",
            dir.display().to_string()
        ))
        .unwrap();
        let mut scene = ImageScene::new(&Params::new(table)).unwrap();
        assert_eq!(scene.paths.len(), 2);

        let mut frame = Frame::new(16, 9);
        let mut render_until = |scene: &mut ImageScene, t: f32, want: u32| {
            for _ in 0..500 {
                scene.render(
                    &mut frame,
                    &FrameCtx {
                        time: t,
                        dt: 0.016,
                        cursor: None,
                    },
                );
                if frame.pixels[8 * 16 + 8] == want {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("never showed {want:06x}, got {:06x}", frame.pixels[8 * 16 + 8]);
        };
        render_until(&mut scene, 0.0, 0xFF0000);
        render_until(&mut scene, 2.0, 0x0000FF);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_path_is_an_error() {
        assert!(ImageScene::new(&Params::default()).is_err());
        let table: toml::Table = toml::from_str("path = \"/definitely/not/here\"").unwrap();
        assert!(ImageScene::new(&Params::new(table)).is_err());
    }
}
