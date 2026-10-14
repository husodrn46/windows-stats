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

/// How the helper runs: our child with its stdout piped to us, or elevated (started through
/// UAC, so it cannot inherit handles) writing into a named pipe we created.
enum Source {
    Child(Child),
    /// server end of the pipe, raw; disconnecting it makes the helper's next write fail and
    /// the helper exit (an elevated process cannot be terminated from here)
    Pipe(isize),
}

pub struct SensorHost {
    source: Source,
    shared: Arc<Mutex<Shared>>,
}

/// Where the helper is: next to the executable, or in the build output during development.
pub fn helper_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidates = [
        dir.join("WindowsStats.Sensors.exe"),
        dir.join("sensors").join("WindowsStats.Sensors.exe"),
        dir.join("..").join("sensors").join("bin").join("Release").join("net48").join("WindowsStats.Sensors.exe"),
    ];
    candidates.into_iter().find(|p| p.exists())
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
    /// Start the helper; `elevated` asks Windows for administrator rights (UAC prompt) and
    /// falls back to a normal start when the user declines.
    pub fn start(interval_ms: u32, elevated: bool) -> Option<SensorHost> {
        if elevated {
            match Self::start_elevated(interval_ms) {
                Some(h) => return Some(h),
                None => crate::log!("sensor helper: elevation declined or failed, starting it without"),
            }
        }
        Self::start_child(interval_ms)
    }

    fn start_elevated(interval_ms: u32) -> Option<SensorHost> {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
        use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};
        use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
        let path = helper_path()?;
        // an unguessable name, one instance, local clients only
        let nonce = {
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
            h.finish()
        };
        let name = format!("WindowsStats-sensors-{}-{nonce:016x}", std::process::id());
        let full = crate::util::wide(&format!(r"\\.\pipe\{name}"));
        let pipe = unsafe {
            CreateNamedPipeW(
                crate::util::pcwstr(&full),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                64 * 1024,
                0,
                None,
            )
        };
        if pipe.is_invalid() {
            crate::log!("sensor helper: CreateNamedPipe failed");
            return None;
        }
        // WINDOWS_STATS_PIPE_TEST: the same pipe path without elevation (for tests, no UAC prompt)
        let verb = crate::util::wide(if std::env::var_os("WINDOWS_STATS_PIPE_TEST").is_some() { "open" } else { "runas" });
        let file = crate::util::wide(&path.display().to_string());
        let params = crate::util::wide(&format!("--pipe {name} --parent {} --interval {}", std::process::id(), interval_ms.clamp(1000, 10_000)));
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: crate::util::pcwstr(&verb),
            lpFile: crate::util::pcwstr(&file),
            lpParameters: crate::util::pcwstr(&params),
            nShow: 0, // SW_HIDE
            ..Default::default()
        };
        // ShellExecuteEx (the UAC prompt) wants COM on the calling thread
        let com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED | windows::Win32::System::Com::COINIT_DISABLE_OLE1DDE).is_ok() };
        let launched = unsafe { ShellExecuteExW(&mut sei) };
        if com {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
        if let Err(e) = launched {
            crate::log!("sensor helper: elevated start failed: {e}");
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(pipe);
            }
            return None;
        }
        unsafe {
            if !sei.hProcess.is_invalid() {
                let _ = windows::Win32::Foundation::CloseHandle(sei.hProcess);
            }
        }
        let raw = pipe.0 as isize;
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let s = shared.clone();
        // wait for the helper to connect on the reader thread, then read like stdout
        let _ = std::thread::Builder::new().name("sensor-pipe".into()).spawn(move || unsafe {
            let h = windows::Win32::Foundation::HANDLE(raw as *mut _);
            let r = ConnectNamedPipe(h, None);
            crate::log!("sensor pipe: client connected ({r:?})");
            let file = std::fs::File::from_raw_handle(raw as *mut _);
            spawn_reader(file, s);
        });
        crate::log!("sensor helper started elevated: {}", path.display());
        Some(SensorHost { source: Source::Pipe(raw), shared })
    }

    fn start_child(interval_ms: u32) -> Option<SensorHost> {
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
        Some(SensorHost { source: Source::Child(child), shared })
    }

    pub fn readings(&self) -> (Vec<Reading>, HelperInfo) {
        self.shared.lock().map(|g| (g.readings.clone(), g.info)).unwrap_or_default()
    }
}

impl Drop for SensorHost {
    fn drop(&mut self) {
        match &mut self.source {
            Source::Child(c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            Source::Pipe(raw) => unsafe {
                // the helper's next write fails and it exits
                let _ = windows::Win32::System::Pipes::DisconnectNamedPipe(windows::Win32::Foundation::HANDLE(*raw as *mut _));
            },
        }
        crate::log!("sensor helper stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
