//! Optional updates from the project's GitHub releases.
//!
//! Nothing here touches the network unless the user clicks "Check for updates" in Settings >
//! About or agreed to the daily check (asked once, changeable in Settings > Privacy). A check
//! is one HTTPS GET of the latest release from api.github.com; it carries nothing beyond what
//! every HTTPS request does (the address it comes from and a `WindowsStats/<version>` agent).
//!
//! Installing downloads the release archive over HTTPS, requires the exact size and the
//! SHA-256 digest GitHub computed for the asset, unpacks it with Windows' own tar.exe and
//! checks the version of the unpacked executable. That executable is then started with
//! `--update <pid>`: it waits for this process to exit, copies itself over the per-user
//! installation and starts it. No administrator rights, no service, no background downloads.
//! Releases are unsigned, so this is the assurance of downloading the archive by hand from the
//! releases page, without the chance of picking a wrong or damaged file.

use crate::util::{pcwstr, wide};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const REPO: &str = "husodrn46/windows-stats";
const API_LATEST: &str = "https://api.github.com/repos/husodrn46/windows-stats/releases/latest";
/// posted to the hidden window when a check or a download finished (see `take_outcome`)
pub const WM_APP_UPDATE: u32 = WM_APP + 6;
/// automatic checks happen at most this often
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// a release archive is about 2 MB; anything far larger is not ours
const MAX_ARCHIVE: u64 = 64 << 20;
const MAX_JSON: u64 = 4 << 20;

pub type Version = (u32, u32, u32);

/// "0.1.4" or "v0.1.4"; three plain numbers, nothing after them (no pre-releases).
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let num = |p: &str| -> Option<u32> {
        if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    Some((num(parts[0])?, num(parts[1])?, num(parts[2])?))
}

/// The version this program compares releases with. Debug builds can pretend to be older
/// (`WINDOWS_STATS_UPDATE_AS=0.1.2`) to exercise an update end to end.
pub fn current() -> Version {
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var("WINDOWS_STATS_UPDATE_AS").ok().as_deref().and_then(parse_version) {
        return v;
    }
    parse_version(crate::about::VERSION).unwrap_or((0, 0, 0))
}

/// The newest published release, as far as an update needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// "0.1.4"
    pub name: String,
    /// the release page on GitHub (notes, manual download)
    pub page: String,
    pub archive_name: String,
    pub archive_url: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

/// Why a check or an update did not work out; the settings show a sentence per kind, the log
/// gets the details.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Failure {
    /// no connection, a proxy or TLS problem, an HTTP error
    Network,
    /// GitHub answered with something that is not a usable release
    Answer,
    /// the download is not the file GitHub describes (size, digest, version inside)
    Verify,
    /// unpacking or starting the new version failed
    Install,
}

#[derive(Debug)]
pub struct Error {
    pub kind: Failure,
    pub detail: String,
}

fn err(kind: Failure, detail: impl Into<String>) -> Error {
    Error { kind, detail: detail.into() }
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Read GitHub's "latest release" answer. Only a final release of this repository with the
/// expected archive, served from this repository's download path, with a SHA-256 digest is
/// accepted.
pub fn parse_release(json: &str) -> Result<Release, Error> {
    #[derive(Deserialize)]
    struct Raw {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
        #[serde(default)]
        assets: Vec<Asset>,
    }
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        size: u64,
        #[serde(default)]
        digest: Option<String>,
    }
    let raw: Raw = serde_json::from_str(json).map_err(|e| err(Failure::Answer, format!("release JSON: {e}")))?;
    if raw.draft || raw.prerelease {
        return Err(err(Failure::Answer, "the latest release is a draft or a pre-release"));
    }
    let version = parse_version(&raw.tag_name).ok_or_else(|| err(Failure::Answer, format!("release tag {:?}", raw.tag_name)))?;
    let name = format!("{}.{}.{}", version.0, version.1, version.2);
    if raw.tag_name != format!("v{name}") {
        return Err(err(Failure::Answer, format!("release tag {:?}", raw.tag_name)));
    }
    if !raw.html_url.starts_with(&format!("https://github.com/{REPO}/releases/")) {
        return Err(err(Failure::Answer, format!("release page {:?}", raw.html_url)));
    }
    let archive_name = format!("WindowsStats-{name}-x64.zip");
    let asset = raw.assets.iter().find(|a| a.name == archive_name).ok_or_else(|| err(Failure::Answer, format!("{archive_name} is not attached to the release")))?;
    let archive_url = format!("https://github.com/{REPO}/releases/download/v{name}/{archive_name}");
    if asset.browser_download_url != archive_url {
        return Err(err(Failure::Answer, format!("archive URL {:?}", asset.browser_download_url)));
    }
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .and_then(parse_hex32)
        .ok_or_else(|| err(Failure::Answer, "GitHub reported no SHA-256 digest for the archive"))?;
    if asset.size == 0 || asset.size > MAX_ARCHIVE {
        return Err(err(Failure::Answer, format!("archive size {}", asset.size)));
    }
    Ok(Release { version, name, page: raw.html_url, archive_name, archive_url, sha256, size: asset.size })
}

