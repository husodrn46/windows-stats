//! Local AI sessions. While a model runs in Ollama, llama.cpp, KoboldCpp or LocalAI (or a
//! program named in `ai_processes`), Windows Stats notes how hard the machine works: peak and
//! average CPU and GPU, peak video memory, memory and temperatures, how long the runtime was
//! busy. The user is asked once, the first time a model is seen (`ai_watch`).
//!
//! Detection looks at the process list every few seconds on the collector thread (one
//! NtQuerySystemInformation call). A runtime that is merely open does not count: Ollama's
//! server only becomes a session when it starts a model runner. The model's name is asked from
//! the runtime's own local address (127.0.0.1); nothing leaves the computer. Sessions are kept
//! in `ai-sessions.jsonl` next to the settings, the running one in `ai-session-live.json` (for
//! the MCP server).

use crate::metrics::processes::Entry;
use crate::metrics::MetricsData;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Look for a runtime this often while none runs.
const SCAN_IDLE: Duration = Duration::from_secs(3);
/// A session ends when its processes have been gone this many looks in a row.
const MISSES_TO_END: u32 = 2;
/// The runtime counts as busy above this share of the whole processor, or the GPU above GPU_BUSY.
const CPU_BUSY: f32 = 0.05;
const GPU_BUSY: f32 = 0.10;
/// Sessions kept in the file.
const KEEP: usize = 200;
/// A start card waits this long for the model's name.
const NAME_WAIT: Duration = Duration::from_millis(2500);

/// A runtime that holds a model, and the processes doing so.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub runtime: &'static str,
    pub pids: Vec<u32>,
}

/// Which runtime runs a model now, if any. Ollama counts only with a model runner (a child
/// `ollama.exe` of the server, or `ollama_llama_server.exe` in older versions).
pub fn detect(entries: &[Entry], custom: &[String]) -> Option<Found> {
    let lower = |e: &Entry| e.name.to_ascii_lowercase();
    let names: HashMap<u32, String> = entries.iter().map(|e| (e.pid, lower(e))).collect();
    let pick = |runtime: &'static str, f: &dyn Fn(&Entry, &str) -> bool| -> Option<Found> {
        let pids: Vec<u32> = entries.iter().filter(|e| f(e, &lower(e))).map(|e| e.pid).collect();
        (!pids.is_empty()).then_some(Found { runtime, pids })
    };
    pick("Ollama", &|e, n| n == "ollama_llama_server.exe" || (n == "ollama.exe" && names.get(&e.parent).map_or(false, |p| p == "ollama.exe")))
        .or_else(|| pick("llama.cpp", &|_, n| matches!(n, "llama-server.exe" | "llama-cli.exe" | "llama-run.exe" | "llama-mtmd-cli.exe")))
        .or_else(|| pick("KoboldCpp", &|_, n| n.starts_with("koboldcpp") && n.ends_with(".exe")))
        .or_else(|| pick("LocalAI", &|_, n| n == "local-ai.exe"))
        .or_else(|| {
            let wanted: Vec<String> = custom.iter().map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty()).map(|c| if c.ends_with(".exe") { c } else { format!("{c}.exe") }).collect();
            pick("custom", &|_, n| wanted.iter().any(|w| w == n)).map(|mut f| {
                // the program's own name, as the user wrote it
                f.runtime = "custom";
                f
            })
        })
}

/// One session as kept on disk and shown in the settings (times in seconds since 1970, UTC).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub runtime: String,
    #[serde(default)]
    pub model: Option<String>,
    /// video memory the runtime reported for the model, bytes
    #[serde(default)]
    pub model_vram: Option<u64>,
    pub start: u64,
    pub end: u64,
    pub seconds: u64,
    /// seconds the runtime was computing (CPU or GPU busy), not just holding the model
    pub busy_seconds: u64,
    pub peak_cpu: f32,
    pub avg_cpu: f32,
    #[serde(default)]
    pub peak_gpu: Option<f32>,
    #[serde(default)]
    pub avg_gpu: Option<f32>,
    pub peak_vram: u64,
    pub peak_ram: u64,
    /// the runtime's own private memory at its highest
    pub peak_runtime_ram: u64,
    /// the runtime's own share of the whole processor at its highest
    pub peak_runtime_cpu: f32,
    #[serde(default)]
    pub peak_cpu_temp: Option<f32>,
    #[serde(default)]
    pub peak_gpu_temp: Option<f32>,
    #[serde(default)]
    pub peak_disk_temp: Option<f32>,
}

