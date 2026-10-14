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
}

impl Metric {
    pub const ALL: &'static [Metric] = &[Metric::CpuLoad, Metric::CpuTemp, Metric::GpuLoad, Metric::GpuTemp, Metric::MemUsage, Metric::DiskTemp, Metric::BatteryLevel];

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
        }
    }

    pub fn is_temperature(self) -> bool {
        matches!(self, Metric::CpuTemp | Metric::GpuTemp | Metric::DiskTemp)
    }

    /// "85 °C", "90%"
    pub fn format(self, v: f32) -> String {
        if self.is_temperature() {
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
    ]
}

/// Margin the value has to come back by before a fired rule can fire again.
fn hysteresis(rule: &Rule) -> f32 {
    if rule.metric.is_temperature() { 3.0 } else { 5.0 }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