/// "https://host/path" -> (host, path); nothing but HTTPS, a plain host name and a path.
fn split_https(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    let plain = !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    plain.then_some((host, path))
}

struct Internet(*mut core::ffi::c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn net(e: windows::core::Error) -> Error {
    err(Failure::Network, e.message())
}

/// GET `url` over HTTPS with Windows' own HTTP stack (system proxy and certificate checks;
/// redirects are followed, never from HTTPS to HTTP) and return the body of a 200 answer.
fn get(url: &str, headers: &str, limit: u64) -> Result<Vec<u8>, Error> {
    let (host, path) = split_https(url).ok_or_else(|| err(Failure::Answer, format!("not an HTTPS address: {url}")))?;
    unsafe {
        let agent = wide(&format!("WindowsStats/{}", crate::about::VERSION));
        let session = Internet(WinHttpOpen(pcwstr(&agent), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(net(windows::core::Error::from_win32()));
        }
        // resolve, connect, send, receive (ms)
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000).map_err(net)?;
        let host_w = wide(host);
        let connection = Internet(WinHttpConnect(session.0, pcwstr(&host_w), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if connection.0.is_null() {
            return Err(net(windows::core::Error::from_win32()));
        }
        let verb = wide("GET");
        let path_w = wide(path);
        let request = Internet(WinHttpOpenRequest(connection.0, pcwstr(&verb), pcwstr(&path_w), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if request.0.is_null() {
            return Err(net(windows::core::Error::from_win32()));
        }
        let extra: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(request.0, Some(&extra), None, 0, 0, 0).map_err(net)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(net)?;
        let mut status = 0u32;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(net)?;
        if status != 200 {
            return Err(err(Failure::Network, format!("HTTP {status} from {host}")));
        }
        let mut body = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut available).map_err(net)?;
            if available == 0 {
                break;
            }
            if body.len() as u64 + available as u64 > limit {
                return Err(err(Failure::Verify, format!("more than {limit} bytes from {host}")));
            }
            let start = body.len();
            body.resize(start + available as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(request.0, body[start..].as_mut_ptr() as *mut _, available, &mut read).map_err(net)?;
            body.truncate(start + read as usize);
        }
        Ok(body)
    }
}

/// SHA-256 with Windows' CNG.
pub fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    status.is_ok().then_some(out)
}

/// Ask GitHub for the latest release: Some when it is newer than this program.
pub fn check() -> Result<Option<Release>, Error> {
    let body = get(API_LATEST, "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n", MAX_JSON)?;
    let text = String::from_utf8(body).map_err(|_| err(Failure::Answer, "release JSON is not UTF-8"))?;
    let release = parse_release(&text)?;
    Ok((release.version > current()).then_some(release))
}

/// The numeric product version in an executable's version resource.
fn file_version(exe: &Path) -> Option<Version> {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO};
    let path = wide(&exe.display().to_string());
    unsafe {
        let size = GetFileVersionInfoSizeW(pcwstr(&path), None);
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        GetFileVersionInfoW(pcwstr(&path), 0, size, block.as_mut_ptr() as *mut _).ok()?;
        let mut info: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let root = wide("\\");
        if !VerQueryValueW(block.as_ptr() as *const _, pcwstr(&root), &mut info, &mut len).as_bool() || info.is_null() || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
            return None;
        }
        let f = &*(info as *const VS_FIXEDFILEINFO);
        Some((f.dwProductVersionMS >> 16, f.dwProductVersionMS & 0xFFFF, f.dwProductVersionLS >> 16))
    }
}

fn system_tar() -> PathBuf {
    let mut buf = [0u16; 260];
    let n = unsafe { windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buf)) } as usize;
    let dir = if n > 0 && n < buf.len() { String::from_utf16_lossy(&buf[..n]) } else { r"C:\Windows\System32".to_string() };
    PathBuf::from(dir).join("tar.exe")
}