fn max_opt(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The running session and what its averages are made of.
struct Live {
    s: Session,
    pids: Vec<u32>,
    misses: u32,
    cpu_sum: f64,
    gpu_sum: f64,
    gpu_n: u32,
    n: u32,
    /// runtime processes' CPU time at the previous look, for their own load
    prev: HashMap<u32, i64>,
    prev_at: Option<Instant>,
    /// the start card is still due (waiting a moment for the model's name)
    announce: bool,
    born: Instant,
    /// busy time in milliseconds (busy_seconds is its whole seconds)
    busy_ms: u64,
    written: Option<Instant>,
}

impl Live {
    fn new(runtime: &str, pids: Vec<u32>, now_secs: u64) -> Live {
        Live {
            s: Session { runtime: runtime.to_string(), start: now_secs, end: now_secs, ..Default::default() },
            pids,
            misses: 0,
            cpu_sum: 0.0,
            gpu_sum: 0.0,
            gpu_n: 0,
            n: 0,
            prev: HashMap::new(),
            prev_at: None,
            announce: true,
            born: Instant::now(),
            busy_ms: 0,
            written: None,
        }
    }

    /// Fold one sample in: the machine's figures and the runtime's processes.
    fn sample(&mut self, d: &MetricsData, entries: &[Entry], cores: usize, now_secs: u64) {
        let now = Instant::now();
        let dt = self.prev_at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        let mut cur = HashMap::new();
        let (mut runtime_cpu_time, mut runtime_ram) = (0i64, 0u64);
        for e in entries.iter().filter(|e| self.pids.contains(&e.pid)) {
            if let Some(prev) = self.prev.get(&e.pid) {
                runtime_cpu_time += (e.cpu_time - prev).max(0);
            }
            cur.insert(e.pid, e.cpu_time);
            runtime_ram += e.private;
        }
        let runtime_cpu = if dt > 0.0 { ((runtime_cpu_time as f64 / 1e7 / dt / cores.max(1) as f64) as f32).clamp(0.0, 1.0) } else { 0.0 };
        self.prev = cur;
        self.prev_at = Some(now);

        let s = &mut self.s;
        s.end = now_secs;
        s.seconds = now_secs.saturating_sub(s.start);
        s.peak_cpu = s.peak_cpu.max(d.cpu_total);
        s.peak_gpu = max_opt(s.peak_gpu, d.gpu);
        s.peak_vram = s.peak_vram.max(d.vram_used);
        s.peak_ram = s.peak_ram.max(d.mem.used);
        s.peak_runtime_ram = s.peak_runtime_ram.max(runtime_ram);
        s.peak_runtime_cpu = s.peak_runtime_cpu.max(runtime_cpu);
        s.peak_cpu_temp = max_opt(s.peak_cpu_temp, d.cpu_temp);
        s.peak_gpu_temp = max_opt(s.peak_gpu_temp, d.gpu_temp);
        s.peak_disk_temp = max_opt(s.peak_disk_temp, d.disks.iter().filter_map(|k| k.temp_c).fold(None, |a: Option<f32>, t| Some(a.map_or(t, |a| a.max(t)))));
        self.n += 1;
        self.cpu_sum += d.cpu_total as f64;
        s.avg_cpu = (self.cpu_sum / self.n as f64) as f32;
        if let Some(g) = d.gpu {
            self.gpu_n += 1;
            self.gpu_sum += g as f64;
            s.avg_gpu = Some((self.gpu_sum / self.gpu_n as f64) as f32);
        }
        if dt > 0.0 && (runtime_cpu > CPU_BUSY || d.gpu.map_or(false, |g| g > GPU_BUSY)) {
            // milliseconds add up, so short samples do not round to zero
            self.busy_ms += (dt * 1000.0) as u64;
            s.busy_seconds = self.busy_ms / 1000;
        }
    }
}

/// What the collector reports to the window: posted with WM_APP_AI.
#[derive(Clone, Debug)]
pub enum Event {
    /// a model runs and the user has not decided yet
    Detected(String),
    Started { runtime: String, model: Option<String> },
    Ended(Session),
}

pub const WM_APP_AI: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 7;

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

pub fn take_events() -> Vec<Event> {
    EVENTS.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
}

fn post(hwnd: isize, e: Event) {
    if let Ok(mut g) = EVENTS.lock() {
        g.push(e);
    }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(windows::Win32::Foundation::HWND(hwnd as *mut _), WM_APP_AI, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn sessions_path() -> std::path::PathBuf {
    crate::util::config_dir().join("ai-sessions.jsonl")
}

fn live_path() -> std::path::PathBuf {
    crate::util::config_dir().join("ai-session-live.json")
}

/// The newest `n` saved sessions, newest first.
pub fn recent(n: usize) -> Vec<Session> {
    let text = std::fs::read_to_string(sessions_path()).unwrap_or_default();
    let mut v: Vec<Session> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    v.reverse();
    v.truncate(n);
    v
}

/// The running session as last written by the app (None when none runs).
pub fn live() -> Option<Session> {
    let text = std::fs::read_to_string(live_path()).ok()?;
    let s: Session = serde_json::from_str(&text).ok()?;
    // a file left behind by a crash is stale after a minute without updates
    (now_secs().saturating_sub(s.end) < 60).then_some(s)
}

fn save(s: &Session) {
    let path = sessions_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
    if let Ok(line) = serde_json::to_string(s) {
        lines.push(line);
    }
    let start = lines.len().saturating_sub(KEEP);
    let _ = std::fs::write(&path, lines[start..].join("\n") + "\n");
    let _ = std::fs::remove_file(live_path());
}

/// Forget every saved session (Settings > AI).
pub fn clear() {
    let _ = std::fs::remove_file(sessions_path());
}

/// The collector's side: looks for runtimes, follows a session, reports to the window.
pub struct Watch {
    buf: Vec<u8>,
    live: Option<Live>,
    next_scan: Instant,
    /// the model's name from the runtime's local address, filled in by a thread
    name: Arc<Mutex<Option<(String, Option<u64>)>>>,
    hwnd: isize,
}

impl Watch {
    pub fn new(notify: windows::Win32::Foundation::HWND) -> Watch {
        Watch { buf: Vec::new(), live: None, next_scan: Instant::now(), name: Arc::new(Mutex::new(None)), hwnd: notify.0 as isize }
    }

    /// A session is being followed (the collector then refreshes video memory every sample).
    pub fn active(&self) -> bool {
        self.live.is_some()
    }

    pub fn current(&self) -> Option<Session> {
        self.live.as_ref().map(|l| l.s.clone())
    }

    /// After each sample. `allowed`: the user's answer (None = not asked yet).
    pub fn tick(&mut self, allowed: Option<bool>, d: &MetricsData, custom: &[String]) {
        if allowed == Some(false) {
            if self.live.take().is_some() {
                let _ = std::fs::remove_file(live_path());
            }
            return;
        }
        let now = Instant::now();
        if self.live.is_none() && now < self.next_scan {
            return;
        }
        self.next_scan = now + SCAN_IDLE;
        let entries = crate::metrics::processes::entries(&mut self.buf);
        let found = detect(&entries, custom);
        let secs = now_secs();
        match (self.live.as_mut(), found) {
            (Some(live), found) => {
                match found {
                    Some(f) if f.runtime == live.s.runtime => {
                        live.pids = f.pids;
                        live.misses = 0;
                    }
                    _ => live.misses += 1,
                }
                if live.misses >= MISSES_TO_END {
                    let mut s = self.live.take().map(|l| l.s).unwrap_or_default();
                    s.end = secs;
                    s.seconds = s.end.saturating_sub(s.start);
                    if allowed == Some(true) {
                        crate::log!("ai: {} session ended after {} s", s.runtime, s.seconds);
                        save(&s);
                        post(self.hwnd, Event::Ended(s));
                    } else {
                        let _ = std::fs::remove_file(live_path());
                    }
                    return;
                }
                if let Ok(mut g) = self.name.lock() {
                    if let Some((model, vram)) = g.take() {
                        live.s.model = Some(model);
                        live.s.model_vram = vram;
                    }
                }
                live.sample(d, &entries, d.cpu_cores.len(), secs);
                if live.announce && (live.s.model.is_some() || now.duration_since(live.born) >= NAME_WAIT) {
                    live.announce = false;
                    if allowed == Some(true) {
                        post(self.hwnd, Event::Started { runtime: live.s.runtime.clone(), model: live.s.model.clone() });
                    }
                }
                if allowed == Some(true) && live.written.map_or(true, |t| now.duration_since(t) >= Duration::from_secs(5)) {
                    live.written = Some(now);
                    if let Ok(text) = serde_json::to_string(&live.s) {
                        let _ = std::fs::write(live_path(), text);
                    }
                }
            }
            (None, Some(f)) => {
                crate::log!("ai: {} is running a model", f.runtime);
                let runtime = if f.runtime == "custom" { custom_name(&entries, &f) } else { f.runtime.to_string() };
                let mut live = Live::new(&runtime, f.pids, secs);
                live.sample(d, &entries, d.cpu_cores.len(), secs);
                self.live = Some(live);
                if let Some(port_path) = api_of(f.runtime) {
                    ask_model_name(port_path, self.name.clone());
                }
                if allowed.is_none() {
                    post(self.hwnd, Event::Detected(runtime));
                }
            }
            (None, None) => {}
        }
    }
}

fn custom_name(entries: &[Entry], f: &Found) -> String {
    entries.iter().find(|e| f.pids.contains(&e.pid)).map(|e| e.name.trim_end_matches(".exe").trim_end_matches(".EXE").to_string()).unwrap_or_else(|| "AI".into())
}

/// Where a runtime tells which model it has loaded: (port, path) on 127.0.0.1.
fn api_of(runtime: &str) -> Option<(u16, &'static str)> {
    match runtime {
        "Ollama" => Some((11434, "/api/ps")),
        "llama.cpp" | "LocalAI" => Some((8080, "/v1/models")),
        "KoboldCpp" => Some((5001, "/api/v1/model")),
        _ => None,
    }
}

/// The model's name (and the video memory Ollama gives it) from a runtime's answer.
pub fn model_from(runtime_api_path: &str, body: &str) -> Option<(String, Option<u64>)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    match runtime_api_path {
        "/api/ps" => {
            let models = v.get("models")?.as_array()?;
            let names: Vec<&str> = models.iter().filter_map(|m| m.get("name").and_then(|n| n.as_str())).collect();
            if names.is_empty() {
                return None;
            }
            let vram: u64 = models.iter().filter_map(|m| m.get("size_vram").and_then(|n| n.as_u64())).sum();
            Some((names.join(", "), (vram > 0).then_some(vram)))
        }
        "/v1/models" => {
            let id = v.get("data")?.as_array()?.first()?.get("id")?.as_str()?;
            // a path to a .gguf file: just its name
            let base = id.rsplit(['/', '\\']).next().unwrap_or(id);
            Some((base.trim_end_matches(".gguf").to_string(), None))
        }
        "/api/v1/model" => {
            let r = v.get("result")?.as_str()?;
            Some((r.trim_start_matches("koboldcpp/").to_string(), None))
        }
        _ => None,
    }
}

/// Ask the runtime at 127.0.0.1:`port` on a thread; the answer lands in `slot`.
fn ask_model_name((port, path): (u16, &'static str), slot: Arc<Mutex<Option<(String, Option<u64>)>>>) {
    let _ = std::thread::Builder::new().name("ai-model-name".into()).spawn(move || {
        // the runner may still be loading: a few tries
        for _ in 0..5 {
            if let Some(body) = loopback_get(port, path) {
                if let Some(found) = model_from(path, &body) {
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(found);
                    }
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// Plain HTTP GET to 127.0.0.1 only (a runtime's own local address), short timeouts.
fn loopback_get(port: u16, path: &str) -> Option<String> {
    use crate::util::{pcwstr, wide};
    use windows::core::PCWSTR;
    use windows::Win32::Networking::WinHttp::*;
    struct H(*mut core::ffi::c_void);
    impl Drop for H {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }
    unsafe {
        let agent = wide(&format!("WindowsStats/{}", crate::about::VERSION));
        let session = H(WinHttpOpen(pcwstr(&agent), WINHTTP_ACCESS_TYPE_NO_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return None;
        }
        WinHttpSetTimeouts(session.0, 1000, 1000, 1500, 1500).ok()?;
        let host = wide("127.0.0.1");
        let conn = H(WinHttpConnect(session.0, pcwstr(&host), port, 0));
        if conn.0.is_null() {
            return None;
        }
        let verb = wide("GET");
        let p = wide(path);
        let req = H(WinHttpOpenRequest(conn.0, pcwstr(&verb), pcwstr(&p), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_OPEN_REQUEST_FLAGS(0)));
        if req.0.is_null() {
            return None;
        }
        WinHttpSendRequest(req.0, None, None, 0, 0, 0).ok()?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).ok()?;
        let mut status = 0u32;
        let mut len = 4u32;
        WinHttpQueryHeaders(req.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut _), &mut len, std::ptr::null_mut()).ok()?;
        if status != 200 {
            return None;
        }
        let mut body = Vec::new();
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut avail).ok()?;
            if avail == 0 || body.len() > 256 * 1024 {
                break;
            }
            let start = body.len();
            body.resize(start + avail as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(req.0, body[start..].as_mut_ptr() as *mut _, avail, &mut read).ok()?;
            body.truncate(start + read as usize);
        }
        String::from_utf8(body).ok()
    }
}

/// "12 min · CPU up to 94% · GPU 78% · VRAM 6.1 GB · GPU 71 °C" in the UI language.
pub fn summary(s: &Session) -> String {
    use crate::i18n::{t, tf};
    let pct = |v: f32| format!("{}%", (v * 100.0).round() as i32);
    // a short session in seconds, not "0 min"
    let duration = if s.seconds < 60 { tf("ai.seconds", &[("n", &s.seconds.to_string())]) } else { crate::util::format_duration(s.seconds.min(u32::MAX as u64) as u32) };
    let mut parts = vec![duration, tf("ai.peak_cpu", &[("v", &pct(s.peak_cpu))])];
    if let Some(g) = s.peak_gpu.filter(|g| *g > 0.005) {
        parts.push(format!("GPU {}", pct(g)));
    }
    if s.peak_vram > 0 {
        parts.push(format!("VRAM {}", crate::util::format_bytes(s.peak_vram)));
    }
    parts.push(format!("{} {}", t("ai.ram"), crate::util::format_bytes(s.peak_ram)));
    if let Some(c) = s.peak_cpu_temp {
        parts.push(format!("CPU {c:.0} \u{00B0}C"));
    }
    if let Some(g) = s.peak_gpu_temp {
        parts.push(format!("GPU {g:.0} \u{00B0}C"));
    }
    parts.join(" \u{00B7} ")
}

/// "Ollama · llama3.1:8b" or the runtime alone.
pub fn title(s: &Session) -> String {
    match &s.model {
        Some(m) => format!("{} \u{00B7} {m}", s.runtime),
        None => s.runtime.clone(),
    }
}

/// Local date and time of a session's start, "3 Nov 14:02" style ("3.11 14:02").
pub fn when(start: u64) -> String {
    // the offset of local time now; fine for the recent sessions shown
    let (lt, st) = unsafe { (windows::Win32::System::SystemInformation::GetLocalTime(), windows::Win32::System::SystemInformation::GetSystemTime()) };
    let mins = |t: &windows::Win32::Foundation::SYSTEMTIME| (t.wDay as i64) * 1440 + (t.wHour as i64) * 60 + t.wMinute as i64;
    let mut offset = mins(&lt) - mins(&st);
    // across a month boundary the day difference is ±(days in month - 1): keep it within a day
    if offset > 1440 {
        offset -= 1440 * ((offset + 720) / 1440);
    } else if offset < -1440 {
        offset += 1440 * ((-offset + 720) / 1440);
    }
    let local = start as i64 + offset * 60;
    let days = local.div_euclid(86_400);
    let (_, m, d) = civil(days);
    let secs = local.rem_euclid(86_400);
    format!("{d}.{m:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// Days since 1970-01-01 to (year, month, day).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(pid: u32, parent: u32, name: &str) -> Entry {
        Entry { pid, parent, name: name.into(), cpu_time: 0, private: 0 }
    }

    #[test]
    fn ollama_counts_only_with_a_runner() {
        let idle = vec![e(10, 1, "ollama app.exe"), e(11, 10, "ollama.exe")];
        assert_eq!(detect(&idle, &[]), None, "the server alone is no session");
        let mut running = idle.clone();
        running.push(e(12, 11, "ollama.exe"));
        assert_eq!(detect(&running, &[]), Some(Found { runtime: "Ollama", pids: vec![12] }));
        let old = vec![e(11, 10, "ollama.exe"), e(13, 11, "ollama_llama_server.exe")];
        assert_eq!(detect(&old, &[]).unwrap().pids, vec![13]);
    }

    #[test]
    fn other_runtimes_and_custom_names() {
        assert_eq!(detect(&[e(5, 1, "llama-server.exe")], &[]).unwrap().runtime, "llama.cpp");
        assert_eq!(detect(&[e(5, 1, "Llama-CLI.EXE")], &[]).unwrap().runtime, "llama.cpp");
        assert_eq!(detect(&[e(5, 1, "koboldcpp_cu12.exe")], &[]).unwrap().runtime, "KoboldCpp");
        assert_eq!(detect(&[e(5, 1, "local-ai.exe")], &[]).unwrap().runtime, "LocalAI");
        assert_eq!(detect(&[e(5, 1, "notepad.exe"), e(6, 1, "server.exe")], &[]), None);
        let custom = vec!["MyRunner".to_string()];
        let f = detect(&[e(7, 1, "myrunner.exe")], &custom).unwrap();
        assert_eq!(f.runtime, "custom");
        assert_eq!(custom_name(&[e(7, 1, "MyRunner.exe")], &f), "MyRunner");
    }

    #[test]
    fn peaks_and_averages() {
        let mut live = Live::new("Ollama", vec![12], 1000);
        let mut d = MetricsData { cpu_total: 0.2, gpu: Some(0.5), vram_used: 3_000, gpu_temp: Some(60.0), ..Default::default() };
        d.mem.used = 8_000;
        live.sample(&d, &[], 16, 1001);
        d.cpu_total = 0.9;
        d.gpu = Some(0.1);
        d.vram_used = 6_000;
        d.gpu_temp = Some(71.0);
        d.mem.used = 9_000;
        live.sample(&d, &[], 16, 1012);
        let s = &live.s;
        assert_eq!(s.peak_cpu, 0.9);
        assert!((s.avg_cpu - 0.55).abs() < 1e-6);
        assert_eq!(s.peak_gpu, Some(0.5));
        assert!((s.avg_gpu.unwrap() - 0.3).abs() < 1e-6);
        assert_eq!((s.peak_vram, s.peak_ram), (6_000, 9_000));
        assert_eq!(s.peak_gpu_temp, Some(71.0));
        assert_eq!(s.peak_cpu_temp, None, "no CPU temperature on this machine stays none");
        assert_eq!(s.seconds, 12);
    }

    #[test]
    fn model_names_from_the_runtimes() {
        let ollama = r#"{"models":[{"name":"llama3.1:8b","size":5000,"size_vram":4900000000}]}"#;
        assert_eq!(model_from("/api/ps", ollama), Some(("llama3.1:8b".into(), Some(4_900_000_000))));
        assert_eq!(model_from("/api/ps", r#"{"models":[]}"#), None);
        let llama = r#"{"object":"list","data":[{"id":"C:\\models\\qwen2.5-0.5b-instruct-q4_k_m.gguf","object":"model"}]}"#;
        assert_eq!(model_from("/v1/models", llama), Some(("qwen2.5-0.5b-instruct-q4_k_m".into(), None)));
        assert_eq!(model_from("/api/v1/model", r#"{"result":"koboldcpp/Mistral-7B"}"#), Some(("Mistral-7B".into(), None)));
        assert_eq!(model_from("/api/ps", "not json"), None);
    }

    #[test]
    fn sessions_round_trip_and_dates() {
        let s = Session { runtime: "llama.cpp".into(), model: Some("qwen".into()), start: 1_793_700_000, end: 1_793_700_600, seconds: 600, peak_cpu: 0.94, ..Default::default() };
        let back: Session = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        // older lines without the optional fields still read
        let old: Session = serde_json::from_str(r#"{"runtime":"Ollama","start":1,"end":2,"seconds":1,"busy_seconds":0,"peak_cpu":0.5,"avg_cpu":0.4,"peak_vram":0,"peak_ram":0,"peak_runtime_ram":0,"peak_runtime_cpu":0.0}"#).unwrap();
        assert_eq!(old.model, None);
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_395), (2025, 11, 3));
        assert_eq!(title(&s), "llama.cpp \u{00B7} qwen");
    }
}
