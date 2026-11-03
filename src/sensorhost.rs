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
}
