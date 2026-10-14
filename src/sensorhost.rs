//! The optional sensor helper (`WindowsStats.Sensors.exe`, LibreHardwareMonitorLib): started as
//! a hidden child process when the user switches "advanced sensors" on, read line by line on
//! its own thread. It talks only through its stdout pipe: no port, no shared files. It exits by
//! itself when Windows Stats goes away (`--parent`), and is killed when switched off.

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
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
struct Shared {
    readings: Vec<Reading>,
    info: HelperInfo,
}

/// The sensor helper is always a child process of the same user, with piped stdout.
/// The unsigned per-user package must never elevate executable bytes that the user can replace.
pub struct SensorHost {
    child: Child,
    shared: Arc<Mutex<Shared>>,
}

/// Production builds use only the packaged helper. Development builds can also use the
/// separate sensor build output. Never search parent or sibling paths in a release.
fn helper_candidates(exe: &std::path::Path, development: bool) -> Vec<std::path::PathBuf> {
    let Some(dir) = exe.parent() else { return Vec::new() };
    let mut candidates = vec![dir.join("sensors").join("WindowsStats.Sensors.exe")];
    if development {
        candidates.push(dir.join("..").join("sensors").join("bin").join("Release").join("net48").join("WindowsStats.Sensors.exe"));
    }
    candidates
}

pub fn helper_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    helper_candidates(&exe, cfg!(debug_assertions)).into_iter().find(|p| p.is_file())
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
                    batch.push(Reading { hw_type: f[1].into(), hw: f[2].into(), kind: f[3].into(), name: f[4].into(), value: v });
                }
            }
            false
        }
        // a complete batch
        Some("E") => true,
        _ => false,
    }
}

/// Read the helper's lines on a thread into `shared`.
fn spawn_reader(reader: impl std::io::Read + Send + 'static, shared: Arc<Mutex<Shared>>) {
    let _ = std::thread::Builder::new().name("sensor-helper".into()).spawn(move || {
        let mut batch = Vec::new();
        let mut info = HelperInfo::default();
        let mut logged_batch = false;
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if !info.running {
                crate::log!("sensor helper says: {}", line.chars().take(40).collect::<String>());
            }
            let done = parse_line(line.trim_end(), &mut batch, &mut info);
            if done && !logged_batch {
                logged_batch = true;
                crate::log!("sensor helper: first batch, {} readings", batch.len());
            }
            if let Ok(mut g) = shared.lock() {
                g.info = info;
                if done {
                    g.readings = std::mem::take(&mut batch);
                }
            }
        }
        if let Ok(mut g) = shared.lock() {
            g.info.running = false;
        }
    });
}

impl SensorHost {
    /// The unsigned sensor helper always runs with the caller's user privileges.
    /// Do not introduce UAC elevation without a protected, authenticated deployment.
    pub fn start(interval_ms: u32) -> Option<SensorHost> {
        let path = helper_path()?;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut child = Command::new(&path)
            .args(["--interval", &interval_ms.clamp(1000, 10_000).to_string(), "--parent", &std::process::id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| crate::log!("sensor helper failed to start: {e}"))
            .ok()?;
        let stdout = child.stdout.take()?;
        let shared: Arc<Mutex<Shared>> = Arc::default();
        spawn_reader(stdout, shared.clone());
        crate::log!("sensor helper started: {}", path.display());
        Some(SensorHost { child, shared })
    }

    pub fn readings(&self) -> (Vec<Reading>, HelperInfo) {
        self.shared.lock().map(|g| (g.readings.clone(), g.info)).unwrap_or_default()
    }
}

impl Drop for SensorHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        crate::log!("sensor helper stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_only_uses_packaged_sensor_helper() {
        let exe = std::path::Path::new(r"C:\Windows Stats\WindowsStats.exe");
        let expected = exe.parent().unwrap().join("sensors").join("WindowsStats.Sensors.exe");
        assert_eq!(helper_candidates(exe, false), vec![expected.clone()]);
        let debug = helper_candidates(exe, true);
        assert_eq!(debug[0], expected);
        assert_eq!(debug.len(), 2);
        assert!(debug[1].ends_with(std::path::Path::new(r"sensors\bin\Release\net48\WindowsStats.Sensors.exe")));
    }

    #[test]
    fn parses_a_batch() {
        let mut b = Vec::new();
        let mut i = HelperInfo::default();
        assert!(!parse_line("H\t0.1.0\t0\t1", &mut b, &mut i));
        assert!(i.running && !i.elevated && i.pawnio);
        assert!(!parse_line("B", &mut b, &mut i));
        assert!(!parse_line("S\tCpu\tIntel Core\tTemperature\tCPU Package\t47.5\t/intelcpu/0/temperature/0", &mut b, &mut i));
        assert!(!parse_line("S\tCpu\tIntel Core\tTemperature\tbroken\tnan-ish\t/x", &mut b, &mut i));
        assert!(parse_line("E", &mut b, &mut i));
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].value, 47.5);
        assert_eq!(b[0].name, "CPU Package");
    }
}
