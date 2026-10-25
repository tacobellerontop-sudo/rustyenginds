//! User configuration (`config.toml`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Which monitors get a wallpaper window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MonitorSelect {
    /// `"all"` (the default).
    #[default]
    All,
    /// A single monitor index, as printed by `rusty-wallpaper monitors`.
    Index(usize),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
    #[serde(with = "monitor_serde")]
    pub monitor: MonitorSelect,
    /// Stop rendering on a monitor while a fullscreen or maximized app hides
    /// it (Windows).
    pub pause_when_covered: bool,
    /// Per-scene parameters, e.g. `[scenes.aurora] speed = 0.5`.
    pub scenes: BTreeMap<String, toml::Table>,
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
            scenes: BTreeMap::new(),
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

    /// Write the config, e.g. from the settings UI. A running engine picks the
    /// change up live. Comments in a hand-edited file are not preserved.
    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self).context("serializing config")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write-then-rename so the engine never reloads a half-written file.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(
            &tmp,
            format!("# Saved by the rusty-wallpaper settings window.\n\n{body}"),
        )
        .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    pub fn params_for(&self, scene: &str) -> crate::scene::Params {
        crate::scene::Params::new(self.scenes.get(scene).cloned().unwrap_or_default())
    }
}

/// `monitor = "all"` or `monitor = 1`.
mod monitor_serde {
    use super::MonitorSelect;
    use serde::{de, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(m: &MonitorSelect, s: S) -> Result<S::Ok, S::Error> {
        match m {
            MonitorSelect::All => s.serialize_str("all"),
            MonitorSelect::Index(i) => s.serialize_u64(*i as u64),
        }
    }

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
    fn save_roundtrips() {
        let mut cfg = Config::parse(EXAMPLE_CONFIG).unwrap();
        cfg.monitor = MonitorSelect::Index(1);
        cfg.fps = 24;
        let path = std::env::temp_dir().join(format!("rusty-wallpaper-cfg-{}.toml", std::process::id()));
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("fsp = 30").is_err());
    }
}
