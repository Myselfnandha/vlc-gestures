use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub vlc: VlcConfig,
    #[serde(default)]
    pub touch_scroll: TouchScrollConfig,
    #[serde(default)]
    pub smart_tap: SmartTapConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            vlc: VlcConfig::default(),
            touch_scroll: TouchScrollConfig::default(),
            smart_tap: SmartTapConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VlcConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_hold_threshold")]
    pub hold_threshold_ms: u64,
    #[serde(default = "default_accel_rate")]
    pub accelerated_rate: f64,
    #[serde(default = "default_true")]
    pub enable_single_tap: bool,
    #[serde(default = "default_true")]
    pub enable_double_click_fullscreen: bool,
}

fn default_true() -> bool {
    true
}

fn default_hold_threshold() -> u64 {
    350
}

fn default_accel_rate() -> f64 {
    1.5
}

impl Default for VlcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hold_threshold_ms: default_hold_threshold(),
            accelerated_rate: default_accel_rate(),
            enable_single_tap: true,
            enable_double_click_fullscreen: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchScrollConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub natural_scroll: bool,
    #[serde(default = "default_scroll_step")]
    pub scroll_step_px: i32,
    #[serde(default = "default_scroll_threshold")]
    pub scroll_threshold_px: i32,
    #[serde(default = "default_true")]
    pub enable_two_finger_scroll: bool,
    #[serde(default = "default_true")]
    pub enable_pinch_zoom: bool,
}

fn default_scroll_step() -> i32 {
    25
}

fn default_scroll_threshold() -> i32 {
    20
}

impl Default for TouchScrollConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            natural_scroll: true,
            scroll_step_px: default_scroll_step(),
            scroll_threshold_px: default_scroll_threshold(),
            enable_two_finger_scroll: true,
            enable_pinch_zoom: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartTapConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_trigger_button")]
    pub trigger_button: u8,
    #[serde(default = "default_device_pattern")]
    pub device_name_pattern: String,
}

fn default_trigger_button() -> u8 {
    20
}

fn default_device_pattern() -> String {
    "SYNA30B3".to_string()
}

impl Default for SmartTapConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger_button: default_trigger_button(),
            device_name_pattern: default_device_pattern(),
        }
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".config/vlc-gestures/config.toml")
    }

    pub fn load_or_create(path: &Path) -> Self {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(cfg) = toml::from_str::<Config>(&content) {
                    return cfg;
                }
            }
        }

        let cfg = Config::default();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(serialized) = toml::to_string_pretty(&cfg) {
            let _ = std::fs::write(path, serialized);
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = Config::default();
        assert!(cfg.vlc.enabled);
        assert_eq!(cfg.vlc.hold_threshold_ms, 350);
        assert_eq!(cfg.vlc.accelerated_rate, 1.5);
        assert!(cfg.touch_scroll.enabled);
        assert!(cfg.smart_tap.enabled);
        assert_eq!(cfg.smart_tap.trigger_button, 20);
    }

    #[test]
    fn test_toml_serialization() {
        let cfg = Config::default();
        let serialized = toml::to_string(&cfg).expect("Should serialize to toml");
        let parsed: Config = toml::from_str(&serialized).expect("Should deserialize from toml");
        assert_eq!(parsed.vlc.hold_threshold_ms, 350);
        assert_eq!(parsed.touch_scroll.scroll_step_px, 25);
    }
}
