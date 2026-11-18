//! Which programs are using the microphone or the camera right now, and since when, from the
//! per-user privacy records Windows keeps for its own "recent activity" list (ConsentStore,
//! Windows 10 1903 and later). Read-only and local; read only while an alert rule asks for it.
//!
//! A record says "in use" when it has a start time and no stop time. A program that crashed while
//! recording leaves such a record behind, so a record only counts while its program still runs.

use crate::metrics::processes::Entry;
use crate::util::{pcwstr, wide};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_QWORD};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

/// One program using the device: a short name and how long it has been using it.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureUse {
    pub app: String,
    pub secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Microphone,
    Camera,
}

impl Device {
    fn key(self) -> &'static str {
        match self {
            Device::Microphone => "microphone",
            Device::Camera => "webcam",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Owner {
    /// a desktop program: its full path
    Desktop(String),
    /// a Store app: its package family name ("MSTeams_8wekyb3d8bbwe")
    Packaged(String),
}

#[derive(Clone, Debug)]
struct Record {
    owner: Owner,
    /// FILETIME, 100 ns since 1601; 0 = never
    start: u64,
    stop: u64,
}

const ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

fn root() -> String {
    // development only: read a copy of the records (a test key) instead of the real ones
    #[cfg(debug_assertions)]
    if let Ok(r) = std::env::var("WINDOWS_STATS_CONSENT_ROOT") {
        if !r.is_empty() {
            return r;
        }
    }
    ROOT.to_string()
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open(path: &str) -> Option<Key> {
    let p = wide(path);
    let mut h = HKEY::default();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, pcwstr(&p), 0, KEY_READ, &mut h) };
    (r == ERROR_SUCCESS).then_some(Key(h))
}

fn subkeys(key: &Key) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = [0u16; 512];
    for index in 0..4096u32 {
        let mut len = buf.len() as u32;
        let r = unsafe { RegEnumKeyExW(key.0, index, windows::core::PWSTR(buf.as_mut_ptr()), &mut len, None, windows::core::PWSTR::null(), None, None) };
        if r != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

fn qword(key: &Key, sub: &str, value: &str) -> u64 {
    let (s, v) = (wide(sub), wide(value));
    let mut data = 0u64;
    let mut size = 8u32;
    let r = unsafe { RegGetValueW(key.0, pcwstr(&s), pcwstr(&v), RRF_RT_REG_QWORD, None, Some(&mut data as *mut u64 as *mut _), Some(&mut size)) };
    if r == ERROR_SUCCESS { data } else { 0 }
}

/// Windows keeps these records (10 version 1903 and later).
pub fn supported() -> bool {
    open(&format!("{}\\{}", root(), Device::Microphone.key())).is_some()
}

fn records(device: Device) -> Vec<Record> {
    let base = format!("{}\\{}", root(), device.key());
    let Some(key) = open(&base) else { return Vec::new() };
    let mut out = Vec::new();
    for name in subkeys(&key) {
        if name.eq_ignore_ascii_case("NonPackaged") {
            let Some(np) = open(&format!("{base}\\NonPackaged")) else { continue };
            for path in subkeys(&np) {
                let start = qword(&np, &path, "LastUsedTimeStart");
                let stop = qword(&np, &path, "LastUsedTimeStop");
                out.push(Record { owner: Owner::Desktop(path.replace('#', "\\")), start, stop });
            }
        } else {
            let start = qword(&key, &name, "LastUsedTimeStart");
            let stop = qword(&key, &name, "LastUsedTimeStop");
            out.push(Record { owner: Owner::Packaged(name), start, stop });
        }
    }
    out
}

fn now_filetime() -> u64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() / 100).unwrap_or(0) as u64;
    unix + 116_444_736_000_000_000
}

/// The short name shown on the card: "Zoom" for C:\...\Zoom.exe, "MSTeams" for a Store app.
fn display_name(owner: &Owner) -> String {
    match owner {
        Owner::Desktop(path) => {
            let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
            file.rsplit_once('.').map_or(file, |(stem, _)| stem).to_string()
        }
        Owner::Packaged(family) => {
            let name = family.rsplit_once('_').map_or(family.as_str(), |(n, _)| n);
            name.rsplit('.').find(|s| !s.is_empty()).unwrap_or(name).to_string()
        }
    }
}

