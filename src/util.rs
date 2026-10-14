//! Small helpers: wide strings, registry reads, OS version, theme detection, logging.

use std::io::Write;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::core::PCWSTR;

/// Null-terminated UTF-16 buffer.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    PCWSTR::from_raw(buf.as_ptr())
}

pub fn reg_dword_hkcu(subkey: &str, value: &str) -> Option<u32> {
    let k = wide(subkey);
    let v = wide(value);
    let mut data: u32 = 0;
    let mut size: u32 = 4;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            pcwstr(&k),
            pcwstr(&v),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    if r == ERROR_SUCCESS { Some(data) } else { None }
}

pub fn reg_sz_hklm(subkey: &str, value: &str) -> Option<String> {
    let k = wide(subkey);
    let v = wide(value);
    let mut buf = [0u16; 512];
    let mut size: u32 = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            pcwstr(&k),
            pcwstr(&v),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    Some(String::from_utf16_lossy(&buf).trim_end_matches('\0').to_string())
}

pub fn windows_build() -> u32 {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = RtlGetVersion(&mut info);
    }
    info.dwBuildNumber
}

pub fn user32_proc(name: &str) -> Option<unsafe extern "system" fn() -> isize> {
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    let module = unsafe { GetModuleHandleW(windows::core::w!("user32.dll")) }.ok()?;
    let cname = std::ffi::CString::new(name).ok()?;
    unsafe { GetProcAddress(module, windows::core::PCSTR::from_raw(cname.as_ptr() as *const u8)) }
}

/// Per-monitor-v2 DPI awareness (Windows 10 1607+), resolved at run time so the binary still
/// loads on Windows 7/8 where the export does not exist. Returns false when unavailable.
pub fn set_per_monitor_dpi() -> bool {
    type SetCtx = unsafe extern "system" fn(*mut core::ffi::c_void) -> i32;
    let Some(p) = user32_proc("SetProcessDpiAwarenessContext") else { return false };
    // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 == (HANDLE)-4
    unsafe {
        let f: SetCtx = std::mem::transmute(p);
        f(-4isize as *mut core::ffi::c_void) != 0
    }
}

/// GetDpiForWindow (Windows 10 1607+), or None on older systems.
pub fn dpi_for_window(hwnd: windows::Win32::Foundation::HWND) -> Option<u32> {
    type GetDpi = unsafe extern "system" fn(windows::Win32::Foundation::HWND) -> u32;
    let p = user32_proc("GetDpiForWindow")?;
    let d = unsafe {
        let f: GetDpi = std::mem::transmute(p);
        f(hwnd)
    };
    if d == 0 { None } else { Some(d) }
}

pub fn is_windows11() -> bool {
    windows_build() >= 22000
}

/// True when the system (taskbar) theme is light.
pub fn system_light_theme() -> bool {
    reg_dword_hkcu(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "SystemUsesLightTheme",
    ) == Some(1)
}

/// Settings and log live in the user's profile: %APPDATA%\WindowsStats. `WINDOWS_STATS_HOME`
/// overrides it (tests run against a scratch directory and leave the real settings alone).
pub fn config_dir() -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("WINDOWS_STATS_HOME") {
        return std::path::PathBuf::from(home);
    }
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("WindowsStats")
}

/// A separate settings home (`WINDOWS_STATS_HOME`, used by tests) runs as its own instance:
/// this tag goes into the single-instance mutex and the hidden window's title. None normally.
pub fn instance_tag() -> Option<String> {
    use std::hash::{Hash, Hasher};
    let home = std::env::var_os("WINDOWS_STATS_HOME")?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    home.hash(&mut h);
    Some(format!("{:016x}", h.finish()))
}

/// Title of the hidden window, which `--quit` looks for.
pub fn hidden_title() -> String {
    match instance_tag() {
        Some(tag) => format!("Windows Stats [{tag}]"),
        None => "Windows Stats".to_string(),
    }
}

pub fn log_path() -> std::path::PathBuf {
    config_dir().join("WindowsStats.log")
}

/// Keeps the log small: past 1 MB it moves to WindowsStats.log.old at start-up.
pub fn rotate_log() {
    let path = log_path();
    if std::fs::metadata(&path).map(|m| m.len() > 1 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
}

/// "2026-10-13 16:39:12" in local time.
pub fn local_timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

pub fn log(msg: &str) {
    eprintln!("{msg}");
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "[{}] {msg}", local_timestamp());
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::util::log(&format!($($arg)*)) };
}

/// `v` with `decimals` digits after the decimal separator of the UI language.
pub fn format_decimal(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if crate::i18n::decimal_comma() { s.replace('.', ",") } else { s }
}

