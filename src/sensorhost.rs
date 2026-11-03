//! The optional sensor helper (`WindowsStats.Sensors.exe`, LibreHardwareMonitorLib): started as
//! a hidden child process when the user switches "advanced sensors" on, read line by line on
//! its own thread. It talks only through its stdout pipe: no port, no shared files. It exits by
//! itself when Windows Stats goes away (`--parent`), and is killed when switched off.

use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

mod stream;
pub use stream::{HelperInfo, Reading};
use stream::Shared;

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

/// Read the helper's lines on a thread into `shared`.
fn spawn_reader(reader: impl std::io::Read + Send + 'static, shared: Arc<Mutex<Shared>>) {
    let _ = std::thread::Builder::new().name("sensor-helper".into()).spawn(move || {
        stream::read_stream(reader, shared, |message| crate::log!("{message}"));
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

    /// The helper process ended by itself (crashed, killed, or gave up).
    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

/// How long to wait before starting the helper again after its `n`th stop in a row (1-based):
/// 10 s, 20 s, 40 s … at most 10 minutes, so a helper that keeps failing costs next to nothing.
pub fn restart_delay(n: u32) -> std::time::Duration {
    let shift = n.saturating_sub(1).min(6);
    std::time::Duration::from_secs((10u64 << shift).min(600))
}

/// A helper that ran this long before stopping counts as a fresh failure, not a repeated one.
const STABLE: std::time::Duration = std::time::Duration::from_secs(600);

/// Keeps the helper running while advanced sensors are on: starts it, notices when it stops by
/// itself and starts it again, waiting longer while it keeps failing. Lives on the collector
/// thread; `sync` is called on every configuration change and every sample.
#[derive(Default)]
pub struct Supervisor {
    host: Option<SensorHost>,
    started: Option<std::time::Instant>,
    retry_at: Option<std::time::Instant>,
    failures: u32,
    /// the helper stopped by itself and is waiting to be started again
    stopped: bool,
}

impl Supervisor {
    pub fn sync(&mut self, on: bool, interval_ms: u32) {
        if !on {
            *self = Supervisor::default();
            return;
        }
        let now = std::time::Instant::now();
        if let Some(h) = self.host.as_mut() {
            if !h.exited() {
                return;
            }
            let long_run = self.started.is_some_and(|t| now.duration_since(t) >= STABLE);
            self.failures = if long_run { 1 } else { self.failures + 1 };
            let wait = restart_delay(self.failures);
            crate::log!("sensor helper stopped by itself; starting it again in {} s", wait.as_secs());
            self.host = None;
            self.stopped = true;
            self.retry_at = Some(now + wait);
            return;
        }
        if self.retry_at.is_some_and(|t| now < t) {
            return;
        }
        self.host = SensorHost::start(interval_ms);
        self.started = Some(now);
        if self.host.is_some() {
            self.stopped = false;
            self.retry_at = None;
        } else {
            // missing or failing to start: try again later instead of on every sample
            self.failures += 1;
            self.retry_at = Some(now + restart_delay(self.failures));
        }
    }

    pub fn host(&self) -> Option<&SensorHost> {
        self.host.as_ref()
    }

    /// Switched on, but the helper stopped and is waiting to be started again.
    pub fn restarting(&self) -> bool {
        self.stopped && self.host.is_none()
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
    fn restarts_back_off_up_to_ten_minutes() {
        let secs: Vec<u64> = (0..=9).map(|n| restart_delay(n).as_secs()).collect();
        assert_eq!(secs, vec![10, 10, 20, 40, 80, 160, 320, 600, 600, 600]);
        assert_eq!(restart_delay(u32::MAX).as_secs(), 600);
    }

    #[test]
    fn switched_off_helper_is_not_restarting() {
        let mut s = Supervisor::default();
        s.sync(false, 2000);
        assert!(s.host().is_none());
        assert!(!s.restarting());
    }

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
}
