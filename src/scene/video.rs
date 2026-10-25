//! Looping video wallpaper, decoded by ffmpeg.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::ffmpeg::{Decoder, Fit};
use crate::render::{Frame, FrameCtx};
use crate::scene::{expand_tilde, Params, Scene};

pub struct VideoScene {
    path: PathBuf,
    fps: f32,
    speed: f32,
    fit: Fit,
    decoder: Option<Decoder>,
    size: (usize, usize),
    /// Seconds of video played so far (advances only while we render).
    clock: f64,
    /// Frames taken from the decoder so far.
    shown: u64,
    current: Option<Vec<u32>>,
    failed: bool,
}

impl VideoScene {
    pub fn new(p: &Params) -> Result<Self> {
        let Some(raw) = p.str("path").filter(|s| !s.trim().is_empty()) else {
            bail!("choose a video file for this wallpaper")
        };
        let path = expand_tilde(raw);
        if !path.is_file() {
            bail!("video {} does not exist", path.display());
        }
        let fit = match p.str("fit").unwrap_or("cover") {
            "contain" => Fit::Contain,
            _ => Fit::Cover,
        };
        Ok(Self {
            path,
            fps: p.f32("fps", 30.0).clamp(1.0, 120.0),
            speed: p.f32("speed", 1.0).clamp(0.05, 8.0),
            fit,
            decoder: None,
            size: (0, 0),
            clock: 0.0,
            shown: 0,
            current: None,
            failed: false,
        })
    }

    fn restart(&mut self, w: usize, h: usize) {
        self.size = (w, h);
        self.current = None;
        self.clock = 0.0;
        self.shown = 0;
        match Decoder::start(&self.path, w, h, self.fps, self.fit) {
            Ok(d) => self.decoder = Some(d),
            Err(e) => {
                log::error!("{e:#}");
                self.decoder = None;
                self.failed = true;
            }
        }
    }
}

impl Scene for VideoScene {
    fn native_resolution(&self) -> bool {
        true
    }

    fn is_ready(&self) -> bool {
        self.current.is_some() || self.failed
    }

    fn render(&mut self, frame: &mut Frame, ctx: &FrameCtx) {
        let size = (frame.width, frame.height);
        if size != self.size && !self.failed {
            self.restart(size.0, size.1);
        }

        // Our own clock, so a long pause (covered by a game) resumes the video
        // where it was instead of fast-forwarding through everything missed.
        self.clock += (ctx.dt.min(0.1) * self.speed) as f64;
        let target = (self.clock * self.fps as f64) as u64;
        if target.saturating_sub(self.shown) > self.fps as u64 {
            // The decoder can't keep up; stop trying to catch up.
            self.shown = target.saturating_sub(1);
        }
        while self.shown < target || self.current.is_none() {
            let Some(decoder) = &self.decoder else { break };
            match decoder.try_next() {
                Some(Ok(f)) => {
                    self.current = Some(f);
                    self.shown += 1;
                }
                Some(Err(e)) => {
                    log::error!("{}: {e:#}", self.path.display());
                    self.decoder = None;
                    self.failed = true;
                }
                None => break,
            }
        }

        match &self.current {
            Some(px) if px.len() == frame.pixels.len() => frame.pixels.copy_from_slice(px),
            _ => frame.pixels.fill(0x0b0b10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Make a short test clip with ffmpeg itself; None if ffmpeg isn't installed.
    fn test_clip(name: &str) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("rusty-wallpaper-video-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("clip.mp4");
        let ok = Command::new(crate::ffmpeg::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=320x180:rate=30:duration=1")
            .args(["-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("ffmpeg not available; skipping video test");
            return None;
        }
        Some(path)
    }

    fn params(path: &std::path::Path) -> Params {
        Params::new(toml::from_str(&format!("path = {:?}", path.display().to_string())).unwrap())
    }

    #[test]
    fn plays_video_frames() {
        let Some(clip) = test_clip("play") else { return };
        let mut scene = VideoScene::new(&params(&clip)).unwrap();
        let mut frame = Frame::new(64, 36);
        for _ in 0..400 {
            scene.render(
                &mut frame,
                &FrameCtx {
                    time: 0.0,
                    dt: 1.0 / 30.0,
                    cursor: None,
                },
            );
            if scene.is_ready() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(scene.current.is_some(), "no frame decoded");
        let first = frame.pixels[0];
        assert!(frame.pixels.iter().any(|&p| p != first), "decoded a flat frame");
        let _ = std::fs::remove_dir_all(clip.parent().unwrap());
    }

    #[test]
    fn thumbnail_has_expected_size() {
        let Some(clip) = test_clip("thumb") else { return };
        let rgba = crate::ffmpeg::thumbnail(&clip, 32, 18).unwrap();
        assert_eq!(rgba.len(), 32 * 18 * 4);
        let _ = std::fs::remove_dir_all(clip.parent().unwrap());
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(VideoScene::new(&params(std::path::Path::new("/no/such/video.mp4"))).is_err());
        assert!(VideoScene::new(&Params::default()).is_err());
    }
}
