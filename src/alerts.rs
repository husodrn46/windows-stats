//! Threshold notifications: "warn when the CPU temperature stays above 85 °C for 30 seconds".
//!
//! A rule fires once when its metric has been on the wrong side of the threshold for the whole
//! duration; it re-arms only after the value came back past the threshold by a small margin
//! (hysteresis), so a value hovering at the limit does not produce a stream of notifications.
//! A metric that is not measured on this machine (no sensor, no battery, no GPU counters) never
//! fires: a missing reading resets the rule instead of counting as "above" or "below".

use crate::metrics::MetricsData;
use serde::{Deserialize, Serialize};

/// What a rule watches. Values are in the unit shown to the user: percent, °C.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    CpuLoad,
    CpuTemp,
    GpuLoad,
    GpuTemp,
    MemUsage,
    DiskTemp,
    BatteryLevel,
    /// minutes the microphone has been in use by one program
    MicInUse,
    /// minutes the camera has been in use by one program
    CameraInUse,
    /// GB one program's memory grew by, steadily, over the last hours (a leak)
    MemGrowth,
}

impl Metric {
    pub const ALL: &'static [Metric] = &[Metric::CpuLoad, Metric::CpuTemp, Metric::GpuLoad, Metric::GpuTemp, Metric::MemUsage, Metric::DiskTemp, Metric::BatteryLevel, Metric::MicInUse, Metric::CameraInUse, Metric::MemGrowth];

    /// Current value, or None when this machine does not measure it.
    pub fn value(self, m: &MetricsData) -> Option<f32> {
        match self {
            Metric::CpuLoad => (!m.cpu_cores.is_empty()).then_some(m.cpu_total * 100.0),
            Metric::CpuTemp => m.cpu_temp,
            Metric::GpuLoad => m.gpu.map(|g| g * 100.0),
            Metric::GpuTemp => m.gpu_temp,
            Metric::MemUsage => (m.mem.total > 0).then_some(m.mem.pct * 100.0),
            Metric::DiskTemp => m.disks.iter().filter_map(|d| d.temp_c).fold(None, |a: Option<f32>, t| Some(a.map_or(t, |a| a.max(t)))),
            Metric::BatteryLevel => m.battery.map(|b| b.level * 100.0),
            Metric::MicInUse => Some(m.mic_use.first().map_or(0.0, |u| u.secs as f32 / 60.0)),
            Metric::CameraInUse => m.camera_supported.then(|| m.camera_use.first().map_or(0.0, |u| u.secs as f32 / 60.0)),
            Metric::MemGrowth => Some(m.mem_growth.as_ref().map_or(0.0, |g| g.grown as f32 / GIB)),
        }
    }

    /// i18n key of the metric's name.
    pub fn key(self) -> &'static str {
        match self {
            Metric::CpuLoad => "alerts.cpu_load",
            Metric::CpuTemp => "alerts.cpu_temp",
            Metric::GpuLoad => "alerts.gpu_load",
            Metric::GpuTemp => "alerts.gpu_temp",
            Metric::MemUsage => "alerts.mem_usage",
            Metric::DiskTemp => "alerts.disk_temp",
            Metric::BatteryLevel => "alerts.battery_level",
            Metric::MicInUse => "alerts.mic_in_use",
            Metric::CameraInUse => "alerts.camera_in_use",
            Metric::MemGrowth => "alerts.mem_growth",
        }
    }

    /// The value already measures a span of time (minutes in use, growth over hours): the rule
    /// fires as soon as it passes the limit, with no extra duration to set.
    pub fn own_duration(self) -> bool {
        self.is_capture() || self == Metric::MemGrowth
    }

    /// Microphone / camera: the value is how long one program has been using the device, in
    /// minutes; the rule fires as soon as that passes the limit (no extra duration).
    pub fn is_capture(self) -> bool {
        matches!(self, Metric::MicInUse | Metric::CameraInUse)
    }

    /// The Windows privacy page a click on the card opens.
    pub fn privacy_page(self) -> Option<&'static str> {
        match self {
            Metric::MicInUse => Some("ms-settings:privacy-microphone"),
            Metric::CameraInUse => Some("ms-settings:privacy-webcam"),
            _ => None,
        }
    }

    /// Load metrics a single program can cause: the card names the one using the most.
    pub fn names_culprit(self) -> bool {
        matches!(self, Metric::CpuLoad | Metric::MemUsage)
    }

    pub fn is_temperature(self) -> bool {
        matches!(self, Metric::CpuTemp | Metric::GpuTemp | Metric::DiskTemp)
    }

    /// "85 °C", "90%", "30 min"
    pub fn format(self, v: f32) -> String {
        if self.is_capture() {
            crate::i18n::tf("alerts.minutes", &[("n", &format!("{v:.0}"))])
        } else if self == Metric::MemGrowth {
            format!("{} GB", crate::util::format_decimal(v as f64, 1))
        } else if self.is_temperature() {
            format!("{v:.0} \u{00B0}C")
        } else {
            format!("{v:.0}%")
        }
    }
}

