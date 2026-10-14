//! Choices remembered between runs, stored as JSON in the user's config folder.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::capture::Method;
use crate::encode::Format;
use crate::pack::ZipSplit;
use crate::recorder::Output;

pub const MIN_INTERVAL_MS: f64 = 10.0;
pub const MAX_INTERVAL_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    #[default]
    Seconds,
    Milliseconds,
}

impl Unit {
    pub fn label(self) -> &'static str {
        match self {
            Unit::Seconds => "seconds",
            Unit::Milliseconds => "milliseconds",
        }
    }

    fn millis_per_unit(self) -> f64 {
        match self {
            Unit::Seconds => 1000.0,
            Unit::Milliseconds => 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub folder: PathBuf,
    pub interval: f64,
    pub unit: Unit,
    pub format: Format,
    pub method: Method,
    pub output: Output,
    pub zip_split: ZipSplit,
    /// On Windows, the X button hides SnapLapse to the notification area instead of quitting.
    pub close_to_tray: bool,
    /// On Windows, a window dropped against a screen edge hides itself when the mouse leaves.
    pub edge_autohide: bool,
    /// Title of the last window recorded, used to preselect it next time.
    pub last_window: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        let base = dirs::picture_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        Settings {
            folder: base.join("SnapLapse"),
            interval: 5.0,
            unit: Unit::Seconds,
            format: Format::Png,
            method: Method::Auto,
            output: Output::Folder,
            zip_split: ZipSplit::Off,
            close_to_tray: true,
            edge_autohide: false,
            last_window: None,
        }
    }
}

impl Settings {
    pub fn interval_duration(&self) -> Duration {
        let ms =
            (self.interval * self.unit.millis_per_unit()).clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
        Duration::from_secs_f64(if ms.is_finite() { ms / 1000.0 } else { 5.0 })
    }

    /// Switches the unit while keeping the same interval, so 5 seconds becomes 5000 milliseconds.
    pub fn set_unit(&mut self, unit: Unit) {
        if unit != self.unit {
            self.interval = self.interval * self.unit.millis_per_unit() / unit.millis_per_unit();
            self.unit = unit;
        }
    }

    fn path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("SnapLapse").join("settings.json"))
    }

    pub fn load() -> Settings {
        Self::path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_uses_the_chosen_unit() {
        let mut s = Settings {
            interval: 2.5,
            unit: Unit::Seconds,
            ..Settings::default()
        };
        assert_eq!(s.interval_duration(), Duration::from_millis(2500));
        s.unit = Unit::Milliseconds;
        s.interval = 250.0;
        assert_eq!(s.interval_duration(), Duration::from_millis(250));
    }

    #[test]
    fn interval_is_kept_within_limits() {
        let s = Settings {
            interval: 1.0,
            unit: Unit::Milliseconds,
            ..Settings::default()
        };
        assert_eq!(s.interval_duration(), Duration::from_millis(10));
        let s = Settings {
            interval: f64::NAN,
            ..Settings::default()
        };
        assert_eq!(s.interval_duration(), Duration::from_secs(5));
    }

    #[test]
    fn switching_units_keeps_the_same_interval() {
        let mut s = Settings {
            interval: 5.0,
            unit: Unit::Seconds,
            ..Settings::default()
        };
        s.set_unit(Unit::Milliseconds);
        assert_eq!(s.interval, 5000.0);
        s.set_unit(Unit::Seconds);
        assert_eq!(s.interval, 5.0);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let s: Settings = serde_json::from_str(r#"{"interval": 3.0}"#).unwrap();
        assert_eq!(s.interval, 3.0);
        assert_eq!(s.format, Format::Png);
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn interval_is_always_usable(value in any::<f64>(), millis in any::<bool>()) {
                let unit = if millis { Unit::Milliseconds } else { Unit::Seconds };
                let s = Settings { interval: value, unit, ..Settings::default() };
                let d = s.interval_duration();
                prop_assert!(d >= Duration::from_millis(10));
                prop_assert!(d <= Duration::from_secs(24 * 60 * 60));
            }

            #[test]
            fn switching_units_and_back_keeps_the_value(value in 0.01f64..100_000.0) {
                let mut s = Settings { interval: value, unit: Unit::Seconds, ..Settings::default() };
                let before = s.interval_duration();
                s.set_unit(Unit::Milliseconds);
                prop_assert!((s.interval_duration().as_secs_f64() - before.as_secs_f64()).abs() < 1e-9);
                s.set_unit(Unit::Seconds);
                prop_assert!((s.interval - value).abs() < 1e-9 * value.max(1.0));
            }
        }
    }
}