/// Programs using the device now, longest first; each program once.
fn in_use(records: &[Record], now: u64, running: impl Fn(&Owner) -> bool) -> Vec<CaptureUse> {
    let mut out: Vec<CaptureUse> = records
        .iter()
        .filter(|r| r.start != 0 && r.stop == 0 && r.start <= now && running(&r.owner))
        .map(|r| CaptureUse { app: display_name(&r.owner), secs: (now - r.start) / 10_000_000 })
        .collect();
    out.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.app.cmp(&b.app)));
    let mut seen = std::collections::HashSet::new();
    out.retain(|u| seen.insert(u.app.to_lowercase()));
    out
}

/// Store app check: one folder of the program's path is its package folder
/// ("MSTeams_25290.205.4069.4894_x64__8wekyb3d8bbwe" or "Name_publisher").
fn path_in_package(path: &str, family: &str) -> bool {
    let Some((name, publisher)) = family.rsplit_once('_') else { return false };
    let (name, publisher) = (name.to_lowercase(), publisher.to_lowercase());
    path.to_lowercase().split(['\\', '/']).any(|dir| dir.starts_with(&format!("{name}_")) && dir.ends_with(&format!("_{publisher}")))
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Programs using `device` now. `procs` is a fresh process snapshot (names and ids).
pub fn read(device: Device, procs: &[Entry]) -> Vec<CaptureUse> {
    let records = records(device);
    if !records.iter().any(|r| r.start != 0 && r.stop == 0) {
        return Vec::new();
    }
    // Store apps need the executable paths; collected only when such a record claims use
    let needs_paths = records.iter().any(|r| r.stop == 0 && r.start != 0 && matches!(r.owner, Owner::Packaged(_)));
    let paths: Vec<String> = if needs_paths { procs.iter().filter_map(|p| image_path(p.pid)).collect() } else { Vec::new() };
    in_use(&records, now_filetime(), |owner| match owner {
        Owner::Desktop(path) => {
            let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
            procs.iter().any(|p| p.name.eq_ignore_ascii_case(file))
        }
        Owner::Packaged(family) => paths.iter().any(|p| path_in_package(p, family)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60 * 10_000_000;

    fn rec(owner: Owner, start: u64, stop: u64) -> Record {
        Record { owner, start, stop }
    }

    #[test]
    fn names_are_short() {
        assert_eq!(display_name(&Owner::Desktop(r"C:\Users\x\AppData\Roaming\Zoom\bin\Zoom.exe".into())), "Zoom");
        assert_eq!(display_name(&Owner::Packaged("MSTeams_8wekyb3d8bbwe".into())), "MSTeams");
        assert_eq!(display_name(&Owner::Packaged("Microsoft.WindowsCamera_8wekyb3d8bbwe".into())), "WindowsCamera");
    }

    #[test]
    fn only_open_records_of_running_programs_count() {
        let now = 1_000 * MIN;
        let records = vec![
            rec(Owner::Desktop(r"C:\Zoom\Zoom.exe".into()), now - 34 * MIN, 0),
            rec(Owner::Desktop(r"C:\Old\Recorder.exe".into()), now - 90 * MIN, 0), // crashed: not running
            rec(Owner::Desktop(r"C:\Done\Meet.exe".into()), now - 50 * MIN, now - 10 * MIN), // stopped
            rec(Owner::Packaged("MSTeams_8wekyb3d8bbwe".into()), now - 5 * MIN, 0),
            rec(Owner::Desktop(r"C:\Never\App.exe".into()), 0, 0),
        ];
        let running = |o: &Owner| !matches!(o, Owner::Desktop(p) if p.contains("Recorder"));
        let used = in_use(&records, now, running);
        assert_eq!(used, vec![CaptureUse { app: "Zoom".into(), secs: 34 * 60 }, CaptureUse { app: "MSTeams".into(), secs: 5 * 60 }]);
    }

    #[test]
    fn store_app_paths_match_their_package() {
        let family = "MSTeams_8wekyb3d8bbwe";
        assert!(path_in_package(r"C:\Program Files\WindowsApps\MSTeams_25290.205.4069.4894_x64__8wekyb3d8bbwe\ms-teams.exe", family));
        assert!(path_in_package(r"C:\Windows\SystemApps\MSTeams_8wekyb3d8bbwe\x.exe", family));
        assert!(!path_in_package(r"C:\Program Files\WindowsApps\MSTeamsBeta_1.0_x64__8wekyb3d8bbwe\x.exe", family));
        assert!(!path_in_package(r"C:\Program Files\WindowsApps\MSTeams_1.0_x64__otherpublisher\x.exe", family));
    }
}