/// One rule as stored in the config (`[[alerts]]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub enabled: bool,
    pub metric: Metric,
    /// true: fire when the value is above `threshold`; false: when below (battery level)
    pub above: bool,
    pub threshold: f32,
    /// how long the condition has to hold, seconds
    pub seconds: u32,
}

impl Default for Rule {
    fn default() -> Self {
        Self { enabled: false, metric: Metric::CpuTemp, above: true, threshold: 85.0, seconds: 30 }
    }
}

/// The presets offered in the settings; all off until the user switches them on.
pub fn default_rules() -> Vec<Rule> {
    vec![
        Rule { enabled: false, metric: Metric::CpuTemp, above: true, threshold: 85.0, seconds: 30 },
        Rule { enabled: false, metric: Metric::GpuTemp, above: true, threshold: 85.0, seconds: 30 },
        Rule { enabled: false, metric: Metric::CpuLoad, above: true, threshold: 90.0, seconds: 60 },
        Rule { enabled: false, metric: Metric::MemUsage, above: true, threshold: 90.0, seconds: 60 },
        Rule { enabled: false, metric: Metric::DiskTemp, above: true, threshold: 60.0, seconds: 60 },
        Rule { enabled: false, metric: Metric::BatteryLevel, above: false, threshold: 15.0, seconds: 10 },
        Rule { enabled: false, metric: Metric::MicInUse, above: true, threshold: 30.0, seconds: 0 },
        Rule { enabled: false, metric: Metric::CameraInUse, above: true, threshold: 30.0, seconds: 0 },
        Rule { enabled: false, metric: Metric::MemGrowth, above: true, threshold: 1.0, seconds: 0 },
    ]
}

const GIB: f32 = 1024.0 * 1024.0 * 1024.0;

/// Margin the value has to come back by before a fired rule can fire again.
fn hysteresis(rule: &Rule) -> f32 {
    if rule.metric == Metric::MemGrowth {
        0.25
    } else if rule.metric.is_temperature() {
        3.0
    } else {
        5.0
    }
}

#[derive(Clone, Debug, Default)]
struct State {
    /// seconds the condition has held so far
    held: f32,
    fired: bool,
}

/// A rule that just fired, with the value that triggered it.
#[derive(Clone, Debug, PartialEq)]
pub struct Fired {
    pub rule: Rule,
    pub value: f32,
}

#[derive(Default)]
pub struct Alerts {
    states: Vec<State>,
}

impl Alerts {
    /// Advance every rule by `dt` seconds with the new sample; returns the rules that fire now.
    pub fn evaluate(&mut self, rules: &[Rule], m: &MetricsData, dt: f32) -> Vec<Fired> {
        self.states.resize(rules.len(), State::default());
        let mut out = Vec::new();
        for (rule, st) in rules.iter().zip(self.states.iter_mut()) {
            if !rule.enabled {
                *st = State::default();
                continue;
            }
            let Some(v) = rule.metric.value(m) else {
                // not measured: never an alert, and nothing carried over
                *st = State::default();
                continue;
            };
            let bad = if rule.above { v > rule.threshold } else { v < rule.threshold };
            if bad {
                st.held += dt.max(0.0);
                if !st.fired && st.held >= rule.seconds as f32 {
                    st.fired = true;
                    out.push(Fired { rule: rule.clone(), value: v });
                }
            } else {
                st.held = 0.0;
                let back = if rule.above { v <= rule.threshold - hysteresis(rule) } else { v >= rule.threshold + hysteresis(rule) };
                if back {
                    st.fired = false;
                }
            }
        }
        out
    }

