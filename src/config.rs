//! User configuration: %APPDATA%\WindowsStats\config.toml

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Bumped when a stored value changes meaning; `load` migrates older files.
pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// absent in files written before versioning (winstats): 0, so `sanitize` migrates them
    #[serde(default)]
    pub config_version: u32,
    /// Where the strip goes: "right" (left of the system tray), "left" (left edge of the taskbar)
    pub position: String,
    /// Extra pixels (DIP) to shift the strip away from the edge it is attached to.
    pub offset: i32,
    /// Widgets in display order.
    pub widgets: Vec<String>,
    /// Refresh interval in milliseconds.
    pub update_ms: u32,
    /// Draw a 3-letter vertical label next to chart widgets.
    pub labels: bool,
    /// Colour cpu bars by utilisation (green/yellow/red) instead of a single accent colour.
    pub utilization_colors: bool,
    /// Drive letter for the disk widget free-space readout.
    pub disk: String,
    /// Accent colour: palette name ("system", "blue", "green", ...) or hex like "#2E7CF6".
    pub accent: String,
    /// Per-module colour mode: module (cpu, gpu, mem, disk, net) -> "accent" | "utilization" | "mono" | palette name | hex.
    pub colors: BTreeMap<String, String>,
    /// Force theme: "auto" | "dark" | "light"
    pub theme: String,
    /// Popup background: "more" (thinner than the system material) | "system" | "less" | "solid"
    pub popup_opacity: String,
    /// Text size multiplier for widgets: 0.85 | 1.0 | 1.15 | 1.3
    pub font_scale: f32,
    /// CPU load the way Task Manager shows it (% Processor Utility: busy time scaled by the
    /// current clock); false = plain busy time (% Processor Time).
    pub cpu_utility: bool,
    /// Show P-cores first, then E-cores, instead of the hardware order Windows uses.
    pub group_pe_cores: bool,
    /// Draw a divider between P and E cores in the bar chart and the popup.
    pub split_pe_cores: bool,
    /// Width of line/network/disk chart widgets in DIP: 34 | 48 | 64 | 96
    pub chart_width: u32,
    /// Width of one core bar in the CPU bar chart: "small" | "medium" | "large"
    pub bar_width: String,
    /// Rows in the popup process lists: 5 | 8 | 10 | 15 | 20
    pub top_processes: u32,
    /// Vertical taskbar: chart widgets (by name, e.g. "cpu_line") drawn across the full width
    /// of the taskbar, which shows a longer history.
    pub stretched: Vec<String>,
    /// UI language: "auto" (Windows UI language) or a locale code such as "tr", "en".
    pub language: String,
    /// Network interfaces (by alias, e.g. "Wi-Fi") left out of the widget and the chart.
    pub net_disabled: Vec<String>,
    /// Physical disks (by model) left out of the widget and the chart.
    pub disks_disabled: Vec<String>,
    /// Series drawn in the network popup chart (clicking Download / Upload toggles them).
    pub net_chart_rx: bool,
    pub net_chart_tx: bool,
    /// Host the network popup pings for the latency and jitter rows. Empty (the default) keeps
    /// the probe off: Windows Stats sends nothing to the network unless asked to.
    pub ping_host: String,
    /// How the strip is hosted: "auto" (embedded in the taskbar), "embedded" (child of the
    /// taskbar) or "floating" (always-on-top window over the taskbar).
    pub mode: String,
    /// Threshold notifications ("[[alerts]]"); the presets are all off.
    pub alerts: Vec<crate::alerts::Rule>,
    /// Advanced sensors through the optional LibreHardwareMonitor helper process (off by default).
    pub lhm_sensors: bool,
    /// start the helper with administrator rights (a UAC prompt at every start); needed with
    /// the PawnIO driver for CPU temperatures, fans and voltages
    pub lhm_elevated: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            position: "right".into(),
            offset: 0,
            widgets: vec![
                "cpu_bars".into(),
                "cpu_line".into(),
                "mem".into(),
                "gpu".into(),
                "net".into(),
                "disk".into(),
                "battery".into(),
            ],
            update_ms: 1000,
            labels: true,
            utilization_colors: false,
            disk: "C:".into(),
            accent: "blue".into(),
            colors: BTreeMap::new(),
            theme: "auto".into(),
            popup_opacity: "system".into(),
            font_scale: 1.0,
            cpu_utility: true,
            group_pe_cores: false,
            split_pe_cores: true,
            chart_width: 34,
            bar_width: "small".into(),
            mode: "auto".into(),
            top_processes: 8,
            stretched: Vec::new(),
            language: "auto".into(),
            net_disabled: Vec::new(),
            disks_disabled: Vec::new(),
            net_chart_rx: true,
            net_chart_tx: true,
            ping_host: String::new(),
            alerts: crate::alerts::default_rules(),
            lhm_sensors: false,
            lhm_elevated: false,
        }
    }
}

/// Update intervals offered in the settings, ms.
pub const INTERVALS: &[u32] = &[500, 1000, 2000, 5000];

impl Config {
    pub fn path() -> PathBuf {
        crate::util::config_dir().join("config.toml")
    }

