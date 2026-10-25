//! Scenes: the things that draw wallpapers.

mod aurora;
mod image;
mod lava;
mod plasma;
mod starfield;
mod synthwave;

use anyhow::{bail, Result};

use crate::render::{Frame, FrameCtx};

/// A wallpaper scene. One instance is created per monitor.
pub trait Scene: Send {
    /// Draw one frame into `frame` (already sized to the render resolution).
    fn render(&mut self, frame: &mut Frame, ctx: &FrameCtx);

    /// False while the scene is still loading assets in the background.
    fn is_ready(&self) -> bool {
        true
    }
}

/// Static description of a built-in scene.
pub struct SceneInfo {
    pub name: &'static str,
    pub description: &'static str,
    build: fn(&Params) -> Result<Box<dyn Scene>>,
}

pub const SCENES: &[SceneInfo] = &[
    SceneInfo {
        name: "aurora",
        description: "Northern lights rippling over a starry sky",
        build: |p| Ok(Box::new(aurora::Aurora::new(p))),
    },
    SceneInfo {
        name: "plasma",
        description: "Classic demoscene plasma with a cycling cosine palette",
        build: |p| Ok(Box::new(plasma::Plasma::new(p))),
    },
    SceneInfo {
        name: "starfield",
        description: "Flying through space; set warp > 0 for light-speed streaks",
        build: |p| Ok(Box::new(starfield::Starfield::new(p))),
    },
    SceneInfo {
        name: "lava",
        description: "Lava lamp metaballs drifting and merging",
        build: |p| Ok(Box::new(lava::Lava::new(p))),
    },
    SceneInfo {
        name: "synthwave",
        description: "Retro sunset with a scrolling neon grid",
        build: |p| Ok(Box::new(synthwave::Synthwave::new(p))),
    },
    SceneInfo {
        name: "image",
        description: "Static image or slideshow with crossfades and Ken Burns zoom",
        build: |p| Ok(Box::new(image::ImageScene::new(p)?)),
    },
];

pub fn build(name: &str, params: &Params) -> Result<Box<dyn Scene>> {
    match SCENES.iter().find(|s| s.name.eq_ignore_ascii_case(name)) {
        Some(info) => (info.build)(params),
        None => {
            let names: Vec<_> = SCENES.iter().map(|s| s.name).collect();
            bail!("unknown scene {name:?}; available: {}", names.join(", "))
        }
    }
}

/// Typed access to a scene's `[scenes.<name>]` table, with defaults.
#[derive(Clone, Debug, Default)]
pub struct Params(toml::Table);

impl Params {
    pub fn new(table: toml::Table) -> Self {
        Self(table)
    }

    pub fn f32(&self, key: &str, default: f32) -> f32 {
        match self.0.get(key) {
            Some(toml::Value::Float(f)) if f.is_finite() => *f as f32,
            Some(toml::Value::Integer(i)) => *i as f32,
            Some(other) => {
                log::warn!("parameter {key:?} should be a number, got {other}; using {default}");
                default
            }
            None => default,
        }
    }

    pub fn usize(&self, key: &str, default: usize, max: usize) -> usize {
        (self.f32(key, default as f32).max(0.0) as usize).min(max)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.as_str())
    }

    /// Parse `"#rrggbb"` / `"rrggbb"` / `"#rgb"` into `0..1` floats.
    pub fn color(&self, key: &str, default: [f32; 3]) -> [f32; 3] {
        match self.str(key) {
            Some(s) => parse_hex_color(s).unwrap_or_else(|| {
                log::warn!("parameter {key:?}: {s:?} is not a #rrggbb color");
                default
            }),
            None => default,
        }
    }
}

pub fn parse_hex_color(s: &str) -> Option<[f32; 3]> {
    let hex = s.trim().trim_start_matches('#');
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    let [r, g, b] = match digits.as_slice() {
        [r, g, b] => [r * 17, g * 17, b * 17],
        [r1, r0, g1, g0, b1, b0] => [r1 * 16 + r0, g1 * 16 + g0, b1 * 16 + b0],
        _ => return None,
    };
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#ffffff"), Some([1.0, 1.0, 1.0]));
        assert_eq!(parse_hex_color("000"), Some([0.0, 0.0, 0.0]));
        assert_eq!(parse_hex_color("#f00").map(|c| c[0]), Some(1.0));
        assert_eq!(parse_hex_color("#12345"), None);
        assert_eq!(parse_hex_color("#gg0000"), None);
    }

    #[test]
    fn params_defaults_and_types() {
        let p = Params::new(toml::from_str("a = 2\nb = 0.5\nc = \"x\"").unwrap());
        assert_eq!(p.f32("a", 0.0), 2.0);
        assert_eq!(p.f32("b", 0.0), 0.5);
        assert_eq!(p.f32("c", 9.0), 9.0);
        assert_eq!(p.f32("missing", 3.0), 3.0);
        assert_eq!(p.usize("a", 0, 1), 1);
    }

    #[test]
    fn unknown_scene_errors() {
        assert!(build("nope", &Params::default()).is_err());
    }

    /// Every procedural scene should draw something that isn't a flat color,
    /// and should be deterministic for a given time.
    #[test]
    fn procedural_scenes_render() {
        for info in SCENES.iter().filter(|s| s.name != "image") {
            let render = || {
                let mut scene = build(info.name, &Params::default()).unwrap();
                let mut frame = Frame::new(96, 54);
                for i in 0..3 {
                    let ctx = FrameCtx {
                        time: 1.5 + i as f32 / 30.0,
                        dt: 1.0 / 30.0,
                        cursor: None,
                    };
                    scene.render(&mut frame, &ctx);
                }
                frame.pixels
            };
            let a = render();
            let first = a[0];
            assert!(
                a.iter().any(|&p| p != first),
                "{} rendered a flat frame",
                info.name
            );
            assert_eq!(a, render(), "{} is not deterministic", info.name);
        }
    }
}