/// "12 KB/s" / "1.5 MB/s"; in Turkish "12 KB/sn", "1,5 MB/sn" (unit and decimal separator
/// follow the UI language).
pub fn format_bytes_rate(bps: f64) -> String {
    let b = bps.max(0.0);
    let ps = crate::i18n::t("units.per_second");
    const K: f64 = 1024.0;
    if b < K {
        format!("{} B{ps}", format_decimal(b, 0))
    } else if b < K * K {
        format!("{} KB{ps}", format_decimal(b / K, 0))
    } else if b < K * K * K {
        format!("{} MB{ps}", format_decimal(b / K / K, 1))
    } else {
        format!("{} GB{ps}", format_decimal(b / K / K / K, 2))
    }
}

pub fn format_bytes(b: u64) -> String {
    let f = b as f64;
    const K: f64 = 1024.0;
    if f < K * K {
        format!("{} KB", format_decimal(f / K, 0))
    } else if f < K * K * K {
        format!("{} MB", format_decimal(f / K / K, 0))
    } else if f < K * K * K * K {
        format!("{} GB", format_decimal(f / K / K / K, 1))
    } else {
        format!("{} TB", format_decimal(f / K / K / K / K, 2))
    }
}

/// "2 h 05 min" / "2 sa 05 dk", "45 min" / "45 dk".
pub fn format_duration(secs: u32) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        crate::i18n::tf("units.hours_minutes", &[("h", &h.to_string()), ("m", &format!("{m:02}"))])
    } else {
        crate::i18n::tf("units.minutes", &[("m", &m.to_string())])
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "WindowsStats";

/// True when the HKCU Run entry points at this executable.
pub fn autostart_enabled() -> bool {
    let k = wide(RUN_KEY);
    let v = wide(RUN_VALUE);
    let mut buf = [0u16; 1024];
    let mut size: u32 = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, pcwstr(&k), pcwstr(&v), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size))
    };
    r == ERROR_SUCCESS
}

pub fn set_autostart(enable: bool) -> bool {
    let k = wide(RUN_KEY);
    let v = wide(RUN_VALUE);
    unsafe {
        if enable {
            let Ok(exe) = std::env::current_exe() else { return false };
            let data = wide(&format!("\"{}\"", exe.display()));
            RegSetKeyValueW(HKEY_CURRENT_USER, pcwstr(&k), pcwstr(&v), REG_SZ.0, Some(data.as_ptr() as *const _), (data.len() * 2) as u32) == ERROR_SUCCESS
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, pcwstr(&k), pcwstr(&v)) == ERROR_SUCCESS
        }
    }
}

/// "18.85 GB" / "512 MB", Stats-style size with two decimals for gigabytes.
pub fn format_size(b: u64) -> String {
    let f = b as f64;
    const K: f64 = 1024.0;
    if f < K * K * K {
        format!("{} MB", format_decimal(f / K / K, 0))
    } else if f < K * K * K * K {
        format!("{} GB", format_decimal(f / K / K / K, 2))
    } else {
        format!("{} TB", format_decimal(f / K / K / K / K, 2))
    }
}

/// Windows accent colour (DWM colorization), if available.
pub fn system_accent_color() -> Option<crate::canvas::Color> {
    let mut argb: u32 = 0;
    let mut opaque = windows::Win32::Foundation::BOOL(0);
    unsafe { windows::Win32::Graphics::Dwm::DwmGetColorizationColor(&mut argb, &mut opaque).ok()? };
    Some(crate::canvas::Color::rgb((argb >> 16) as u8, (argb >> 8) as u8, argb as u8))
}

/// Put `text` on the clipboard as Unicode text.
pub fn set_clipboard(text: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(HWND::default()).is_err() {
            return false;
        }
        let mut ok = false;
        if EmptyClipboard().is_ok() {
            if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
                let p = GlobalLock(h) as *mut u16;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                    let _ = GlobalUnlock(h);
                    ok = SetClipboardData(CF_UNICODETEXT, HANDLE(h.0)).is_ok();
                }
                if !ok {
                    let _ = GlobalFree(HGLOBAL(h.0));
                }
            }
        }
        let _ = CloseClipboard();
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_follow_the_language() {
        crate::i18n::set_language("tr");
        assert_eq!(format_bytes_rate(1536.0 * 1024.0), "1,5 MB/sn");
        assert_eq!(format_bytes_rate(12.0 * 1024.0), "12 KB/sn");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1,5 GB");
        assert_eq!(format_duration(2 * 3600 + 5 * 60), "2 sa 05 dk");
        assert_eq!(format_duration(45 * 60), "45 dk");
        crate::i18n::set_language("en");
        assert_eq!(format_bytes_rate(1536.0 * 1024.0), "1.5 MB/s");
        assert_eq!(format_duration(45 * 60), "45 min");
    }
}
