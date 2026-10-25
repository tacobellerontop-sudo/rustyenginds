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
    /// The `[scenes.<name>]` keys this scene reads, for the settings UI.
    pub params: &'static [ParamSpec],
    build: fn(&Params) -> Result<Box<dyn Scene>>,
}

/// One tweakable scene parameter. Defaults mirror the ones scenes use.
pub struct ParamSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
}

pub enum ParamKind {
    Float {
        min: f32,
        max: f32,
        default: f32,
    },
    Int {
        min: i64,
        max: i64,
        default: i64,
    },
    Color {
        default: &'static str,
    },
    Path,
    Choice {
        options: &'static [&'static str],
        default: &'static str,
    },
}

const fn float(key: &'static str, label: &'static str, min: f32, max: f32, default: f32) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Float { min, max, default },
    }
}

const fn int(key: &'static str, label: &'static str, min: i64, max: i64, default: i64) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Int { min, max, default },
    }
}

const fn color(key: &'static str, label: &'static str, default: &'static str) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Color { default },
    }
}

const SPEED: ParamSpec = float("speed", "Speed", 0.0, 3.0, 1.0);

pub const SCENES: &[SceneInfo] = &[
    SceneInfo {
        name: "aurora",
        description: "Northern lights rippling over a starry sky",
        params: &[
            SPEED,
            float("intensity", "Intensity", 0.0, 3.0, 1.0),
            color("color_a", "Lower color", "#22ff99"),
            color("color_b", "Upper color", "#7a3cff"),
            float("stars", "Stars", 0.0, 2.0, 1.0),
        ],
        build: |p| Ok(Box::new(aurora::Aurora::new(p))),
    },
    SceneInfo {
        name: "plasma",
        description: "Classic demoscene plasma with a cycling cosine palette",
        params: &[
            SPEED,
            float("scale", "Scale", 1.0, 20.0, 8.0),
            float("palette_shift", "Palette shift", 0.0, 1.0, 0.0),
        ],
        build: |p| Ok(Box::new(plasma::Plasma::new(p))),
    },
    SceneInfo {
        name: "starfield",
        description: "Flying through space; set warp > 0 for light-speed streaks",
        params: &[
            int("count", "Stars", 50, 5000, 900),
            SPEED,
            float("warp", "Warp", 0.0, 3.0, 0.0),
        ],
        build: |p| Ok(Box::new(starfield::Starfield::new(p))),
    },
    SceneInfo {
        name: "lava",
        description: "Lava lamp metaballs drifting and merging",
        params: &[
            int("blobs", "Blobs", 1, 20, 7),
            SPEED,
            color("color_a", "Edge color", "#ff4d00"),
            color("color_b", "Core color", "#ffd000"),
            color("background", "Background", "#1a0633"),
        ],
        build: |p| Ok(Box::new(lava::Lava::new(p))),
    },
    SceneInfo {
        name: "synthwave",
        description: "Retro sunset with a scrolling neon grid",
        params: &[
            SPEED,
            color("sun", "Sun", "#ff2a6d"),
            color("grid", "Grid", "#05d9e8"),
        ],
        build: |p| Ok(Box::new(synthwave::Synthwave::new(p))),
    },
    SceneInfo {
        name: "image",
        description: "Static image or slideshow with crossfades and Ken Burns zoom",
        params: &[
            ParamSpec {
                key: "path",
                label: "Image or folder",
                kind: ParamKind::Path,
            },
            float("interval", "Seconds per image", 1.0, 600.0, 60.0),
            float("fade", "Crossfade (s)", 0.0, 10.0, 2.0),
            float("zoom", "Zoom", 0.0, 0.5, 0.08),
            ParamSpec {
                key: "fit",
                label: "Fit",
                kind: ParamKind::Choice {
                    options: &["cover", "contain"],
                    default: "cover",
                },
            },
        ],
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

    /// The UI's parameter list and the documented example config must agree.
    #[test]
    fn param_specs_match_example_config() {
        let cfg = crate::config::Config::parse(crate::config::EXAMPLE_CONFIG).unwrap();
        for info in SCENES {
            let mut documented: Vec<_> = cfg.scenes[info.name].keys().map(String::as_str).collect();
            let mut spec: Vec<_> = info.params.iter().map(|p| p.key).collect();
            documented.sort();
            spec.sort();
            assert_eq!(documented, spec, "{}", info.name);
        }
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