    /// Load the config; write a default file if none exists. A file that does not parse is kept
    /// as config.toml.bad (so a typo does not silently wipe the user's settings) and the
    /// defaults are used.
    pub fn load() -> Config {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => match Self::parse(&s) {
                Ok(c) => c,
                Err(e) => {
                    crate::log!("config parse error: {e}; keeping the file as config.toml.bad, using defaults");
                    let _ = std::fs::rename(&path, path.with_extension("toml.bad"));
                    let c = Config::default();
                    c.save();
                    c
                }
            },
            Err(_) => {
                let c = Config::default();
                c.save();
                c
            }
        }
    }

    /// The stored config, or the defaults; never writes anything (command-line modes).
    pub fn load_readonly() -> Config {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| Self::parse(&s).ok()).unwrap_or_default()
    }

    /// Parse a config document and bring the values into range.
    pub fn parse(s: &str) -> Result<Config, toml::de::Error> {
        let mut c = toml::from_str::<Config>(s)?;
        c.sanitize();
        Ok(c)
    }

    /// Clamp values a hand-edited file may have put out of range.
    pub fn sanitize(&mut self) {
        if self.config_version < CONFIG_VERSION {
            // version 0 = winstats-era files: the ping went to 1.1.1.1 by default; Windows Stats
            // only pings when the user sets a host
            if self.ping_host == "1.1.1.1" {
                self.ping_host.clear();
            }
            self.config_version = CONFIG_VERSION;
        }
        if !INTERVALS.contains(&self.update_ms) {
            self.update_ms = INTERVALS.iter().copied().min_by_key(|v| v.abs_diff(self.update_ms)).unwrap_or(1000);
        }
        if !(0.5..=2.0).contains(&self.font_scale) {
            self.font_scale = 1.0;
        }
        self.chart_width = self.chart_width.clamp(20, 200);
        self.top_processes = self.top_processes.clamp(1, 30);
        self.offset = self.offset.clamp(-2000, 2000);
        if !matches!(self.theme.as_str(), "auto" | "dark" | "light") {
            self.theme = "auto".into();
        }
        if !matches!(self.mode.as_str(), "auto" | "embedded" | "floating") {
            self.mode = "auto".into();
        }
        if !matches!(self.position.as_str(), "right" | "left") {
            self.position = "right".into();
        }
        if !matches!(self.bar_width.as_str(), "small" | "medium" | "large") {
            self.bar_width = "small".into();
        }
        if !matches!(self.popup_opacity.as_str(), "more" | "system" | "less" | "solid") {
            self.popup_opacity = "system".into();
        }
        if !self.net_chart_rx && !self.net_chart_tx {
            self.net_chart_rx = true;
        }
    }

    /// The config as stored on disk, with a short header.
    pub fn to_document(&self) -> Result<String, toml::ser::Error> {
        let body = toml::to_string_pretty(self)?;
        let header = "# Windows Stats yapılandırması / configuration\n# widgets: cpu_bars, cpu_line, cpu_mini, gpu, gpu_line, mem, mem_line, mem_text,\n#          disk, disk_chart, net, net_chart, battery\n\n";
        Ok(format!("{header}{body}"))
    }

    /// Write the config back (comments in the file are not preserved). Written to a temporary
    /// file first and moved over the old one, so a crash or power loss never leaves half a file.
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match self.to_document() {
            Ok(doc) => {
                let tmp = path.with_extension("toml.tmp");
                let result = std::fs::write(&tmp, doc).and_then(|_| std::fs::rename(&tmp, &path));
                if let Err(e) = result {
                    crate::log!("config save failed: {e}");
                    let _ = std::fs::remove_file(&tmp);
                }
            }
            Err(e) => crate::log!("config serialize failed: {e}"),
        }
    }

    pub fn accent_color(&self) -> crate::canvas::Color {
        crate::widgets::named_color(&self.accent).unwrap_or(crate::widgets::BLUE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let c = Config::default();
        let doc = c.to_document().unwrap();
        let back = Config::parse(&doc).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn defaults_are_private() {
        // nothing in the defaults talks to the network
        let c = Config::default();
        assert!(c.ping_host.is_empty());
        assert_eq!(c.update_ms, 1000);
    }

    #[test]
    fn old_ping_default_is_dropped() {
        let c = Config::parse("ping_host = \"1.1.1.1\"\n").unwrap();
        assert!(c.ping_host.is_empty());
        // a host chosen by the user in a current file stays
        let c = Config::parse("config_version = 1\nping_host = \"1.1.1.1\"\n").unwrap();
        assert_eq!(c.ping_host, "1.1.1.1");
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let c = Config::parse("update_ms = 1300\nfont_scale = 9.0\ntheme = \"purple\"\nchart_width = 5000\n").unwrap();
        assert_eq!(c.update_ms, 1000);
        assert_eq!(c.font_scale, 1.0);
        assert_eq!(c.theme, "auto");
        assert_eq!(c.chart_width, 200);
        let c = Config::parse("update_ms = 4000\n").unwrap();
        assert_eq!(c.update_ms, 5000);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // winstats-era remote machine settings in an old file do not break loading
        let c = Config::parse("discovery = true\nshare = false\n[remotes.pi]\nid = \"x\"\n").unwrap();
        assert_eq!(c.widgets, Config::default().widgets);
    }
}