/// Where downloads are unpacked: `%TEMP%\WindowsStats-update-<version>-<pid>`.
const TEMP_PREFIX: &str = "WindowsStats-update-";

/// Download, verify and unpack `r`; returns the new WindowsStats.exe, ready for `--update`.
pub fn download(r: &Release) -> Result<PathBuf, Error> {
    let data = get(&r.archive_url, "Accept: application/octet-stream\r\n", r.size)?;
    if data.len() as u64 != r.size {
        return Err(err(Failure::Verify, format!("{} bytes downloaded, {} expected", data.len(), r.size)));
    }
    if sha256(&data) != Some(r.sha256) {
        return Err(err(Failure::Verify, "SHA-256 of the download differs from GitHub's digest"));
    }
    let dir = std::env::temp_dir().join(format!("{TEMP_PREFIX}{}-{}", r.name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let files = dir.join("files");
    std::fs::create_dir_all(&files).map_err(|e| err(Failure::Install, format!("{}: {e}", files.display())))?;
    let archive = dir.join(&r.archive_name);
    std::fs::write(&archive, &data).map_err(|e| err(Failure::Install, format!("{}: {e}", archive.display())))?;
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // bsdtar refuses absolute paths and ".." in an archive unless told otherwise
        let status = std::process::Command::new(system_tar())
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&files)
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| err(Failure::Install, format!("tar: {e}")))?;
        if !status.success() {
            return Err(err(Failure::Install, format!("tar exited with {status}")));
        }
    }
    let exe = files.join("WindowsStats.exe");
    match file_version(&exe) {
        Some(v) if v == r.version => Ok(exe),
        other => Err(err(Failure::Verify, format!("archive holds version {other:?}, expected {}", r.name))),
    }
}

fn ready_event_name(pid: u32) -> String {
    format!("Local\\WindowsStats-update-{pid}")
}

/// Start the unpacked release with `--update <our pid>` and wait until it reports that it took
/// over. Only then may this copy exit: a release that fails to start, crashes or does not know
/// `--update` leaves this copy running, so an update never leaves the user without the app.
pub fn hand_over(exe: &Path) -> Result<(), Error> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};
    let pid = std::process::id();
    let name = wide(&ready_event_name(pid));
    let event = unsafe { CreateEventW(None, true, false, pcwstr(&name)) }.map_err(|e| err(Failure::Install, format!("event: {}", e.message())))?;
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    let mut child = match std::process::Command::new(exe).arg("--update").arg(pid.to_string()).current_dir(dir).spawn() {
        Ok(c) => c,
        Err(e) => {
            unsafe {
                let _ = CloseHandle(event);
            }
            return Err(err(Failure::Install, format!("starting {}: {e}", exe.display())));
        }
    };
    let handles = [event, HANDLE(child.as_raw_handle())];
    let r = unsafe { WaitForMultipleObjects(&handles, false, 15_000) };
    unsafe {
        let _ = CloseHandle(event);
    }
    if r == WAIT_OBJECT_0 {
        return Ok(());
    }
    let _ = child.kill();
    Err(err(Failure::Install, format!("the new version did not take over (wait result {})", r.0)))
}

/// `--update <pid>`: tell the copy that started us that we take over from here.
pub fn signal_ready(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    let name = wide(&ready_event_name(pid));
    unsafe {
        if let Ok(h) = OpenEventW(EVENT_MODIFY_STATE, false, pcwstr(&name)) {
            let _ = SetEvent(h);
            let _ = CloseHandle(h);
        }
    }
}

