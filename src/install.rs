//! Per-user installation without administrator rights:
//!
//! * `--install`: copies the program (with the sensor helper and the licences) to
//!   %LOCALAPPDATA%\Programs\Windows Stats, adds a Start menu shortcut and an entry in
//!   Settings > Apps (HKCU), optionally starts it with Windows, and launches it;
//! * `--uninstall` (what Settings > Apps runs): closes the running app, removes the autostart
//!   value, the shortcut, the Apps entry and the program folder; the settings folder only when
//!   the user says so. Nothing outside the user's profile is touched.

use crate::i18n::{t, tf};
use crate::util::{pcwstr, wide};
use std::path::{Path, PathBuf};
use windows::core::{Interface, PCWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Registry::{RegDeleteTreeW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO};

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\WindowsStats";

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Programs").join("Windows Stats")
}

fn shortcut_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join(r"Microsoft\Windows\Start Menu\Programs").join("Windows Stats.lnk")
}

fn message(text: &str, flags: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    let title = wide("Windows Stats");
    let text = wide(text);
    unsafe { MessageBoxW(None, pcwstr(&text), pcwstr(&title), flags) }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<u64> {
    std::fs::create_dir_all(to)?;
    let mut size = 0;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            size += copy_dir(&e.path(), &target)?;
        } else {
            size += std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(size)
}

fn set_sz(name: &str, value: &str) {
    let k = wide(UNINSTALL_KEY);
    let n = wide(name);
    let v = wide(value);
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, pcwstr(&k), pcwstr(&n), REG_SZ.0, Some(v.as_ptr() as *const _), (v.len() * 2) as u32);
    }
}

fn set_dword(name: &str, value: u32) {
    let k = wide(UNINSTALL_KEY);
    let n = wide(name);
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, pcwstr(&k), pcwstr(&n), REG_DWORD.0, Some(&value as *const u32 as *const _), 4);
    }
}

fn create_shortcut(target: &Path, lnk: &Path) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let t = wide(&target.display().to_string());
        let dir = wide(&target.parent().map(|p| p.display().to_string()).unwrap_or_default());
        let desc = wide("Windows Stats");
        link.SetPath(pcwstr(&t))?;
        link.SetWorkingDirectory(pcwstr(&dir))?;
        link.SetDescription(pcwstr(&desc))?;
        link.SetIconLocation(pcwstr(&t), 0)?;
        let file: IPersistFile = link.cast()?;
        let l = wide(&lnk.display().to_string());
        file.Save(pcwstr(&l), true)?;
        Ok(())
    }
}

/// Wait for a running instance to close after `--quit` (up to ~5 s).
fn close_running() {
    if crate::cli::quit_running() {
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !crate::cli::post_running(windows::Win32::UI::WindowsAndMessaging::WM_NULL) {
                break;
            }
        }
    }
}

pub fn install(quiet: bool) {
    let Ok(exe) = std::env::current_exe() else { return };
    let src = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = install_dir();
    if src == dir {
        if !quiet {
            message(&t("install.already"), MB_OK | MB_ICONINFORMATION);
        }
        return;
    }
    close_running();
    let mut size: u64 = 0;
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        size += std::fs::copy(&exe, dir.join("WindowsStats.exe"))?;
        for f in ["LICENSE.txt", "THIRD-PARTY-NOTICES.txt", "README.md"] {
            if src.join(f).exists() {
                size += std::fs::copy(src.join(f), dir.join(f))?;
            }
        }
        for d in ["sensors", "licenses"] {
            if src.join(d).is_dir() {
                size += copy_dir(&src.join(d), &dir.join(d))?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        crate::log!("install failed: {e}");
        if !quiet {
            message(&tf("install.failed", &[("err", &e.to_string())]), MB_OK | MB_ICONWARNING);
        }
        return;
    }
    let installed = dir.join("WindowsStats.exe");
    if let Err(e) = create_shortcut(&installed, &shortcut_path()) {
        crate::log!("install: shortcut failed: {e}");
    }
    let quoted = format!("\"{}\"", installed.display());
    set_sz("DisplayName", "Windows Stats");
    set_sz("DisplayVersion", crate::about::VERSION);
    set_sz("Publisher", "Windows Stats");
    set_sz("DisplayIcon", &installed.display().to_string());
    set_sz("InstallLocation", &dir.display().to_string());
    set_sz("UninstallString", &format!("{quoted} --uninstall"));
    set_sz("QuietUninstallString", &format!("{quoted} --uninstall --quiet"));
    set_dword("NoModify", 1);
    set_dword("NoRepair", 1);
    set_dword("EstimatedSize", (size / 1024) as u32);
    crate::log!("installed to {}", dir.display());
    // the autostart value points at the installed copy
    if quiet {
        // scripted: no questions, no autostart, not started
        return;
    }
    if message(&t("install.autostart"), MB_YESNO | MB_ICONQUESTION) == IDYES {
        crate::util::set_autostart_path(&installed, true);
    }
    let _ = std::process::Command::new(&installed).spawn();
    message(&tf("install.done", &[("dir", &dir.display().to_string())]), MB_OK | MB_ICONINFORMATION);
}

pub fn uninstall(quiet: bool) {
    if !quiet && message(&t("install.confirm_uninstall"), MB_YESNO | MB_ICONQUESTION) != IDYES {
        return;
    }
    close_running();
    crate::util::set_autostart(false);
    let _ = std::fs::remove_file(shortcut_path());
    let k = wide(UNINSTALL_KEY);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, pcwstr(&k));
    }
    if !quiet && message(&t("install.remove_settings"), MB_YESNO | MB_ICONQUESTION) == IDYES {
        let _ = std::fs::remove_dir_all(crate::util::config_dir());
    }
    // the running executable cannot delete its own folder: a hidden cmd does it once we exit
    let dir = install_dir();
    if dir.exists() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = format!("ping 127.0.0.1 -n 3 > nul & rmdir /s /q \"{}\"", dir.display());
        // raw: cmd.exe does not understand the \" quoting Rust would add
        let _ = std::process::Command::new("cmd.exe").arg("/c").raw_arg(&script).current_dir(std::env::temp_dir()).creation_flags(CREATE_NO_WINDOW).spawn();
    }
    crate::log!("uninstalled");
    if !quiet {
        message(&t("install.removed"), MB_OK | MB_ICONINFORMATION);
    }
    let _ = PCWSTR::null();
}
