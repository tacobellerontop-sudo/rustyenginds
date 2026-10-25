//! User configuration (`config.toml`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Which monitors get a wallpaper window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MonitorSelect {
    /// `"all"` (the default).
    #[default]
    All,
    /// A single monitor index, as printed by `rusty-wallpaper monitors`.
    Index(usize),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Scene to display; see `rusty-wallpaper list`.
    pub scene: String,
    /// Frame rate cap.
    pub fps: u32,
    /// Internal resolution as a fraction of the screen (0.1..=1.0).
    /// Lower is cheaper; the result is smoothly upscaled.
    pub render_scale: f32,
    /// Speed multiplier applied to every scene's clock.
    pub time_scale: f32,
    #[serde(deserialize_with = "monitor_serde::deserialize")]
    pub monitor: MonitorSelect,
    /// Stop rendering on a monitor while a fullscreen or maximized app hides
    /// it (Windows).
    pub pause_when_covered: bool,
    /// Per-scene parameters, e.g. `[scenes.aurora] speed = 0.5`.
    pub scenes: HashMap<String, toml::Table>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scene: "aurora".into(),
            fps: 30,
            render_scale: 0.5,
            time_scale: 1.0,
            monitor: MonitorSelect::All,
            pause_when_covered: true,
            scenes: HashMap::new(),
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("rusty-wallpaper").join("config.toml"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing config {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg: Config = toml::from_str(text)?;
        cfg.sanitize();
        Ok(cfg)
    }

    /// Clamp values into ranges the engine can actually honour.
    pub fn sanitize(&mut self) {
        self.fps = self.fps.clamp(1, 240);
        if !self.render_scale.is_finite() {
            self.render_scale = 0.5;
        }
        self.render_scale = self.render_scale.clamp(0.1, 1.0);
        if !self.time_scale.is_finite() {
            self.time_scale = 1.0;
        }
    }

    pub fn params_for(&self, scene: &str) -> crate::scene::Params {
        crate::scene::Params::new(self.scenes.get(scene).cloned().unwrap_or_default())
    }
}

/// `monitor = "all"` or `monitor = 1`.
mod monitor_serde {
    use super::MonitorSelect;
    use serde::{de, Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<MonitorSelect, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Int(i64),
        }
        match Raw::deserialize(d)? {
            Raw::Str(s) if s.eq_ignore_ascii_case("all") => Ok(MonitorSelect::All),
            Raw::Str(s) => s
                .parse()
                .map(MonitorSelect::Index)
                .map_err(|_| de::Error::custom(format!("monitor must be \"all\" or an index, got {s:?}"))),
            Raw::Int(i) if i >= 0 => Ok(MonitorSelect::Index(i as usize)),
            Raw::Int(i) => Err(de::Error::custom(format!("monitor index must be >= 0, got {i}"))),
        }
    }
}

/// The config written by `rusty-wallpaper init`.
pub const EXAMPLE_CONFIG: &str = include_str!("../examples/config.toml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let cfg = Config::parse(EXAMPLE_CONFIG).unwrap();
        assert_eq!(cfg.scene, "aurora");
        assert!(cfg.scenes.contains_key("image"));
    }

    #[test]
    fn empty_config_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn values_are_clamped() {
        let cfg = Config::parse("fps = 0\nrender_scale = 7.0").unwrap();
        assert_eq!(cfg.fps, 1);
        assert_eq!(cfg.render_scale, 1.0);
    }

    #[test]
    fn monitor_forms() {
        assert_eq!(
            Config::parse("monitor = \"all\"").unwrap().monitor,
            MonitorSelect::All
        );
        assert_eq!(
            Config::parse("monitor = 2").unwrap().monitor,
            MonitorSelect::Index(2)
        );
        assert!(Config::parse("monitor = -1").is_err());
        assert!(Config::parse("monitor = \"left\"").is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("fsp = 30").is_err());
    }
}