/// Remove what earlier updates left in %TEMP% (the unpacked copy cannot delete itself), but
/// not a download of this process.
pub fn clean_temp() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    let own = format!("-{}", std::process::id());
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(TEMP_PREFIX) && !name.ends_with(&own) && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Only the per-user installation updates itself; a copy run from elsewhere (an unpacked
/// archive, a build folder) points at the release page instead.
pub fn can_install() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let Some(dir) = exe.parent() else { return false };
    dir.to_string_lossy().to_lowercase() == crate::install::install_dir().to_string_lossy().to_lowercase()
}

/// What the settings show and what the app is doing about updates.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    Failed(Failure),
}

impl State {
    pub fn busy(&self) -> bool {
        matches!(self, State::Checking | State::Downloading(_))
    }
}

/// A finished background job, picked up on the UI thread after WM_APP_UPDATE.
pub enum Outcome {
    Checked { automatic: bool, result: Result<Option<Release>, Error> },
    /// Ok: the new version is running and waits for this process to exit
    Downloaded(Result<(), Error>),
}

/// finished jobs in order (a manual check can end while the daily one is still running)
static OUTCOMES: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());

pub fn take_outcomes() -> Vec<Outcome> {
    OUTCOMES.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
}

fn finish(hwnd: isize, outcome: Outcome) {
    if let Ok(mut g) = OUTCOMES.lock() {
        g.push(outcome);
    }
    unsafe {
        let _ = PostMessageW(HWND(hwnd as *mut _), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
    }
}

/// Check on a thread; `notify` gets WM_APP_UPDATE when done.
pub fn start_check(notify: HWND, automatic: bool) {
    let hwnd = notify.0 as isize;
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        finish(hwnd, Outcome::Checked { automatic, result: check() });
    });
}

/// Download, unpack and start the new version on a thread; `notify` gets WM_APP_UPDATE when
/// the new version has taken over (time for this copy to exit) or something failed.
pub fn start_download(notify: HWND, release: Release) {
    let hwnd = notify.0 as isize;
    let _ = std::thread::Builder::new().name("update-download".into()).spawn(move || {
        let prepared = match unpacked_for_test() {
            Some(exe) => Ok(exe),
            None => download(&release),
        };
        let result = prepared.and_then(|exe| {
            crate::log!("update: starting {}", exe.display());
            hand_over(&exe)
        });
        finish(hwnd, Outcome::Downloaded(result));
    });
}

/// Debug builds only: `WINDOWS_STATS_UPDATE_EXE=<unpacked WindowsStats.exe>` replaces the
/// download, to exercise the hand-over and the installation with a local build.
#[cfg(debug_assertions)]
fn unpacked_for_test() -> Option<PathBuf> {
    std::env::var_os("WINDOWS_STATS_UPDATE_EXE").map(PathBuf::from)
}

#[cfg(not(debug_assertions))]
fn unpacked_for_test() -> Option<PathBuf> {
    None
}