    /// Forget all timers (rules edited, system resumed from sleep).
    pub fn reset(&mut self) {
        self.states.clear();
    }

    /// A CPU or memory rule is counting towards firing: the per-process list should be sampled
    /// now, so the card can name the program when it fires. Off otherwise (no cost while idle).
    pub fn wants_processes(&self, rules: &[Rule]) -> bool {
        rules.iter().zip(self.states.iter()).any(|(r, st)| r.enabled && r.metric.names_culprit() && st.held > 0.0 && !st.fired)
    }
}

/// The program using the most of `metric` right now: "Teams (71%)", "chrome.exe (2.1 GB)".
/// None when the per-process list was not sampled or nothing stands out.
pub fn culprit(metric: Metric, m: &MetricsData) -> Option<(String, String)> {
    match metric {
        Metric::CpuLoad => m.top_cpu.first().filter(|p| p.cpu >= 0.05).map(|p| (p.name.clone(), format!("{:.0}%", p.cpu * 100.0))),
        Metric::MemUsage => m.top_mem.first().filter(|p| p.mem >= 64 * 1024 * 1024).map(|p| (p.name.clone(), crate::util::format_bytes(p.mem))),
        Metric::MicInUse | Metric::CameraInUse => {
            let list = if metric == Metric::MicInUse { &m.mic_use } else { &m.camera_use };
            let first = list.first()?;
            let names: Vec<&str> = list.iter().take(3).map(|u| u.app.as_str()).collect();
            Some((names.join(", "), format!("{}", first.secs / 60)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_info(name: &str, cpu: f32, mem: u64) -> crate::metrics::processes::ProcInfo {
        crate::metrics::processes::ProcInfo { pid: 1, name: name.into(), cpu, mem, io: 0.0 }
    }

    #[test]
    fn processes_are_sampled_only_while_a_load_rule_counts() {
        let rules = vec![
            Rule { enabled: true, metric: Metric::CpuLoad, above: true, threshold: 90.0, seconds: 3 },
            Rule { enabled: true, metric: Metric::CpuTemp, above: true, threshold: 50.0, seconds: 3 },
        ];
        let mut a = Alerts::default();
        let mut m = MetricsData { cpu_cores: vec![0.0], cpu_total: 0.2, cpu_temp: Some(80.0), ..Default::default() };
        a.evaluate(&rules, &m, 1.0);
        // a hot CPU alone does not need the process list
        assert!(!a.wants_processes(&rules));
        m.cpu_total = 0.95;
        a.evaluate(&rules, &m, 1.0);
        assert!(a.wants_processes(&rules));
        a.evaluate(&rules, &m, 1.0);
        let fired = a.evaluate(&rules, &m, 1.0);
        assert!(fired.iter().any(|f| f.rule.metric == Metric::CpuLoad));
        // fired: the card is shown, sampling stops until the load drops and rises again
        assert!(!a.wants_processes(&rules));
        let mut off = rules.clone();
        off[0].enabled = false;
        assert!(!a.wants_processes(&off));
    }

    #[test]
    fn memory_growth_fires_once_per_leak() {
        use crate::metrics::growth::Grower;
        let rules = vec![Rule { enabled: true, metric: Metric::MemGrowth, above: true, threshold: 1.0, seconds: 0 }];
        let mut a = Alerts::default();
        let mut m = MetricsData::default();
        assert_eq!(Metric::MemGrowth.value(&m), Some(0.0));
        let g = |gb: f32| Grower { pid: 7, name: "leaky.exe".into(), grown: (gb * GIB) as u64, now: 3 * GIB as u64, secs: 7200 };
        m.mem_growth = Some(g(0.8));
        assert!(a.evaluate(&rules, &m, 1.0).is_empty());
        m.mem_growth = Some(g(1.2));
        assert_eq!(a.evaluate(&rules, &m, 1.0).len(), 1);
        // still growing: one card, not one per sample
        m.mem_growth = Some(g(1.6));
        assert!(a.evaluate(&rules, &m, 1.0).is_empty());
        // gave the memory back, leaked again later: a new card
        m.mem_growth = None;
        a.evaluate(&rules, &m, 1.0);
        m.mem_growth = Some(g(1.1));
        assert_eq!(a.evaluate(&rules, &m, 1.0).len(), 1);
    }

    #[test]
    fn microphone_rule_fires_once_per_long_use() {
        use crate::metrics::capture::CaptureUse;
        let rules = vec![Rule { enabled: true, metric: Metric::MicInUse, above: true, threshold: 30.0, seconds: 0 }];
        let mut a = Alerts::default();
        let mut m = MetricsData::default();
        assert_eq!(Metric::MicInUse.value(&m), Some(0.0));
        assert!(a.evaluate(&rules, &m, 1.0).is_empty());
        m.mic_use = vec![CaptureUse { app: "Zoom".into(), secs: 29 * 60 }];
        assert!(a.evaluate(&rules, &m, 1.0).is_empty());
        m.mic_use = vec![CaptureUse { app: "Zoom".into(), secs: 31 * 60 }, CaptureUse { app: "Discord".into(), secs: 60 }];
        assert_eq!(a.evaluate(&rules, &m, 1.0).len(), 1);
        assert_eq!(culprit(Metric::MicInUse, &m), Some(("Zoom, Discord".into(), "31".into())));
        // still in use: no second card
        m.mic_use[0].secs = 50 * 60;
        assert!(a.evaluate(&rules, &m, 1.0).is_empty());
        // released and used again for long: a new card
        m.mic_use.clear();
        a.evaluate(&rules, &m, 1.0);
        m.mic_use = vec![CaptureUse { app: "Zoom".into(), secs: 40 * 60 }];
        assert_eq!(a.evaluate(&rules, &m, 1.0).len(), 1);
        // no records on this Windows: never measured, never fires
        assert_eq!(Metric::CameraInUse.value(&MetricsData::default()), None);
    }

    #[test]
    fn culprit_names_the_top_program_when_it_stands_out() {
        let mut m = MetricsData::default();
        assert_eq!(culprit(Metric::CpuLoad, &m), None);
        m.top_cpu = vec![proc_info("Teams.exe", 0.712, 0)];
        assert_eq!(culprit(Metric::CpuLoad, &m), Some(("Teams.exe".into(), "71%".into())));
        m.top_cpu = vec![proc_info("idle.exe", 0.01, 0)];
        assert_eq!(culprit(Metric::CpuLoad, &m), None);
        m.top_mem = vec![proc_info("chrome.exe", 0.0, 3 * 1024 * 1024 * 1024)];
        assert_eq!(culprit(Metric::MemUsage, &m).map(|c| c.0), Some("chrome.exe".into()));
        assert_eq!(culprit(Metric::CpuTemp, &m), None);
    }

    fn data(temp: Option<f32>) -> MetricsData {
        MetricsData { cpu_temp: temp, ..Default::default() }
    }

    fn rule() -> Rule {
        Rule { enabled: true, metric: Metric::CpuTemp, above: true, threshold: 85.0, seconds: 30 }
    }

    #[test]
    fn fires_once_after_the_duration() {
        let mut a = Alerts::default();
        let rules = [rule()];
        for _ in 0..29 {
            assert!(a.evaluate(&rules, &data(Some(90.0)), 1.0).is_empty());
        }
        let f = a.evaluate(&rules, &data(Some(91.0)), 1.0);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].value, 91.0);
        // still hot: no repeat
        for _ in 0..100 {
            assert!(a.evaluate(&rules, &data(Some(95.0)), 1.0).is_empty());
        }
    }

    #[test]
    fn a_dip_restarts_the_timer() {
        let mut a = Alerts::default();
        let rules = [rule()];
        for _ in 0..20 {
            a.evaluate(&rules, &data(Some(90.0)), 1.0);
        }
        a.evaluate(&rules, &data(Some(80.0)), 1.0);
        for _ in 0..29 {
            assert!(a.evaluate(&rules, &data(Some(90.0)), 1.0).is_empty());
        }
        assert_eq!(a.evaluate(&rules, &data(Some(90.0)), 1.0).len(), 1);
    }

    #[test]
    fn rearms_only_past_the_hysteresis() {
        let mut a = Alerts::default();
        let rules = [Rule { seconds: 0, ..rule() }];
        assert_eq!(a.evaluate(&rules, &data(Some(86.0)), 1.0).len(), 1);
        // 84 is below the threshold but within the margin: stays fired
        a.evaluate(&rules, &data(Some(84.0)), 1.0);
        assert!(a.evaluate(&rules, &data(Some(86.0)), 1.0).is_empty());
        // 81 is past it: the next excursion notifies again
        a.evaluate(&rules, &data(Some(81.0)), 1.0);
        assert_eq!(a.evaluate(&rules, &data(Some(86.0)), 1.0).len(), 1);
    }

    #[test]
    fn missing_sensor_never_fires() {
        let mut a = Alerts::default();
        let rules = [Rule { seconds: 0, ..rule() }];
        for _ in 0..1000 {
            assert!(a.evaluate(&rules, &data(None), 1.0).is_empty());
        }
        // and a reading that drops out resets the timer
        let rules = [rule()];
        for _ in 0..25 {
            a.evaluate(&rules, &data(Some(90.0)), 1.0);
        }
        a.evaluate(&rules, &data(None), 1.0);
        for _ in 0..29 {
            assert!(a.evaluate(&rules, &data(Some(90.0)), 1.0).is_empty());
        }
    }

    #[test]
    fn a_stopped_helper_cannot_continue_a_temperature_alert() {
        use crate::sensorhost::{HelperInfo, Reading};
        let readings = vec![Reading { hw_type: "Cpu".into(), hw: "CPU".into(), kind: "Temperature".into(), name: "CPU Package".into(), value: 95.0 }];
        let mut m = MetricsData::default();
        let mut a = Alerts::default();
        m.apply_helper(readings.clone(), HelperInfo { running: true, elevated: false, pawnio: true });
        assert_eq!(m.cpu_temp, Some(95.0));
        assert!(a.evaluate(&[rule()], &m, 29.0).is_empty());

        // Even if a caller retains the last batch, a stopped helper is not a live source.
        m.apply_helper(readings, HelperInfo::default());
        assert!(m.cpu_temp.is_none());
        assert!(m.ext_sensors.is_empty());
        assert!(a.evaluate(&[rule()], &m, 60.0).is_empty());
    }

    #[test]
    fn below_rules_and_disabled_rules() {
        let mut a = Alerts::default();
        let battery = |level: f32| MetricsData {
            battery: Some(crate::metrics::battery::Battery { level, charging: false, on_ac: false, remaining_secs: None }),
            ..Default::default()
        };
        let rules = [Rule { enabled: true, metric: Metric::BatteryLevel, above: false, threshold: 15.0, seconds: 0 }];
        assert!(a.evaluate(&rules, &battery(0.5), 1.0).is_empty());
        assert_eq!(a.evaluate(&rules, &battery(0.1), 1.0).len(), 1);
        let off = [Rule { enabled: false, ..rules[0].clone() }];
        assert!(Alerts::default().evaluate(&off, &battery(0.05), 1.0).is_empty());
        // a desktop without a battery: nothing
        assert!(Alerts::default().evaluate(&rules, &MetricsData::default(), 1.0).is_empty());
    }

    #[test]
    fn presets_are_off() {
        assert!(default_rules().iter().all(|r| !r.enabled));
    }
}
