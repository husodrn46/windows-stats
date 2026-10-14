//! Command-line modes used by tests, scripts and the installer:
//!
//! * `--dump [samples] [interval_ms]` runs the collectors without any window and prints one
//!   JSON object per sample, then a summary with the cost of collecting (own CPU time, memory).
//!   `scripts\compare-taskmgr.ps1` compares these values with Windows' own performance counters.
//! * `--quit` asks a running Windows Stats to close (cleanly: the taskbar is put back).
//! * `--version`, `--help`.

use crate::metrics::{Metrics, MetricsData};
use crate::util::{pcwstr, wide};
use std::io::Write;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};

/// Handles a command-line mode; false when the app should start normally.
pub fn run(args: &[String]) -> bool {
    let Some(first) = args.first() else { return false };
    match first.as_str() {
        "--dump" => {
            attach_console();
            let samples = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5usize).clamp(1, 100_000);
            let interval = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1000u64).clamp(100, 60_000);
            dump(samples, interval);
            true
        }
        "--taskbar-items" => {
            // diagnostics: what UI Automation reports below the taskbar (class, name, id, rect)
            attach_console();
            let tray = unsafe { FindWindowW(windows::core::w!("Shell_TrayWnd"), windows::core::PCWSTR::null()) }.unwrap_or_default();
            let t = tray.0 as isize;
            match crate::taskbar_uia::with_com(move || crate::taskbar_uia::all_items(windows::Win32::Foundation::HWND(t as *mut _)).map_err(|e| e.to_string())) {
                Some(Ok(items)) => {
                    for it in &items {
                        println!("{:<48} [{:>5},{:>5},{:>5},{:>5}] id={} name={}", it.class, it.rect.left, it.rect.top, it.rect.right, it.rect.bottom, it.id, it.name);
                    }
                    println!("occupied (all items): {:?}", crate::taskbar_uia::occupied(&items));
                    // the cached query the watcher uses, timed
                    let timed = crate::taskbar_uia::with_com(move || {
                        let q = crate::taskbar_uia::Query::new().map_err(|e| e.to_string())?;
                        let t0 = std::time::Instant::now();
                        let n = 10;
                        let mut last = Vec::new();
                        for _ in 0..n {
                            last = q.items(windows::Win32::Foundation::HWND(t as *mut _)).map_err(|e| e.to_string())?;
                        }
                        Ok::<_, String>((t0.elapsed().as_secs_f64() * 1000.0 / n as f64, last))
                    });
                    match timed {
                        Some(Ok((ms, items))) => println!("cached query: {} items, {ms:.1} ms per query, occupied {:?}", items.len(), crate::taskbar_uia::occupied(&items)),
                        Some(Err(e)) => println!("cached query failed: {e}"),
                        None => println!("cached query thread failed"),
                    }
                }
                Some(Err(e)) => println!("UI Automation failed: {e}"),
                None => println!("UI Automation thread failed"),
            }
            true
        }
        "--settings" => {
            // a running instance opens its settings window; otherwise the app starts and opens it
            if post_running(crate::WM_APP_OPEN_SETTINGS) {
                return true;
            }
            crate::OPEN_SETTINGS_AT_START.store(true, std::sync::atomic::Ordering::Relaxed);
            false
        }
        "--quit" => {
            attach_console();
            let found = quit_running();
            println!("{}", if found { "Windows Stats closing" } else { "Windows Stats is not running" });
            true
        }
        "--version" => {
            attach_console();
            println!("Windows Stats {}", crate::about::VERSION);
            true
        }
        "--help" | "-h" | "/?" => {
            attach_console();
            println!("Windows Stats {}\n\nWindowsStats.exe                 start (taskbar widgets)\nWindowsStats.exe --dump [n] [ms] print n samples as JSON lines\nWindowsStats.exe --quit          close the running instance\nWindowsStats.exe --version", crate::about::VERSION);
            true
        }
        _ => false,
    }
}