/// Seconds since 1970, for the daily check.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The automatic check is on and the last one is at least a day old (or in the future: the
/// clock was changed).
pub fn due(enabled: Option<bool>, last: u64, now: u64) -> bool {
    enabled == Some(true) && (last > now || now - last >= CHECK_INTERVAL_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_json(tag: &str, name: &str, url: &str, digest: &str, draft: bool, size: u64) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/{REPO}/releases/tag/{tag}","draft":{draft},"prerelease":false,
               "assets":[{{"name":"other.txt","browser_download_url":"https://example.com/x","size":1}},
                         {{"name":"{name}","browser_download_url":"{url}","size":{size},"digest":"{digest}"}}]}}"#
        )
    }

    const DIGEST: &str = "sha256:63c0383bca3dd4eff0cbb6bfeeb8c7d8c0964251a9730c62bf8005d8e47e8f6d";

    fn good() -> String {
        release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", "https://github.com/husodrn46/windows-stats/releases/download/v0.1.3/WindowsStats-0.1.3-x64.zip", DIGEST, false, 2204400)
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.1.3"), Some((0, 1, 3)));
        assert_eq!(parse_version("10.20.30"), Some((10, 20, 30)));
        for bad in ["", "v", "0.1", "0.1.3.4", "0.1.3-beta", "0.1.x", "v0..3", " 0.1.3", "0.1.3 ", "+1.2.3", "1234567890.0.0"] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
        assert!(parse_version("0.1.10") > parse_version("0.1.9"));
        assert!(parse_version("1.0.0") > parse_version("0.99.99"));
    }

    #[test]
    fn reads_a_release() {
        let r = parse_release(&good()).unwrap();
        assert_eq!(r.version, (0, 1, 3));
        assert_eq!(r.name, "0.1.3");
        assert_eq!(r.archive_name, "WindowsStats-0.1.3-x64.zip");
        assert_eq!(r.size, 2204400);
        assert_eq!(r.sha256[0], 0x63);
        assert_eq!(r.sha256[31], 0x6d);
        assert_eq!(r.page, "https://github.com/husodrn46/windows-stats/releases/tag/v0.1.3");
    }

    #[test]
    fn rejects_releases_it_cannot_trust() {
        let url = "https://github.com/husodrn46/windows-stats/releases/download/v0.1.3/WindowsStats-0.1.3-x64.zip";
        let cases = [
            // draft
            release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", url, DIGEST, true, 10),
            // archive of another version
            release_json("v0.1.3", "WindowsStats-0.1.2-x64.zip", url, DIGEST, false, 10),
            // served from somewhere else
            release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", "https://example.com/WindowsStats-0.1.3-x64.zip", DIGEST, false, 10),
            release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", "http://github.com/husodrn46/windows-stats/releases/download/v0.1.3/WindowsStats-0.1.3-x64.zip", DIGEST, false, 10),
            // no usable digest
            release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", url, "", false, 10),
            release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", url, "sha1:0123456789abcdef0123456789abcdef01234567", false, 10),
            release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", url, "sha256:1234", false, 10),
            // empty or oversized
            release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", url, DIGEST, false, 0),
            release_json("v0.1.3", "WindowsStats-0.1.3-x64.zip", url, DIGEST, false, MAX_ARCHIVE + 1),
            // not a plain version tag
            release_json("v0.1.3-rc1", "WindowsStats-0.1.3-x64.zip", url, DIGEST, false, 10),
            release_json("0.1.3", "WindowsStats-0.1.3-x64.zip", url, DIGEST, false, 10),
        ];
        for json in cases {
            let e = parse_release(&json).unwrap_err();
            assert_eq!(e.kind, Failure::Answer, "{json}");
        }
        let other_page = good().replace("https://github.com/husodrn46/windows-stats/releases/tag", "https://github.com/someone/else/releases/tag");
        assert!(parse_release(&other_page).is_err());
        assert!(parse_release(&good().replace("\"prerelease\":false", "\"prerelease\":true")).is_err());
        assert!(parse_release("{}").is_err());
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn only_https_addresses() {
        assert_eq!(split_https("https://api.github.com/repos/x"), Some(("api.github.com", "/repos/x")));
        assert_eq!(split_https("http://api.github.com/repos/x"), None);
        assert_eq!(split_https("https://api.github.com"), None);
        assert_eq!(split_https("https://user@evil.example/x"), None);
        assert_eq!(split_https("https://evil.example:8443/x"), None);
    }

    #[test]
    fn sha256_matches_the_standard() {
        let abc = sha256(b"abc").unwrap();
        assert_eq!(abc, parse_hex32("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").unwrap());
    }

    #[test]
    fn daily_check_timing() {
        let day = CHECK_INTERVAL_SECS;
        assert!(!due(None, 0, 10 * day), "not asked yet");
        assert!(!due(Some(false), 0, 10 * day));
        assert!(due(Some(true), 0, 10 * day));
        assert!(!due(Some(true), 10 * day, 10 * day + day - 1));
        assert!(due(Some(true), 10 * day, 11 * day));
        assert!(due(Some(true), 12 * day, 11 * day), "clock moved back");
    }
}
