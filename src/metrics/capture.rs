//! Which programs are using the microphone or the camera right now, and for how long. Read-only
//! and local (no sound or image is touched); read only while an alert rule asks for it.
//!
//! Microphone: the audio sessions Windows keeps per recording device (Core Audio, documented, no
//! rights needed) say which processes have a recording stream open right now. They don't say
//! since when, so [`MicWatch`] counts from the first look that saw a program.
//!
//! Camera (and the microphone when the audio system can't be asked): the per-user privacy records
//! Windows keeps for its own "recent activity" list (ConsentStore, Windows 10 1903 and later). A
//! record says "in use" when it has a start time and no stop time. A program that crashed while
//! recording leaves such a record behind, so a record only counts while its program still runs.
//! Newer Windows 11 builds (seen on 26H2) no longer fill these records in.

use crate::metrics::processes::Entry;
use crate::util::{pcwstr, wide};
use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::Media::Audio::{eCapture, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
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

/// Windows keeps the camera records (10 version 1903 and later).
pub fn supported() -> bool {
    open(&format!("{}\\{}", root(), Device::Camera.key())).is_some()
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

/// "Zoom" for "C:\...\Zoom.exe" or "Zoom.exe".
fn stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    file.rsplit_once('.').map_or(file, |(stem, _)| stem).to_string()
}

/// The short name shown on the card: "Zoom" for C:\...\Zoom.exe, "MSTeams" for a Store app.
fn display_name(owner: &Owner) -> String {
    match owner {
        Owner::Desktop(path) => stem(path),
        Owner::Packaged(family) => {
            let name = family.rsplit_once('_').map_or(family.as_str(), |(n, _)| n);
            name.rsplit('.').find(|s| !s.is_empty()).unwrap_or(name).to_string()
        }
    }
}

/// Longest use first; each program once.
fn longest_first(out: &mut Vec<CaptureUse>) {
    out.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.app.cmp(&b.app)));
    let mut seen = std::collections::HashSet::new();
    out.retain(|u| seen.insert(u.app.to_lowercase()));
}

/// Programs using the device now, longest first; each program once.
fn in_use(records: &[Record], now: u64, running: impl Fn(&Owner) -> bool) -> Vec<CaptureUse> {
    let mut out: Vec<CaptureUse> = records
        .iter()
        .filter(|r| r.start != 0 && r.stop == 0 && r.start <= now && running(&r.owner))
        .map(|r| CaptureUse { app: display_name(&r.owner), secs: (now - r.start) / 10_000_000 })
        .collect();
    longest_first(&mut out);
    out
}

/// Ids of the processes with a recording stream open on any active recording device right now.
/// `None` when the audio system can't be asked.
fn recording_pids() -> Option<Vec<u32>> {
    thread_local! {
        // the collector thread joins the multithreaded apartment once (harmless when it already has one)
        static COM: () = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        };
    }
    COM.with(|_| ());
    let own = std::process::id();
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let list = devices.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE).ok()?;
        let mut pids = Vec::new();
        for i in 0..list.GetCount().ok()? {
            let Ok(device) = list.Item(i) else { continue };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
            for j in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(session) = sessions.GetSession(j) else { continue };
                if session.GetState().ok() != Some(AudioSessionStateActive) {
                    continue;
                }
                let Ok(session) = session.cast::<IAudioSessionControl2>() else { continue };
                let pid = session.GetProcessId().unwrap_or(0);
                if pid != 0 && pid != own && !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
        }
        Some(pids)
    }
}

/// How long each program has kept a recording stream open, counted from the first look that saw
/// it. A pause of up to `GAP_SECS` (a short mute, a device switch) does not restart the count.
#[derive(Default)]
pub struct MicWatch {
    open: Vec<Open>,
}

struct Open {
    pid: u32,
    app: String,
    since: u64,
    seen: u64,
}

const GAP_SECS: u64 = 30;

impl MicWatch {
    /// `now`: seconds on a steady clock; `active`: (process id, short name) recording now.
    fn observe(&mut self, now: u64, active: &[(u32, String)]) -> Vec<CaptureUse> {
        for (pid, app) in active {
            match self.open.iter_mut().find(|o| o.pid == *pid && o.app == *app) {
                Some(o) => o.seen = now,
                None => self.open.push(Open { pid: *pid, app: app.clone(), since: now, seen: now }),
            }
        }
        self.open.retain(|o| now.saturating_sub(o.seen) <= GAP_SECS);
        let mut out: Vec<CaptureUse> = self.open.iter().map(|o| CaptureUse { app: o.app.clone(), secs: now.saturating_sub(o.since) }).collect();
        longest_first(&mut out);
        out
    }

    pub fn clear(&mut self) {
        self.open.clear();
    }

    /// Programs using the microphone now. `now`: seconds on a steady clock; `procs`: a fresh
    /// process snapshot (names and ids).
    pub fn read(&mut self, now: u64, procs: &[Entry]) -> Vec<CaptureUse> {
        let Some(pids) = recording_pids() else { return read(Device::Microphone, procs) };
        let active: Vec<(u32, String)> = pids.iter().filter_map(|&pid| procs.iter().find(|p| p.pid == pid).map(|p| (pid, stem(&p.name)))).collect();
        self.observe(now, &active)
    }
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
    fn microphone_use_counts_from_first_sight_and_survives_short_pauses() {
        let mut w = MicWatch::default();
        let zoom = vec![(10, "Zoom".to_string())];
        assert_eq!(w.observe(100, &zoom), vec![CaptureUse { app: "Zoom".into(), secs: 0 }]);
        assert_eq!(w.observe(400, &zoom)[0].secs, 300);
        // a short pause (a mute, a device switch) keeps counting
        assert_eq!(w.observe(420, &[])[0].secs, 320);
        assert_eq!(w.observe(425, &zoom)[0].secs, 325);
        // a second program; longest first
        let both = vec![(10, "Zoom".to_string()), (22, "Discord".to_string())];
        let used = w.observe(430, &both);
        assert_eq!(used, vec![CaptureUse { app: "Zoom".into(), secs: 330 }, CaptureUse { app: "Discord".into(), secs: 0 }]);
        // a long pause ends the use; the next one starts from zero
        assert_eq!(w.observe(470, &[(22, "Discord".to_string())]), vec![CaptureUse { app: "Discord".into(), secs: 40 }]);
        assert_eq!(w.observe(475, &both)[1], CaptureUse { app: "Zoom".into(), secs: 0 });
        // two processes of one program count once, with the longer use
        let tabs = vec![(10, "Zoom".to_string()), (11, "Zoom".to_string()), (22, "Discord".to_string())];
        assert_eq!(w.observe(480, &tabs).iter().filter(|u| u.app == "Zoom").count(), 1);
        w.clear();
        assert!(w.observe(490, &[]).is_empty());
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