/// The release build has no console of its own: print into the one we were started from,
/// unless the output is already redirected (a pipe or a file from a script).
fn attach_console() {
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    unsafe {
        let redirected = GetStdHandle(STD_OUTPUT_HANDLE).map_or(false, |h| !h.is_invalid() && !h.0.is_null());
        if !redirected {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Post `msg` to the hidden window of the running instance (same settings home).
pub fn post_running(msg: u32) -> bool {
    let cls = wide(crate::CLASS_HIDDEN);
    let title = wide(&crate::util::hidden_title());
    unsafe {
        match FindWindowW(pcwstr(&cls), pcwstr(&title)) {
            Ok(h) if !h.0.is_null() => PostMessageW(h, msg, WPARAM(0), LPARAM(0)).is_ok(),
            _ => false,
        }
    }
}

/// Post WM_CLOSE to the hidden window of the running instance (the one using the same settings
/// home as this process).
pub fn quit_running() -> bool {
    let cls = wide(crate::CLASS_HIDDEN);
    let title = wide(&crate::util::hidden_title());
    unsafe {
        match FindWindowW(pcwstr(&cls), pcwstr(&title)) {
            Ok(h) if !h.0.is_null() => PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok(),
            _ => false,
        }
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn opt(v: Option<f32>) -> String {
    v.map(|v| format!("{v:.4}")).unwrap_or_else(|| "null".into())
}

/// One sample as a JSON object (values as fractions 0..1, rates in bytes/s, sizes in bytes).
pub fn sample_json(t_ms: u128, took_ms: f64, m: &MetricsData) -> String {
    let cores: Vec<String> = m.cpu_cores.iter().map(|v| format!("{v:.4}")).collect();
    let battery = match m.battery {
        Some(b) => format!("{{\"level\":{:.3},\"charging\":{},\"on_ac\":{}}}", b.level, b.charging, b.on_ac),
        None => "null".into(),
    };
    format!(
        "{{\"t_ms\":{t_ms},\"sample_ms\":{took_ms:.2},\"gpu_temp\":{},\"disks\":[{}],\"cpu\":{:.4},\"cpu_system\":{:.4},\"cpu_user\":{:.4},\"cpu_cores\":[{}],\"cpu_clock_mhz\":{},\"mem_used\":{},\"mem_total\":{},\"mem_pct\":{:.4},\"gpu\":{},\"net_rx\":{:.0},\"net_tx\":{:.0},\"disk_read\":{:.0},\"disk_write\":{:.0},\"battery\":{battery}}}",
        opt(m.gpu_temp),
        m.disks
            .iter()
            .map(|d| format!("{{\"model\":{},\"bus\":{},\"ssd\":{},\"temp_c\":{}}}", json_str(&d.model), json_str(d.bus), d.ssd.map(|v| v.to_string()).unwrap_or_else(|| "null".into()), opt(d.temp_c)))
            .collect::<Vec<_>>()
            .join(","),
        m.cpu_total,
        m.cpu_system,
        m.cpu_user,
        cores.join(","),
        m.cpu_clock(None).map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
        m.mem.used,
        m.mem.total,
        m.mem.pct,
        opt(m.gpu),
        m.net_rx,
        m.net_tx,
        m.disk_read,
        m.disk_write,
    )
}

fn dump(samples: usize, interval_ms: u64) {
    let cfg = crate::config::Config::load_readonly();
    let start = Instant::now();
    let mut m = Metrics::new(&cfg.disk, cfg.top_processes as usize, &cfg.net_disabled, &cfg.disks_disabled);
    m.cpu_utility = cfg.cpu_utility;
    // the first update only primes the deltas (and fills the core list)
    m.update(false);
    let init_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "{{\"info\":{{\"version\":{},\"cpu\":{},\"cores\":{},\"gpu\":{},\"gpu_available\":{},\"battery_available\":{},\"init_ms\":{init_ms:.1}}}}}",
        json_str(crate::about::VERSION),
        json_str(&m.data.cpu_name),
        m.data.cpu_cores.len(),
        json_str(&m.data.gpu_name),
        m.data.gpu_available,
        m.data.battery_available
    );
    let mut next = Instant::now() + Duration::from_millis(interval_ms);
    let mut sample_total = 0f64;
    for _ in 0..samples {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        next += Duration::from_millis(interval_ms);
        let t0 = Instant::now();
        m.update(false);
        let took = t0.elapsed().as_secs_f64() * 1000.0;
        sample_total += took;
        let _ = writeln!(out, "{}", sample_json(start.elapsed().as_millis(), took, &m.data));
        let _ = out.flush();
    }
    let (cpu_s, private, peak) = own_usage();
    let _ = writeln!(
        out,
        "{{\"summary\":{{\"samples\":{samples},\"interval_ms\":{interval_ms},\"avg_sample_ms\":{:.2},\"process_cpu_s\":{cpu_s:.3},\"private_bytes\":{private},\"peak_working_set\":{peak}}}}}",
        sample_total / samples as f64
    );
}

/// (CPU seconds used by this process, private bytes, peak working set).
pub fn own_usage() -> (f64, u64, u64) {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let h = GetCurrentProcess();
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let cpu = if GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok() {
            let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 1e7;
            f(k) + f(u)
        } else {
            0.0
        };
        let mut pmc = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        let ok = GetProcessMemoryInfo(h, &mut pmc as *mut _ as *mut PROCESS_MEMORY_COUNTERS, pmc.cb).is_ok();
        if ok {
            (cpu, pmc.PrivateUsage as u64, pmc.PeakWorkingSetSize as u64)
        } else {
            (cpu, 0, 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(json_str("İşlemci"), "\"İşlemci\"");
    }

    #[test]
    fn sample_is_one_json_line() {
        let mut m = MetricsData::default();
        m.cpu_cores = vec![0.5, 0.25];
        m.cpu_total = 0.375;
        let s = sample_json(1000, 1.5, &m);
        assert!(s.starts_with('{') && s.ends_with('}'));
        assert!(!s.contains('\n'));
        assert!(s.contains("\"cpu\":0.3750"));
        assert!(s.contains("\"cpu_cores\":[0.5000,0.2500]"));
        assert!(s.contains("\"gpu\":null"));
        assert!(s.contains("\"battery\":null"));
    }
}
