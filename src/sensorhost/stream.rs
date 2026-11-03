//! Sensor helper protocol and snapshot lifetime, independent of Windows process launch.

use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};

/// One sensor reading from the helper.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// LibreHardwareMonitor hardware type: "Cpu", "GpuNvidia", "Motherboard", "Storage"...
    pub hw_type: String,
    pub hw: String,
    /// "Temperature", "Fan", "Voltage", "Power", "Clock", "Load"...
    pub kind: String,
    pub name: String,
    pub value: f32,
}

/// What the helper reported about itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HelperInfo {
    pub running: bool,
    pub elevated: bool,
    pub pawnio: bool,
}

#[derive(Default)]
pub(super) struct Shared {
    pub(super) readings: Vec<Reading>,
    pub(super) info: HelperInfo,
}

/// Parse one line of the helper's output into the batch being built.
pub fn parse_line(line: &str, batch: &mut Vec<Reading>, info: &mut HelperInfo) -> bool {
    let f: Vec<&str> = line.split('\t').collect();
    match f.first().copied() {
        Some("H") if f.len() >= 4 => {
            info.running = true;
            info.elevated = f[2] == "1";
            info.pawnio = f[3] == "1";
            false
        }
        Some("B") => {
            batch.clear();
            false
        }
        Some("S") if f.len() >= 6 => {
            if let Ok(v) = f[5].parse::<f32>() {
                if v.is_finite() {
                    batch.push(Reading {
                        hw_type: f[1].into(),
                        hw: f[2].into(),
                        kind: f[3].into(),
                        name: f[4].into(),
                        value: v,
                    });
                }
            }
            false
        }
        // a complete batch
        Some("E") => true,
        _ => false,
    }
}

pub(super) fn read_stream(
    reader: impl std::io::Read,
    shared: Arc<Mutex<Shared>>,
    mut log: impl FnMut(String),
) {
    let mut batch = Vec::new();
    let mut info = HelperInfo::default();
    let mut logged_batch = false;
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        if !info.running {
            log(format!(
                "sensor helper says: {}",
                line.chars().take(40).collect::<String>()
            ));
        }
        let done = parse_line(line.trim_end(), &mut batch, &mut info);
        if done && !logged_batch {
            logged_batch = true;
            log(format!(
                "sensor helper: first batch, {} readings",
                batch.len()
            ));
        }
        if let Ok(mut g) = shared.lock() {
            g.info = info;
            if done {
                g.readings = std::mem::take(&mut batch);
            }
        }
    }
    if let Ok(mut g) = shared.lock() {
        // EOF or a read error invalidates the entire snapshot, including helper flags.
        *g = Shared::default();
    }
}
