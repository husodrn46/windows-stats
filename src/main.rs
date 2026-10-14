#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Windows Stats: system resource widgets in the Windows taskbar, in the style of macOS Stats.
//! Based on winstats by Oleksandr Zhabotynskyi (MIT).
//!
//! Threads: the UI thread owns every window (the strip inside the taskbar, the popups, the
//! menu) and only draws; the collector thread (`collector.rs`) does all measuring; the ping
//! thread probes latency only when the user configured a host and the network popup is open.

mod canvas;
mod cli;
mod collector;
mod config;
mod dwtext;
mod i18n;
mod menu;
mod metrics;
mod about;
mod alerts;
mod backdrop;
mod popup;
mod sensorhost;
mod settings;
mod procicon;
mod taskbar;
mod taskbar_uia;
mod toast;
mod util;
mod widgets;

use about::About;
use canvas::{Canvas, Color, WHITE};
use collector::{Cmd, Collector};
use config::Config;
use i18n::t;
use metrics::MetricsData;
use popup::Popup;
use std::cell::RefCell;
use taskbar::Taskbar;
use util::{pcwstr, wide};
use widgets::{ColorMode, Kind, Theme, Widget};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromWindow, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

/// lives in the UI.Controls feature of the windows crate
const WM_MOUSELEAVE: u32 = 0x02A3;
/// WM_POWERBROADCAST events
const PBT_APMSUSPEND: usize = 0x0004;
const PBT_APMRESUMESUSPEND: usize = 0x0007;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;

/// floating mode: re-assert the strip above the taskbar (Explorer raises the taskbar over other
/// topmost windows whenever it is activated, e.g. when the Start menu opens)
const TIMER_ZORDER: usize = 2;
/// floating mode: short burst of z-order checks (every 16 ms) right after a shell event, because
/// Explorer raises the taskbar a few milliseconds *after* the events it emits
const TIMER_ZBURST: usize = 3;
const ZBURST_TICKS: u32 = 14;
/// no sample arrived for a while (collector stuck or suspended): the strip still gets
/// re-attached after an Explorer restart
const TIMER_WATCH: usize = 5;
const WATCH_MS: u32 = 5000;
/// auto mode, floating because the taskbar refused the strip: how often embedding is retried
const EMBED_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

const MENU_EXIT: usize = 1;
const MENU_CONFIG: usize = 2;
const MENU_RELOAD: usize = 3;
const MENU_TASKMGR: usize = 4;
const MENU_ABOUT: usize = 5;
const MENU_SETTINGS: usize = 8;
/// posted by `WindowsStats.exe --settings` to a running instance
pub const WM_APP_OPEN_SETTINGS: u32 = WM_APP + 5;
/// `--settings` when no instance was running: open the window once started
pub static OPEN_SETTINGS_AT_START: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Class names of our windows (also checked by the full-screen detection).
const CLASS_HIDDEN: &str = "WindowsStatsHidden";
const CLASS_BAR: &str = "WindowsStatsBar";
const CLASS_POPUP: &str = "WindowsStatsPopup";
const CLASS_SHADOW: &str = "WindowsStatsShadow";
const CLASS_ABOUT: &str = "WindowsStatsAbout";
const CLASS_TOAST: &str = "WindowsStatsToast";
const CLASS_SETTINGS: &str = "WindowsStatsSettings";

struct App {
    cfg: Config,
    hidden: HWND,
    bar: HWND,
    taskbar: Option<Taskbar>,
    collector: Option<Collector>,
    /// the newest snapshot from the collector
    metrics: MetricsData,
    canvas: Canvas,
    widgets: Vec<Box<dyn Widget>>,
    /// placed widgets: (index, kind, main-axis start, main-axis end, cross-axis start,
    /// cross-axis end) in bar coords; the main axis runs along the strip (y on a vertical taskbar)
    layout: Vec<(usize, Kind, i32, i32, i32, i32)>,
    /// mouse-drag reordering state
    drag: Option<Drag>,
    /// floating mode: the strip is a top-level window over the taskbar instead of a child
    floating: bool,
    /// floating mode: remaining burst checks
    zburst: u32,
    popup: Popup,
    about: About,
    /// ICMP probe for the latency rows of the network popup (only with a configured host)
    ping: metrics::ping::Ping,
    /// what the collector was last told about the popup (detail collection on / off)
    details: bool,
    /// last logged layout (widget kinds and widths), to log only changes
    layout_sig: String,
    /// floating mode: the strip is hidden while a full-screen game or presentation runs
    fullscreen_hidden: bool,
    wm_taskbar_created: u32,
    /// RegisterShellHookWindow notifications (windows created / destroyed: task buttons change)
    wm_shellhook: u32,
    ticks: u64,
    /// when the last sample arrived, for the watchdog timer
    last_data: std::time::Instant,
    /// Windows 11: where Explorer's own taskbar items are (UI Automation, its own thread)
    items: Option<taskbar_uia::Watcher>,
    /// the taskbar window the watcher reads
    items_tray: HWND,
    /// last logged fit (shown widgets / span), to log only changes
    fit_sig: String,
    /// floating mode: WinEvent hooks on the taskbar's thread (unhooked when embedding again)
    hooks: Vec<windows::Win32::UI::Accessibility::HWINEVENTHOOK>,
    /// floating because Explorer refused the strip as a child window (mode "auto"); embedding is
    /// tried again every EMBED_RETRY
    auto_floating: bool,
    /// consecutive SetParent refusals
    attach_failures: u32,
    embed_probe_at: std::time::Instant,
    /// threshold notifications: rule timers and the card that shows them
    alerts: alerts::Alerts,
    toast: toast::Toast,
    settings: settings::Settings,
}

/// A full-screen Direct3D game or a presentation is in front: the shell says so, or the
/// foreground window (not ours, not the desktop) covers its whole monitor, taskbar included.
fn fullscreen_active(own: HWND) -> bool {
    unsafe {
        if let Ok(s) = SHQueryUserNotificationState() {
            if s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE {
                return true;
            }
        }
        let fg = GetForegroundWindow();
        if fg.0.is_null() || fg == own {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls) as usize;
        let cls = String::from_utf16_lossy(&cls[..n]);
        if matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") || cls == CLASS_POPUP || cls == CLASS_ABOUT {
            return false;
        }
        let mut rc = RECT::default();
        if GetWindowRect(fg, &mut rc).is_err() {
            return false;
        }
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let hm = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(hm, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        rc.left <= m.left && rc.top <= m.top && rc.right >= m.right && rc.bottom >= m.bottom
    }
}

/// A widget being dragged along the strip.
struct Drag {
    /// index into `widgets` / `cfg.widgets`
    widget: usize,
    /// where the button went down and where the pointer is, bar-client coords
    start_x: i32,
    start_y: i32,
    cur_x: i32,
    cur_y: i32,
    /// true once the pointer moved far enough to count as a drag rather than a click
    active: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if cli::run(&args) {
        return;
    }
    util::rotate_log();
    let first_run = !Config::path().exists();
    let mut cfg = Config::load();
    widgets::normalize(&mut cfg.widgets);
    i18n::set_language(&cfg.language);
    let floating = cfg.mode == "floating";
    unsafe {
        // per-monitor DPI where Windows offers it (10 1607+), system DPI awareness otherwise
        if !util::set_per_monitor_dpi() {
            let _ = SetProcessDPIAware();
        }
        // single instance; a separate settings home (WINDOWS_STATS_HOME, tests) is its own instance
        let instance = match util::instance_tag() {
            Some(tag) => format!("Local\\WindowsStats-single-instance-{tag}"),
            None => "Local\\WindowsStats-single-instance".to_string(),
        };
        let name = wide(&instance);
        let _mutex = CreateMutexW(None, false, pcwstr(&name));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log!("already running");
            return;
        }
    }

    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None).expect("module handle").into() };

    unsafe {
        register_class(hinstance, CLASS_HIDDEN, Some(hidden_wndproc));
        register_class(hinstance, CLASS_BAR, Some(bar_wndproc));
        register_class(hinstance, CLASS_POPUP, Some(popup_wndproc));
        register_class(hinstance, CLASS_SHADOW, Some(shadow_wndproc));
        register_class(hinstance, CLASS_ABOUT, Some(about_wndproc));
        register_class(hinstance, CLASS_TOAST, Some(toast_wndproc));
        register_class(hinstance, CLASS_SETTINGS, Some(settings_wndproc));
    }

    // a top-level (not message-only) window: it has to receive the TaskbarCreated broadcast
    let hidden = unsafe {
        let cls = wide(CLASS_HIDDEN);
        let title = wide(&util::hidden_title());
        CreateWindowExW(WS_EX_TOOLWINDOW, pcwstr(&cls), pcwstr(&title), WS_OVERLAPPED, 0, 0, 0, 0, None, None, hinstance, None).expect("hidden window")
    };
    metrics::net::watch(hidden);

    let wm_taskbar_created = unsafe {
        let s = wide("TaskbarCreated");
        let m = RegisterWindowMessageW(pcwstr(&s));
        // let the broadcast through even if we run elevated
        let _ = ChangeWindowMessageFilterEx(hidden, m, MSGFLT_ALLOW, None);
        m
    };

    // the shell tells this window when top-level windows come and go: the moments the task list
    // changes width, so the strip is re-fitted without polling Explorer
    let wm_shellhook = unsafe {
        let s = wide("SHELLHOOK");
        let m = RegisterWindowMessageW(pcwstr(&s));
        if !RegisterShellHookWindow(hidden).as_bool() {
            log!("RegisterShellHookWindow failed");
        }
        m
    };

    let (collector, first) = Collector::start(hidden, collector::Settings::from_config(&cfg));
    let metrics = first.unwrap_or_default();
    if first_run {
        // do not start with widgets this machine cannot feed
        let before = cfg.widgets.len();
        if !metrics.gpu_available {
            cfg.widgets.retain(|w| widgets::CATALOG.iter().all(|(k, n, _)| *k != Kind::Gpu || *n != w));
        }
        if !metrics.battery_available {
            cfg.widgets.retain(|w| widgets::CATALOG.iter().all(|(k, n, _)| *k != Kind::Battery || *n != w));
        }
        if cfg.widgets.len() != before {
            cfg.save();
        }
    }
    let widgets = widgets::build(&cfg.widgets);
    let popup = Popup::new(hinstance, CLASS_POPUP, backdrop::Opacity::from_config(&cfg.popup_opacity)).expect("popup window");
    let about = About::new(hinstance, CLASS_ABOUT).expect("about window");
    let toast = toast::Toast::new(hinstance, CLASS_TOAST).expect("toast window");
    let settings = settings::Settings::new(hinstance, CLASS_SETTINGS).expect("settings window");
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            cfg: cfg.clone(),
            hidden,
            bar: HWND::default(),
            taskbar: None,
            collector: Some(collector),
            metrics,
            canvas: Canvas::new(),
            widgets,
            layout: Vec::new(),
            drag: None,
            floating,
            zburst: 0,
            popup,
            about,
            ping: metrics::ping::Ping::new(&cfg.ping_host),
            details: false,
            layout_sig: String::new(),
            fullscreen_hidden: false,
            wm_taskbar_created,
            wm_shellhook,
            ticks: 0,
            last_data: std::time::Instant::now(),
            items: None,
            items_tray: HWND::default(),
            fit_sig: String::new(),
            hooks: Vec::new(),
            auto_floating: false,
            attach_failures: 0,
            embed_probe_at: std::time::Instant::now(),
            alerts: alerts::Alerts::default(),
            toast,
            settings,
        })
    });

    log!(
        "Windows Stats {} started (Windows build {}, win11={}, floating={}, language={})",
        about::VERSION,
        util::windows_build(),
        util::is_windows11(),
        floating,
        i18n::current()
    );
    with_app(|app| app.tick());
    if OPEN_SETTINGS_AT_START.load(std::sync::atomic::Ordering::Relaxed) {
        with_app(|app| app.open_settings());
    }
    unsafe {
        SetTimer(hidden, TIMER_WATCH, WATCH_MS, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().take() {
            app.shutdown();
        }
    });
    log!("Windows Stats stopped");
}

unsafe fn register_class(hinstance: HINSTANCE, name: &str, proc: WNDPROC) {
    let cls = wide(name);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: proc,
        hInstance: hinstance,
        lpszClassName: pcwstr(&cls),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        ..Default::default()
    };
    // keep the class name alive for the process lifetime
    std::mem::forget(cls);
    unsafe { RegisterClassExW(&wc) };
}

impl App {
    fn send(&self, cmd: Cmd) {
        if let Some(c) = &self.collector {
            c.send(cmd);
        }
    }

    /// The collector follows the settings that change what it measures.
    fn reconfigure(&self) {
        self.send(Cmd::Configure(collector::Settings::from_config(&self.cfg)));
    }

    fn ensure_bar(&mut self) -> bool {
        if self.taskbar.as_ref().map(|t| t.alive()).unwrap_or(false) == false {
            self.taskbar = taskbar::find(self.floating);
            match &self.taskbar {
                Some(t) => log!("taskbar found: tray={:?} notify={:?} rebar={:?} client={:?} dpi={}", t.tray, t.notify, t.rebar, t.client(), t.dpi()),
                None => {
                    return false;
                }
            }
            if self.floating {
                // a new taskbar (Explorer restarted) runs on a new thread: hook that one
                self.watch_floating(true);
            }
        }
        // floating only because Explorer refused the strip: try embedding again now and then
        if self.auto_floating && self.embed_probe_at.elapsed() >= EMBED_RETRY {
            self.embed_probe_at = std::time::Instant::now();
            let tray = self.taskbar.as_ref().map(|t| t.tray).unwrap_or_default();
            if taskbar::can_host_children(tray) {
                self.go_embedded();
                return self.ensure_bar();
            }
        }
        // Windows 11 draws its task list in XAML: watch its extent so the strip never covers it
        // (embedded or floating over it)
        let tray = self.taskbar.as_ref().map(|t| t.tray).unwrap_or_default();
        if util::is_windows11() && (self.items.is_none() || self.items_tray != tray) {
            self.items = Some(taskbar_uia::Watcher::start(self.hidden, tray));
            self.items_tray = tray;
        }
        let alive = unsafe { IsWindow(self.bar).as_bool() };
        // floating: the strip is a top-level window over the taskbar, there is no parent to check
        let parent_ok = alive && (self.floating || unsafe { GetParent(self.bar).ok() } == self.taskbar.as_ref().map(|t| t.tray));
        if !alive || !parent_ok {
            if alive {
                unsafe {
                    let _ = DestroyWindow(self.bar);
                }
            }
            self.canvas = Canvas::new();
            let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None).unwrap().into() };
            let cls = wide(CLASS_BAR);
            let title = wide("Windows Stats bar");
            let bar = unsafe {
                CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOPARENTNOTIFY,
                    pcwstr(&cls),
                    pcwstr(&title),
                    WS_POPUP,
                    0,
                    0,
                    10,
                    10,
                    None,
                    None,
                    hinstance,
                    None,
                )
            };
            let Ok(bar) = bar else {
                log!("CreateWindowExW(bar) failed: {:?}", bar.err());
                return false;
            };
            self.bar = bar;
            let tb = self.taskbar.as_ref().unwrap();
            if !tb.attach(bar) {
                unsafe {
                    let _ = DestroyWindow(bar);
                }
                self.bar = HWND::default();
                self.attach_failures += 1;
                if self.attach_failures >= 2 && self.cfg.mode != "embedded" {
                    // newer Windows 11 taskbars do not take child windows of other processes:
                    // the strip floats over the taskbar instead, in the same place
                    self.auto_floating = true;
                    self.go_floating("the taskbar refused the strip as a child window");
                    return self.ensure_bar();
                }
                return false;
            }
            self.attach_failures = 0;
            log!("bar window {:?} {}", bar, if self.floating { "floating over the taskbar" } else { "attached to taskbar" });
        }
        true
    }

    /// From now on the strip is an always-on-top window over the taskbar, in the place it would
    /// have inside it, kept above the taskbar by shell events and a timer.
    fn go_floating(&mut self, why: &str) {
        log!("strip: floating ({why})");
        unsafe {
            if IsWindow(self.bar).as_bool() {
                let _ = DestroyWindow(self.bar);
            }
        }
        self.bar = HWND::default();
        if let Some(tb) = &self.taskbar {
            tb.restore();
        }
        self.floating = true;
        self.taskbar = taskbar::find(true);
        self.watch_floating(true);
        self.embed_probe_at = std::time::Instant::now();
    }

    /// The taskbar takes child windows again: back inside it.
    fn go_embedded(&mut self) {
        log!("strip: the taskbar hosts the strip again, embedding");
        unsafe {
            if IsWindow(self.bar).as_bool() {
                let _ = DestroyWindow(self.bar);
            }
        }
        self.bar = HWND::default();
        self.floating = false;
        self.auto_floating = false;
        self.attach_failures = 0;
        self.watch_floating(false);
        self.taskbar = taskbar::find(false);
    }

    /// Floating mode: hooks and the z-order timer on (`on`) or off. The hooks only listen to the
    /// taskbar's own thread, which is what raises the taskbar over the strip (activation, Start),
    /// so the rest of the system's window traffic never wakes us up.
    fn watch_floating(&mut self, on: bool) {
        use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent};
        unsafe {
            for h in self.hooks.drain(..) {
                let _ = UnhookWinEvent(h);
            }
            let _ = KillTimer(self.hidden, TIMER_ZORDER);
            if !on {
                return;
            }
            SetTimer(self.hidden, TIMER_ZORDER, 250, None);
            let Some(tray) = self.taskbar.as_ref().map(|t| t.tray) else { return };
            let mut pid = 0u32;
            let tid = GetWindowThreadProcessId(tray, Some(&mut pid));
            for (a, b) in [(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_CAPTUREEND), (EVENT_OBJECT_SHOW, EVENT_OBJECT_REORDER)] {
                let h = SetWinEventHook(a, b, None, Some(shell_event_proc), pid, tid, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
                if !h.is_invalid() {
                    self.hooks.push(h);
                }
            }
        }
    }

    fn is_light(&self) -> bool {
        match self.cfg.theme.as_str() {
            "light" => true,
            "dark" => false,
            _ => util::system_light_theme(),
        }
    }

    fn theme(&self) -> Theme {
        let light = self.is_light();
        let (scale, h, vertical) = match self.taskbar.as_ref() {
            Some(tb) => {
                let scale = tb.dpi() as f32 / 96.0;
                // widget row: the taskbar's thickness; a vertical taskbar is wide (it shows
                // labels), so there the row is 48 DIP like an ordinary taskbar
                let h = if tb.vertical() { tb.thickness().min((48.0 * scale).round() as i32) } else { tb.thickness() };
                (scale, h, tb.vertical())
            }
            None => {
                let scale = util::dpi_for_window(self.hidden).unwrap_or(96) as f32 / 96.0;
                (scale, (48.0 * scale).round() as i32, false)
            }
        };
        let _ = vertical;
        let (text, dim) = if light {
            (Color::rgb(0x1B, 0x1B, 0x1B), Color::rgba(0x1B, 0x1B, 0x1B, 160))
        } else {
            (WHITE, Color::rgba(255, 255, 255, 150))
        };
        let mut modes = [ColorMode::Accent; 7];
        for (i, k) in widgets::MODULES.iter().enumerate() {
            modes[i] = match self.cfg.colors.get(module_key(*k)) {
                Some(v) => ColorMode::parse(v),
                None if self.cfg.utilization_colors && *k == Kind::Cpu => ColorMode::Utilization,
                None => ColorMode::Accent,
            };
        }
        Theme {
            text,
            dim,
            accent: self.cfg.accent_color(),
            light,
            scale,
            inner_h: ((h as f32) * 0.55).round() as i32,
            labels: self.cfg.labels,
            font_scale: if self.cfg.font_scale > 0.5 { self.cfg.font_scale } else { 1.0 },
            split_pe: self.cfg.split_pe_cores,
            chart_w: self.cfg.chart_width.clamp(20, 200) as f32,
            bar_w: widgets::bar_width_dip(&self.cfg.bar_width),
            net_rx: self.cfg.net_chart_rx,
            net_tx: self.cfg.net_chart_tx,
            modes,
        }
    }

    fn render_popup(&mut self, first: bool) {
        let light = self.is_light();
        let theme = self.theme();
        self.popup.render(&theme, light, &self.metrics, first);
    }

    /// A new snapshot from the collector.
    fn on_data(&mut self) {
        let Some(c) = &self.collector else { return };
        if let Some(d) = c.take() {
            self.metrics = d;
            self.last_data = std::time::Instant::now();
            self.check_alerts();
        }
        self.tick();
    }

    /// Advance the threshold rules by one sample; a rule that fires shows the card.
    fn check_alerts(&mut self) {
        let dt = self.cfg.update_ms as f32 / 1000.0;
        let fired = self.alerts.evaluate(&self.cfg.alerts, &self.metrics, dt);
        if let Some(f) = fired.last() {
            let metric = t(f.rule.metric.key());
            let (value, limit) = (f.rule.metric.format(f.value), f.rule.metric.format(f.rule.threshold));
            let secs = f.rule.seconds.to_string();
            let key = if f.rule.above { "alerts.fired_above" } else { "alerts.fired_below" };
            let body = i18n::tf(key, &[("metric", &metric), ("limit", &limit), ("value", &value), ("seconds", &secs)]);
            log!("alert: {body}");
            let theme = self.theme();
            let light = self.is_light();
            self.toast.show(&theme, light, &t("alerts.title"), &body);
        }
    }

    /// Latency rows: the ping thread's last readings go into the snapshot the popup draws.
    fn apply_ping(&mut self) {
        let (lat, jit, target) = self.ping.read();
        // the section title names what is actually measured (a fallback may be in use)
        self.metrics.net_ping_host = if self.cfg.ping_host.is_empty() || target.is_empty() { self.cfg.ping_host.clone() } else { target };
        self.metrics.net_latency = lat;
        self.metrics.net_jitter = jit;
    }

    fn tick(&mut self) {
        self.ticks += 1;
        if !self.ensure_bar() {
            return;
        }
        // floating strip over a full-screen Direct3D game would flicker through it: hide it
        // until the game is gone (the child strip on Windows 10/11 is covered anyway)
        if self.floating {
            let fs = fullscreen_active(self.bar);
            if fs != self.fullscreen_hidden {
                self.fullscreen_hidden = fs;
                unsafe {
                    let _ = ShowWindow(self.bar, if fs { SW_HIDE } else { SW_SHOWNA });
                }
                if fs {
                    self.popup.hide("fullscreen");
                    log!("full-screen application in front: strip hidden");
                } else {
                    log!("full-screen application gone: strip back");
                    self.raise_bar();
                }
            }
        }
        // the collector reads the per-process lists and other details only while a popup is open
        let want = self.popup.visible;
        if want != self.details {
            self.details = want;
            self.send(Cmd::Details(want));
        }
        // latency probe only while the network popup is open, and only with a configured host
        let net_open = self.popup.visible && self.popup.kind == Some(Kind::Net);
        self.ping.set(net_open, &self.cfg.ping_host);
        self.apply_ping();
        if !self.fullscreen_hidden {
            self.render();
        }
    }

    /// Layout, draw and present the strip (and the popup) from the current metrics.
    fn render(&mut self) {
        if self.taskbar.is_none() {
            return;
        }
        let theme = self.theme();
        let tb = self.taskbar.as_ref().unwrap();
        // on a vertical taskbar the widgets are stacked top to bottom, each in a row of `h`
        let vertical = tb.vertical();
        let thick = tb.thickness();
        let h = if vertical { thick.min(theme.s(48.0)) } else { thick };
        let gap = theme.s(6.0);
        let edge = theme.s(6.0);
        // natural widths; a chart marked as stretched (vertical taskbar) grows later to fill
        // what is left of its row
        let mut widths: Vec<i32> = Vec::with_capacity(self.widgets.len());
        for w in self.widgets.iter() {
            widths.push(w.width(&mut self.canvas, &theme, &self.metrics));
        }
        let mut visible: Vec<(usize, i32)> = widths.iter().cloned().enumerate().filter(|(_, w)| *w > 0).collect();
        // Windows 11 fit: (span index 0 = left / 1 = right, span) the strip goes into
        let mut fit_span: Option<(usize, (i32, i32))> = None;
        let stretched = |i: usize| vertical && self.cfg.widgets.get(i).map_or(false, |n| self.cfg.stretched.contains(n));
        // cells: (widget index, main-axis start, cross-axis start, width)
        let mut cells: Vec<(usize, i32, i32, i32)> = Vec::with_capacity(visible.len());
        let total;
        if vertical {
            // flow layout: a row takes the next widgets while they fit side by side, rows are
            // `h` tall; stretched charts share what is left of their row, other rows are centred
            let avail = thick - edge * 2;
            let (mut row_y, mut row_x, mut row_start) = (edge, edge, 0usize);
            let mut rows: Vec<(usize, usize)> = Vec::new();
            for (i, w) in &visible {
                let cw = (*w).min(avail);
                if row_x > edge && row_x + cw > edge + avail {
                    rows.push((row_start, cells.len()));
                    row_y += h + gap;
                    row_x = edge;
                    row_start = cells.len();
                }
                cells.push((*i, row_y, row_x, cw));
                row_x += cw + gap;
            }
            if row_x > edge {
                rows.push((row_start, cells.len()));
            }
            for (a, b) in rows {
                let used: i32 = cells[a..b].iter().map(|c| c.3).sum::<i32>() + gap * (b - a - 1) as i32;
                let n_stretch = cells[a..b].iter().filter(|c| stretched(c.0)).count() as i32;
                if n_stretch > 0 {
                    // give the spare width to the stretched charts and lay the row out again
                    let share = (avail - used) / n_stretch;
                    let mut x = edge;
                    for c in &mut cells[a..b] {
                        if stretched(c.0) {
                            c.3 += share;
                        }
                        c.2 = x;
                        x += c.3 + gap;
                    }
                } else {
                    let shift = (avail - used) / 2;
                    for c in &mut cells[a..b] {
                        c.2 += shift;
                    }
                }
            }
            total = if cells.is_empty() { edge * 2 } else { row_y + h + edge };
        } else {
            // Windows 11: only as many widgets as fit between the task list and the tray (or left
            // of the task list); the trailing ones wait until there is room again
            if let Some(occ) = self.items.as_ref().and_then(|w| w.get()) {
                let spans = taskbar::free_spans(tb.client(), tb.notify_left(), occ, tb.origin_x(), theme.s(4.0));
                let prefer = if self.cfg.position == "left" { 0 } else { 1 };
                let ws: Vec<i32> = visible.iter().map(|v| v.1).collect();
                let (n, span) = taskbar::fit(&ws, edge, gap, spans, prefer);
                let sig = format!("{n}/{} in {:?}", ws.len(), spans[span]);
                if sig != self.fit_sig {
                    log!("fit: {n} of {} widgets, span {:?} ({})", ws.len(), spans[span], if span == 0 { "left" } else { "right" });
                    self.fit_sig = sig;
                }
                visible.truncate(n);
                fit_span = Some((span, spans[span]));
            }
            let mut x = edge;
            for (i, w) in &visible {
                cells.push((*i, x, 0, *w));
                x += w + gap;
            }
            total = if cells.is_empty() { edge * 2 } else { x - gap + edge };
        }

        let (cw, ch) = if vertical { (thick.max(1), total.max(1)) } else { (total.max(1), h.max(1)) };
        self.canvas.resize(cw, ch);
        self.canvas.clear();
        // alpha 0 pixels are transparent to hit-testing on layered windows: keep the strip clickable
        self.canvas.fill_rect(0, 0, cw, ch, Color::rgba(0, 0, 0, 1));
        let y = (h - theme.inner_h) / 2;
        self.layout.clear();
        // one line per layout change so tests and bug reports can find the widgets
        let sig: Vec<String> = visible.iter().map(|(i, w)| format!("{:?}@{}", self.widgets[*i].kind(), w)).collect();
        let sig = sig.join(" ");
        if sig != self.layout_sig {
            self.layout_sig = sig.clone();
            log!("layout: {sig}");
        }
        for (i, main, cross, w) in &cells {
            let (wx, wy) = if vertical { (*cross, main + y) } else { (*main, y) };
            // a stretched chart is drawn with the chart width that fills its cell
            let mut fitted;
            let th: &Theme = if stretched(*i) {
                fitted = theme.clone();
                fitted.chart_w = ((w - widgets::label_width(&theme)) as f32 / theme.scale).max(10.0);
                &fitted
            } else {
                &theme
            };
            self.widgets[*i].draw(&mut self.canvas, wx, wy, th, &self.metrics);
            let cell = if vertical {
                (main - gap / 2, main + h + gap / 2, cross - gap / 2, cross + w + gap / 2)
            } else {
                (main - gap / 2, main + w + gap / 2, 0, h)
            };
            self.layout.push((*i, self.widgets[*i].kind(), cell.0, cell.1, cell.2, cell.3));
        }
        if let Some(d) = self.drag.as_ref().filter(|d| d.active) {
            // dim the dragged widget and mark the insertion slot nearest to the pointer
            if let Some((_, _, x0, x1, q0, q1)) = self.layout.iter().find(|(i, ..)| *i == d.widget) {
                let bg = if self.is_light() { Color::rgba(255, 255, 255, 140) } else { Color::rgba(0, 0, 0, 140) };
                if vertical {
                    self.canvas.fill_rect(*q0 + gap / 2, *x0 + gap / 2, q1 - q0 - gap, x1 - x0 - gap, bg);
                } else {
                    self.canvas.fill_rect(*x0 + gap / 2, y, x1 - x0 - gap, theme.inner_h, bg);
                }
            }
            let slot = self.drop_slot(d.cur_x, d.cur_y);
            if vertical {
                if let Some((mx, y0, y1)) = self.slot_marker_vertical(slot, gap) {
                    self.canvas.fill_rect(mx - 1, y0, theme.s(2.0), y1 - y0, theme.accent);
                }
            } else {
                let slot_x = self.slot_x(slot, total);
                self.canvas.fill_rect(slot_x - 1, y - theme.s(2.0), theme.s(2.0), theme.inner_h + theme.s(4.0), theme.accent);
            }
        }

        let offset = theme.s(self.cfg.offset as f32);
        // the span decides the side: the configured one, or the other when only that one has room
        let (position, span) = match fit_span {
            Some((0, s)) => ("left", Some(s)),
            Some((_, s)) => ("right", Some(s)),
            None => (self.cfg.position.as_str(), None),
        };
        let rc = tb.place(self.bar, total, position, offset, span);
        if self.ticks == 1 {
            log!("placed bar at {:?}", rc);
        }
        if let Err(e) = self.canvas.present(self.bar) {
            if self.ticks % 30 == 1 {
                log!("UpdateLayeredWindow failed: {e}");
            }
        }
        // WINDOWS_STATS_DEBUG_DUMP=<dir>: the strip as drawn, for checking the rendering
        if let Some(dir) = std::env::var_os("WINDOWS_STATS_DEBUG_DUMP") {
            let _ = self.canvas.save_bmp(&std::path::Path::new(&dir).join("strip.bmp"));
        }
        if self.popup.visible {
            self.render_popup(false);
        }
    }

    fn open_settings(&mut self) {
        self.popup.hide("settings");
        let light = self.is_light();
        self.settings.show(light);
    }

    /// A click in the settings window: change the config (save, apply everywhere) or act.
    fn apply_settings(&mut self, e: settings::Effect) {
        use settings::Effect;
        match e {
            Effect::Page(_) => {}
            Effect::Autostart(on) => {
                if !util::set_autostart(on) {
                    log!("autostart change failed");
                }
            }
            Effect::Open(target) => shell_open(&target),
            Effect::LanguageMenu => {
                if let Some(code) = language_menu(self.settings.hwnd, &self.cfg.language) {
                    self.cfg.language = code;
                    self.after_config_change();
                }
            }
            Effect::Cfg(f) => {
                let before = self.cfg.clone();
                f(&mut self.cfg);
                if self.cfg.alerts != before.alerts {
                    self.alerts.reset();
                }
                if self.cfg.mode != before.mode {
                    if self.cfg.mode == "floating" && !self.floating {
                        self.go_floating("chosen in the settings");
                    } else if self.cfg.mode != "floating" && self.floating {
                        self.go_embedded();
                    }
                }
                self.after_config_change();
            }
        }
        self.settings.invalidate();
    }

    /// Save the config and bring everything in line with it.
    fn after_config_change(&mut self) {
        self.cfg.sanitize();
        i18n::set_language(&self.cfg.language);
        self.popup.opacity = backdrop::Opacity::from_config(&self.cfg.popup_opacity);
        self.cfg.save();
        self.widgets = widgets::build(&self.cfg.widgets);
        self.reconfigure();
        if self.cfg.position == "left" {
            if let Some(tb) = &self.taskbar {
                tb.restore();
            }
        }
        self.tick();
    }

    /// Where the settings menu opens: flush against the taskbar on the side away from the
    /// screen edge, so it never covers the strip, whatever the DPI. The pointer only picks the
    /// position along the taskbar.
    fn menu_anchor(&self) -> (POINT, TRACK_POPUP_MENU_FLAGS) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let Some(tb) = self.taskbar.as_ref() else { return (pt, TPM_LEFTALIGN | TPM_BOTTOMALIGN) };
        let mut rc = RECT::default();
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe {
            let _ = GetWindowRect(tb.tray, &mut rc);
            let _ = GetMonitorInfoW(MonitorFromWindow(tb.tray, MONITOR_DEFAULTTONEAREST), &mut mi);
        }
        let mon = mi.rcMonitor;
        if tb.vertical() {
            if rc.left <= mon.left + 10 {
                (POINT { x: rc.right, y: pt.y }, TPM_LEFTALIGN | TPM_BOTTOMALIGN)
            } else {
                (POINT { x: rc.left, y: pt.y }, TPM_RIGHTALIGN | TPM_BOTTOMALIGN)
            }
        } else if rc.top > mon.top + 10 {
            (POINT { x: pt.x, y: rc.top }, TPM_LEFTALIGN | TPM_BOTTOMALIGN)
        } else {
            (POINT { x: pt.x, y: rc.bottom }, TPM_LEFTALIGN | TPM_TOPALIGN)
        }
    }

    /// Bar-client (x, y) as (position along the strip, position across it).
    fn axes(&self, x: i32, y: i32) -> (i32, i32) {
        if self.taskbar.as_ref().map_or(false, |t| t.vertical()) {
            (y, x)
        } else {
            (x, y)
        }
    }

    /// The placed widget under bar-client (x, y).
    fn widget_at(&self, x: i32, y: i32) -> Option<(usize, Kind, i32, i32, i32, i32)> {
        let (m, c) = self.axes(x, y);
        self.layout.iter().find(|(_, _, a, b, c0, c1)| m >= *a && m < *b && c >= *c0 && c < *c1).cloned()
    }

    /// Left click on the strip at bar-client (x, y).
    fn click(&mut self, x: i32, y: i32) {
        let Some((wi, kind, p0, p1, q0, q1)) = self.widget_at(x, y) else { return };
        let Some(tb) = self.taskbar.as_ref() else { return };
        let mut bar_rc = RECT::default();
        let mut tray_rc = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.bar, &mut bar_rc);
            let _ = GetWindowRect(tb.tray, &mut tray_rc);
        }
        let anchor = if tb.vertical() {
            RECT { left: bar_rc.left + q0, top: bar_rc.top + p0, right: bar_rc.left + q1, bottom: bar_rc.top + p1 }
        } else {
            RECT { left: bar_rc.left + p0, top: bar_rc.top, right: bar_rc.left + p1, bottom: bar_rc.bottom }
        };
        if self.popup.toggle(kind, wi, anchor, tray_rc) {
            // the collector starts the per-process lists now; the popup shows "collecting"
            // for the moment until that sample arrives
            self.details = true;
            self.send(Cmd::BeginDetails);
            self.render_popup(true);
        }
    }

    /// Insertion slot (0..=visible count) whose boundary is nearest to bar-client x.
    fn drop_slot(&self, x: i32, y: i32) -> usize {
        if self.taskbar.as_ref().map_or(false, |t| t.vertical()) {
            return self.drop_slot_vertical(x, y);
        }
        let mut best = 0usize;
        let mut best_d = i32::MAX;
        for (k, (_, _, x0, ..)) in self.layout.iter().enumerate() {
            let d = (x - x0).abs();
            if d < best_d {
                best_d = d;
                best = k;
            }
        }
        if let Some((_, _, _, x1, ..)) = self.layout.last() {
            if (x - x1).abs() < best_d {
                best = self.layout.len();
            }
        }
        best
    }

    /// Vertical taskbar: the row under the pointer first (nearest row centre), then the boundary
    /// in that row nearest to x, so a widget can also be moved left / right within its row.
    fn drop_slot_vertical(&self, x: i32, y: i32) -> usize {
        if self.layout.is_empty() {
            return 0;
        }
        let row_p0 = self.layout.iter().map(|c| (c.2, c.3)).min_by_key(|(p0, p1)| (y - (p0 + p1) / 2).abs()).map(|(p0, _)| p0).unwrap_or(0);
        let row: Vec<(usize, &(usize, Kind, i32, i32, i32, i32))> = self.layout.iter().enumerate().filter(|(_, c)| c.2 == row_p0).collect();
        let mut best = row[0].0;
        let mut best_d = i32::MAX;
        for (k, c) in &row {
            let d = (x - c.4).abs();
            if d < best_d {
                best_d = d;
                best = *k;
            }
        }
        if let Some((k, c)) = row.last() {
            if (x - c.5).abs() < best_d {
                best = k + 1;
            }
        }
        best
    }

    /// Vertical taskbar: the insertion marker for `slot` as (x, y0, y1) in bar coords.
    fn slot_marker_vertical(&self, slot: usize, gap: i32) -> Option<(i32, i32, i32)> {
        let (c, x) = match self.layout.get(slot) {
            Some(c) => (c, c.4),
            None => {
                let c = self.layout.last()?;
                (c, c.5)
            }
        };
        Some((x, c.2 + gap / 2, c.3 - gap / 2))
    }

    /// x of a slot boundary in bar-client coords.
    fn slot_x(&self, slot: usize, total: i32) -> i32 {
        match self.layout.get(slot) {
            // x0 already sits in the middle of the gap before the widget
            Some((_, _, x0, ..)) => (*x0).max(1),
            None => self.layout.last().map(|(_, _, _, x1, ..)| *x1).unwrap_or(total).min(total - 2),
        }
    }

    fn drag_start(&mut self, x: i32, y: i32) {
        let Some((i, ..)) = self.widget_at(x, y) else { return };
        self.drag = Some(Drag { widget: i, start_x: x, start_y: y, cur_x: x, cur_y: y, active: false });
        unsafe {
            SetCapture(self.bar);
        }
    }

    fn drag_move(&mut self, x: i32, y: i32) {
        let threshold = self.theme().s(6.0);
        let Some(d) = self.drag.as_mut() else { return };
        d.cur_x = x;
        d.cur_y = y;
        // either direction counts: on a vertical taskbar a widget may move within its row
        if !d.active && ((x - d.start_x).abs() >= threshold || (y - d.start_y).abs() >= threshold) {
            d.active = true;
            self.popup.hide("drag");
        }
        if d.active {
            self.render();
        }
    }

    /// Button released: finish a drag (reorder) or treat it as a click.
    fn drag_end(&mut self, x: i32, y: i32) {
        unsafe {
            let _ = ReleaseCapture();
        }
        let Some(d) = self.drag.take() else { return };
        if !d.active {
            self.click(x, y);
            return;
        }
        let slot = self.drop_slot(x, y);
        let from = d.widget;
        let to = match self.layout.get(slot) {
            Some((i, ..)) => *i,
            None => self.widgets.len(),
        };
        if to != from && to != from + 1 && from < self.cfg.widgets.len() {
            let name = self.cfg.widgets.remove(from);
            let insert_at = if to > from { to - 1 } else { to };
            self.cfg.widgets.insert(insert_at.min(self.cfg.widgets.len()), name);
            self.cfg.save();
            self.widgets = widgets::build(&self.cfg.widgets);
            log!("widgets reordered: {}", self.cfg.widgets.join(", "));
        }
        self.render();
    }

    /// floating mode: a shell event happened; check the z-order now and keep checking every 16 ms
    /// for a short while, since the taskbar is raised slightly after the event.
    fn shell_event(&mut self) {
        self.raise_bar();
        if self.zburst == 0 {
            unsafe {
                SetTimer(self.hidden, TIMER_ZBURST, 16, None);
            }
        }
        self.zburst = ZBURST_TICKS;
    }

    /// floating mode: put the strip back on top, but only when the taskbar actually covers it
    /// (an unconditional SetWindowPos would generate reorder events and feed the hook forever).
    fn raise_bar(&mut self) {
        if !self.floating || self.drag.is_some() || self.fullscreen_hidden {
            return;
        }
        let Some(tb) = self.taskbar.as_ref() else { return };
        unsafe {
            if IsWindow(self.bar).as_bool() && tb.covers(self.bar) {
                let _ = SetWindowPos(self.bar, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    fn reload(&mut self) {
        self.cfg = Config::load();
        widgets::normalize(&mut self.cfg.widgets);
        i18n::set_language(&self.cfg.language);
        self.widgets = widgets::build(&self.cfg.widgets);
        self.popup.opacity = backdrop::Opacity::from_config(&self.cfg.popup_opacity);
        self.reconfigure();
        log!("config reloaded");
        self.tick();
    }

    fn shutdown(mut self) {
        unsafe {
            let _ = DestroyWindow(self.popup.hwnd);
            let _ = DestroyWindow(self.about.hwnd);
            let _ = DestroyWindow(self.toast.hwnd);
            let _ = DestroyWindow(self.settings.hwnd);
            if IsWindow(self.bar).as_bool() {
                let _ = DestroyWindow(self.bar);
            }
        }
        if let Some(tb) = &self.taskbar {
            tb.restore();
        }
        if let Some(c) = self.collector.take() {
            c.stop();
        }
    }
}

unsafe extern "system" fn shell_event_proc(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // object events for child objects (buttons, list items, the caret...) are noise
    if event >= EVENT_OBJECT_SHOW && id_object != OBJID_WINDOW.0 {
        return;
    }
    with_app(|app| app.shell_event());
}

unsafe extern "system" fn hidden_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        collector::WM_APP_DATA => {
            with_app(|app| app.on_data());
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_ZORDER => {
                    with_app(|app| app.raise_bar());
                }
                TIMER_ZBURST => {
                    with_app(|app| {
                        app.raise_bar();
                        app.zburst = app.zburst.saturating_sub(1);
                        if app.zburst == 0 {
                            unsafe {
                                let _ = KillTimer(app.hidden, TIMER_ZBURST);
                            }
                        }
                    });
                }
                TIMER_WATCH => {
                    // samples drive the strip; without them (suspended, collector busy) the
                    // strip is still re-attached when Explorer came back
                    with_app(|app| {
                        if app.last_data.elapsed().as_millis() as u32 >= WATCH_MS {
                            app.tick();
                        }
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_OPEN_SETTINGS => {
            with_app(|app| app.open_settings());
            LRESULT(0)
        }
        taskbar_uia::WM_APP_TASKBAR_ITEMS => {
            with_app(|app| {
                if !app.fullscreen_hidden {
                    app.render();
                }
            });
            LRESULT(0)
        }
        metrics::net::WM_APP_NET => {
            with_app(|app| app.send(Cmd::RefreshNet));
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            match wparam.0 {
                PBT_APMSUSPEND => {
                    log!("system suspending");
                    with_app(|app| {
                        app.popup.hide("suspend");
                        app.send(Cmd::Suspend);
                    });
                }
                PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => {
                    log!("system resumed");
                    with_app(|app| {
                        app.send(Cmd::Resume);
                        app.alerts.reset();
                        app.tick();
                    });
                }
                _ => {}
            }
            LRESULT(1)
        }
        // light / dark switch, accent colour, taskbar size or a monitor change: redraw now
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED | WM_DISPLAYCHANGE => {
            with_app(|app| app.tick());
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                // the session ends without our message loop finishing: put the taskbar back now
                log!("session ending");
                with_app(|app| {
                    if let Some(tb) = &app.taskbar {
                        tb.restore();
                    }
                });
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            let (created, shellhook) = with_app(|app| (app.wm_taskbar_created, app.wm_shellhook)).unwrap_or((0, 0));
            if shellhook != 0 && msg == shellhook {
                // HSHELL_WINDOWCREATED, HSHELL_WINDOWDESTROYED, HSHELL_WINDOWREPLACED: the task list
                // gains or loses a button
                if matches!(wparam.0, 1 | 2 | 13) {
                    with_app(|app| {
                        if let Some(w) = &app.items {
                            w.poke();
                        }
                    });
                }
                return LRESULT(0);
            }
            if created != 0 && msg == created {
                log!("TaskbarCreated: explorer restarted, re-attaching");
                with_app(|app| {
                    app.taskbar = None;
                    app.bar = HWND::default();
                    app.tick();
                });
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

unsafe extern "system" fn bar_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_RBUTTONUP => {
            show_menu();
            LRESULT(0)
        }
        // Do not let Explorer activate the taskbar (it would eat the click while our popup is foreground).
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            with_app(|app| app.drag_start(x, y));
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if wparam.0 & 0x0001 != 0 {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                with_app(|app| app.drag_move(x, y));
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            with_app(|app| app.drag_end(x, y));
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            with_app(|app| {
                app.drag = None;
                app.render();
            });
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            open_taskmgr();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn toast_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == toast::TOAST_TIMER => {
            with_app(|app| app.toast.hide());
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            with_app(|app| app.toast.hide());
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn about_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF == 0 {
                with_app(|app| app.about.hide());
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == 0x1B {
                with_app(|app| app.about.hide());
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            with_app(|app| app.about.click(x, y));
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // hand over links, arrow elsewhere
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
            }
            let link = with_app(|app| app.about.is_link_at(pt.x, pt.y)).unwrap_or(false);
            unsafe {
                let _ = SetCursor(LoadCursorW(None, if link { IDC_HAND } else { IDC_ARROW }).unwrap_or_default());
            }
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The popup's shadow window: nothing to handle, it never gets input.
unsafe extern "system" fn shadow_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn popup_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            with_app(|app| {
                if let Some(action) = app.popup.click(x, y) {
                    app.popup_action(action);
                }
            });
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // hand over the rows that react to a click
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            let clickable = with_app(|app| app.popup.visible && app.popup.click_target(pt.x, pt.y)).unwrap_or(false);
            if clickable {
                if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                    SetCursor(c);
                }
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            // ask for WM_MOUSELEAVE so hover highlights go away when the mouse leaves
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            with_app(|app| {
                if app.popup.visible && app.popup.mouse_move(x, y) {
                    app.render_popup(false);
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|app| {
                if app.popup.visible && app.popup.mouse_leave() {
                    app.render_popup(false);
                }
            });
            LRESULT(0)
        }
        WM_PAINT => {
            // the DWM-backed popup keeps no layered bitmap: repaint the last frame
            if with_app(|app| app.popup.paint()).is_none() {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // the DWM-backed popup has a frame only to get the backdrop: no non-client area, no resizing
        WM_NCCALCSIZE if wparam.0 != 0 => LRESULT(0),
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_TIMER if wparam.0 == popup::COPY_TIMER => {
            // copy flash frames; the render ends the timer when the flash is over
            with_app(|app| {
                if app.popup.visible {
                    app.render_popup(false);
                }
            });
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF == 0 {
                let other = HWND(lparam.0 as *mut _);
                let mut cls = [0u16; 64];
                let n = unsafe { GetClassNameW(other, &mut cls) } as usize;
                let reason = format!("deactivated by {:?} '{}'", other, String::from_utf16_lossy(&cls[..n]));
                with_app(|app| app.popup.hide(&reason));
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == 0x1B {
                with_app(|app| app.popup.hide("escape"));
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Open a file, folder or URL with its default program.
fn shell_open(target: &str) {
    let verb = wide("open");
    let t = wide(target);
    unsafe {
        ShellExecuteW(None, pcwstr(&verb), pcwstr(&t), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// The language list at the pointer; the chosen code ("auto" for the Windows language).
fn language_menu(owner: HWND, current: &str) -> Option<String> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        menu_item(menu, 1, &t("menu.lang_system"), current == "auto");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        for (i, (code, name, _)) in i18n::LOCALES.iter().enumerate() {
            menu_item(menu, 2 + i, name, current == *code);
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, 0, owner, None).0 as usize;
        let _ = DestroyMenu(menu);
        match cmd {
            0 => None,
            1 => Some("auto".to_string()),
            c => i18n::LOCALES.get(c - 2).map(|l| l.0.to_string()),
        }
    }
}

unsafe extern "system" fn settings_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let xy = || ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
    match msg {
        WM_PAINT => {
            let painted = with_app(|app| {
                let th = app.theme();
                app.settings.paint(&th, &app.cfg, &app.metrics, util::autostart_enabled());
            });
            if painted.is_none() {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            with_app(|app| app.settings.invalidate());
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            with_app(|app| {
                if app.settings.mouse_move(x, y) {
                    app.settings.invalidate();
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|app| app.settings.mouse_leave());
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy();
            with_app(|app| {
                if let Some(e) = app.settings.click(x, y) {
                    app.apply_settings(e);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            with_app(|app| app.settings.wheel(delta));
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            if with_app(|app| app.settings.over_control(pt.x, pt.y)).unwrap_or(false) {
                if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                    SetCursor(c);
                }
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            let s = util::dpi_for_window(hwnd).unwrap_or(96) as i32;
            mmi.ptMinTrackSize = POINT { x: 640 * s / 96, y: 420 * s / 96 };
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            with_app(|app| app.settings.invalidate());
            LRESULT(0)
        }
        WM_CLOSE => {
            with_app(|app| app.settings.hide());
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn open_taskmgr() {
    let verb = wide("open");
    let file = wide("taskmgr.exe");
    unsafe {
        ShellExecuteW(None, pcwstr(&verb), pcwstr(&file), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

const MENU_WIDGET_BASE: usize = 100;
const MENU_LABELS: usize = 200;
const MENU_AUTOSTART: usize = 202;
const MENU_GROUP_PE: usize = 203;
const MENU_SPLIT_PE: usize = 204;
/// vertical taskbar only: stretch one chart widget (+ CATALOG index)
const MENU_STRETCH_BASE: usize = 560;
/// per-module move: MENU_MOVE_BASE + module_index * 2 + (0 = left, 1 = right)
const MENU_MOVE_BASE: usize = 520;
const MENU_POS_RIGHT: usize = 210;
const MENU_POS_LEFT: usize = 211;
const MENU_THEME_AUTO: usize = 220;
const MENU_THEME_DARK: usize = 221;
const MENU_THEME_LIGHT: usize = 222;
/// popup background opacity: MENU_OPACITY_BASE + index in POPUP_OPACITY
const MENU_OPACITY_BASE: usize = 225;
const POPUP_OPACITY: [(&str, &str); 4] =
    [("more", "menu.opacity_more"), ("system", "menu.opacity_system"), ("less", "menu.opacity_less"), ("solid", "menu.opacity_solid")];
const MENU_INTERVAL_BASE: usize = 230;
/// per-module colour: MENU_COLOR_BASE + module_index * 20 + entry
const MENU_COLOR_BASE: usize = 300;
const MENU_COLOR_STRIDE: usize = 20;
/// global accent: MENU_ACCENT_BASE + palette index
const MENU_ACCENT_BASE: usize = 450;
const MENU_FONT_BASE: usize = 470;
const MENU_CHART_BASE: usize = 480;
const MENU_TOP_BASE: usize = 490;
const MENU_BAR_BASE: usize = 500;
/// language: MENU_LANG_BASE = system language, then one entry per locale
const MENU_LANG_BASE: usize = 600;
const TOP_COUNTS: &[u32] = &[5, 8, 10, 15, 20];
const CHART_WIDTHS: &[(u32, &str)] = &[(34, "menu.small"), (48, "menu.medium"), (64, "menu.large"), (96, "menu.extra_large")];
const FONT_SIZES: &[(f32, &str)] = &[(0.85, "menu.small"), (1.0, "menu.normal"), (1.15, "menu.large"), (1.3, "menu.extra_large")];
/// colour submenu entries before the palette
const COLOR_MODES: &[(&str, &str)] = &[("accent", "menu.accent"), ("utilization", "menu.by_utilisation"), ("mono", "menu.monochrome")];

fn module_key(k: Kind) -> &'static str {
    match k {
        Kind::Cpu => "cpu",
        Kind::Gpu => "gpu",
        Kind::Mem => "mem",
        Kind::Disk => "disk",
        Kind::Net => "net",
        Kind::Battery => "battery",
        Kind::Sensors => "sensors",
    }
}

/// Colour submenu for one module: modes first, then the palette.
unsafe fn color_submenu(cfg: &Config, mi: usize, kind: Kind, dpi: u32) -> Option<HMENU> {
    unsafe {
        let sub = CreatePopupMenu().ok()?;
        let current = cfg.colors.get(module_key(kind)).map(String::as_str).unwrap_or("accent");
        let base = MENU_COLOR_BASE + mi * MENU_COLOR_STRIDE;
        color_entries(sub, base, current, cfg, dpi);
        Some(sub)
    }
}

/// Body of a colour submenu: the modes (the accent one showing the current accent colour),
/// a separator, then the palette with a swatch per entry.
unsafe fn color_entries(sub: HMENU, base: usize, current: &str, cfg: &Config, dpi: u32) {
    unsafe {
        for (i, (name, title)) in COLOR_MODES.iter().enumerate() {
            let color = if *name == "accent" { palette_color(&cfg.accent, widgets::BLUE) } else { None };
            menu_color_item(sub, base + i, &t(title), current == *name, color, dpi);
        }
        let _ = AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null());
        for (i, (name, title, c)) in widgets::PALETTE.iter().enumerate() {
            menu_color_item(sub, base + COLOR_MODES.len() + i, &t(title), current == *name, palette_color(name, *c), dpi);
        }
    }
}

/// Update intervals as shown in the menu.
fn interval_title(ms: u32) -> String {
    let unit = t("settings.seconds_short");
    if ms % 1000 == 0 {
        format!("{} {unit}", ms / 1000)
    } else {
        // "0,5 sn" in Turkish, "0.5 s" in English
        format!("{} {unit}", util::format_decimal(ms as f64 / 1000.0, 1))
    }
}

unsafe fn menu_item(menu: HMENU, id: usize, text: &str, checked: bool) {
    let t = wide(text);
    let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = unsafe { AppendMenuW(menu, flags, id, pcwstr(&t)) };
}

/// Checkable entry with a colour square in the check column instead of a tick.
unsafe fn menu_color_item(menu: HMENU, id: usize, text: &str, checked: bool, color: Option<Color>, dpi: u32) {
    unsafe {
        menu_item(menu, id, text, checked);
        if let Some(bmp) = color.and_then(|c| menu::swatch(c, dpi)) {
            let pos = GetMenuItemCount(menu) - 1;
            if pos >= 0 {
                menu::set_item_bitmap(menu, pos as u32, bmp);
            }
        }
    }
}

/// Palette entry -> the colour it stands for on this machine ("system" follows the accent).
fn palette_color(name: &str, fallback: Color) -> Option<Color> {
    Some(widgets::named_color(name).unwrap_or(fallback))
}

unsafe fn menu_submenu(menu: HMENU, sub: HMENU, text: &str) {
    unsafe { menu_submenu_ex(menu, sub, text, true) }
}

/// Submenu entry; `enabled = false` greys it out (module the machine cannot provide).
unsafe fn menu_submenu_ex(menu: HMENU, sub: HMENU, text: &str, enabled: bool) {
    let t = wide(text);
    let flags = if enabled { MF_POPUP } else { MF_POPUP | MF_GRAYED };
    let _ = unsafe { AppendMenuW(menu, flags, sub.0 as usize, pcwstr(&t)) };
}

/// Settings menu: one submenu per module with a checkable entry per widget type.
unsafe fn build_menu(st: &MenuState) -> Option<HMENU> {
    let MenuState { cfg, hybrid, splittable, gpu_ok, battery_ok, dpi, vertical, .. } = st;
    let (hybrid, splittable, gpu_ok, battery_ok, dpi, vertical) = (*hybrid, *splittable, *gpu_ok, *battery_ok, *dpi, *vertical);
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        menu_item(menu, MENU_SETTINGS, &t("menu.settings"), false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        for (mi, kind) in widgets::MODULES.iter().enumerate() {
            let sub = CreatePopupMenu().ok()?;
            for (i, (k, name, title)) in widgets::CATALOG.iter().enumerate() {
                if k != kind {
                    continue;
                }
                let on = cfg.widgets.iter().any(|w| w == name);
                menu_item(sub, MENU_WIDGET_BASE + i, &t(title), on);
                if vertical && on && widgets::is_chart(name) {
                    menu_item(sub, MENU_STRETCH_BASE + i, &t("menu.stretch_chart"), cfg.stretched.iter().any(|w| w == name));
                }
            }
            if *kind != Kind::Battery {
                let _ = AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null());
                if let Some(cs) = color_submenu(cfg, mi, *kind, dpi) {
                    menu_submenu(sub, cs, &t("menu.colour"));
                }
            }
            if *kind == Kind::Cpu && splittable {
                menu_item(sub, MENU_SPLIT_PE, &t(if hybrid { "menu.show_pe_splitter" } else { "menu.show_splitter" }), cfg.split_pe_cores);
            }
            if *kind == Kind::Cpu && hybrid {
                menu_item(sub, MENU_GROUP_PE, &t("menu.group_pe_cores"), cfg.group_pe_cores);
            }
            let _ = AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null());
            menu_item(sub, MENU_MOVE_BASE + mi * 2, &t("menu.move_left"), false);
            menu_item(sub, MENU_MOVE_BASE + mi * 2 + 1, &t("menu.move_right"), false);
            let enabled = widgets::CATALOG.iter().any(|(k, n, _)| k == kind && cfg.widgets.iter().any(|w| w == n));
            let available = match kind {
                Kind::Gpu => gpu_ok,
                Kind::Battery => battery_ok,
                _ => true,
            };
            let title = if !available {
                format!("{}  {}", kind.title(), t("menu.na"))
            } else if enabled {
                format!("{}  \u{2022}", kind.title())
            } else {
                kind.title().to_string()
            };
            menu_submenu_ex(menu, sub, &title, available);
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        menu_item(menu, MENU_LABELS, &t("menu.labels"), cfg.labels);

        let accent = CreatePopupMenu().ok()?;
        for (i, (name, title, c)) in widgets::PALETTE.iter().enumerate() {
            menu_color_item(accent, MENU_ACCENT_BASE + i, &t(title), cfg.accent == *name, palette_color(name, *c), dpi);
        }
        menu_submenu(menu, accent, &t("menu.accent_colour"));

        let font = CreatePopupMenu().ok()?;
        for (i, (scale, title)) in FONT_SIZES.iter().enumerate() {
            menu_item(font, MENU_FONT_BASE + i, &t(title), (cfg.font_scale - scale).abs() < 0.01);
        }
        menu_submenu(menu, font, &t("menu.font_size"));

        let chart = CreatePopupMenu().ok()?;
        for (i, (w, title)) in CHART_WIDTHS.iter().enumerate() {
            menu_item(chart, MENU_CHART_BASE + i, &t(title), cfg.chart_width == *w);
        }
        menu_submenu(menu, chart, &t("menu.chart_width"));

        let bars = CreatePopupMenu().ok()?;
        for (i, (name, _, title)) in widgets::BAR_WIDTHS.iter().enumerate() {
            menu_item(bars, MENU_BAR_BASE + i, &t(title), cfg.bar_width == *name);
        }
        menu_submenu(menu, bars, &t("menu.bar_width"));

        let top = CreatePopupMenu().ok()?;
        for (i, n) in TOP_COUNTS.iter().enumerate() {
            menu_item(top, MENU_TOP_BASE + i, &n.to_string(), cfg.top_processes == *n);
        }
        menu_submenu(menu, top, &t("menu.top_processes"));

        let pos = CreatePopupMenu().ok()?;
        menu_item(pos, MENU_POS_RIGHT, &t("menu.pos_right"), cfg.position != "left");
        menu_item(pos, MENU_POS_LEFT, &t("menu.pos_left"), cfg.position == "left");
        menu_submenu(menu, pos, &t("menu.position"));

        let interval = CreatePopupMenu().ok()?;
        for (i, ms) in config::INTERVALS.iter().enumerate() {
            menu_item(interval, MENU_INTERVAL_BASE + i, &interval_title(*ms), cfg.update_ms == *ms);
        }
        menu_submenu(menu, interval, &t("menu.update_interval"));

        let theme = CreatePopupMenu().ok()?;
        menu_item(theme, MENU_THEME_AUTO, &t("menu.theme_auto"), cfg.theme == "auto");
        menu_item(theme, MENU_THEME_DARK, &t("menu.theme_dark"), cfg.theme == "dark");
        menu_item(theme, MENU_THEME_LIGHT, &t("menu.theme_light"), cfg.theme == "light");
        menu_submenu(menu, theme, &t("menu.theme"));

        let opacity = CreatePopupMenu().ok()?;
        for (i, (value, key)) in POPUP_OPACITY.iter().enumerate() {
            menu_item(opacity, MENU_OPACITY_BASE + i, &t(key), cfg.popup_opacity == *value);
        }
        menu_submenu(menu, opacity, &t("menu.popup_opacity"));

        let lang = CreatePopupMenu().ok()?;
        menu_item(lang, MENU_LANG_BASE, &t("menu.lang_system"), cfg.language == "auto");
        let _ = AppendMenuW(lang, MF_SEPARATOR, 0, PCWSTR::null());
        for (i, (code, name, _)) in i18n::LOCALES.iter().enumerate() {
            menu_item(lang, MENU_LANG_BASE + 1 + i, name, cfg.language == *code);
        }
        menu_submenu(menu, lang, &t("menu.language"));

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        menu_item(menu, MENU_AUTOSTART, &t("menu.start_with_windows"), util::autostart_enabled());
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        menu_item(menu, MENU_TASKMGR, &t("menu.task_manager"), false);
        menu_item(menu, MENU_CONFIG, &t("menu.open_config"), false);
        menu_item(menu, MENU_RELOAD, &t("menu.reload_config"), false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        menu_item(menu, MENU_ABOUT, &t("menu.about"), false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        menu_item(menu, MENU_EXIT, &t("menu.exit"), false);
        Some(menu)
    }
}

/// Add `item` to the list when absent, remove it when present.
fn toggle(list: &mut Vec<String>, item: String) {
    if let Some(i) = list.iter().position(|s| *s == item) {
        list.remove(i);
    } else {
        list.push(item);
    }
}

/// Everything the menu builder needs, snapshotted from the app so no borrow is held while
/// the menu runs its modal loop.
struct MenuState {
    hidden: HWND,
    cfg: Config,
    hybrid: bool,
    splittable: bool,
    gpu_ok: bool,
    battery_ok: bool,
    dpi: u32,
    /// the taskbar is docked at a side: the vertical-layout items are shown
    vertical: bool,
    /// where the menu opens and which corner of it sits there (see `App::menu_anchor`)
    anchor: POINT,
    align: TRACK_POPUP_MENU_FLAGS,
}

fn menu_state() -> Option<MenuState> {
    with_app(|app| {
        let (anchor, align) = app.menu_anchor();
        MenuState {
            hidden: app.hidden,
            cfg: app.cfg.clone(),
            hybrid: app.metrics.cpu_hybrid(),
            splittable: app.metrics.cpu_breaks().iter().any(|b| *b),
            gpu_ok: app.metrics.gpu_available,
            battery_ok: app.metrics.battery_available,
            dpi: app.taskbar.as_ref().map(|t| t.dpi()).or_else(|| util::dpi_for_window(app.hidden)).unwrap_or(96),
            vertical: app.taskbar.as_ref().map_or(false, |t| t.vertical()),
            anchor,
            align,
        }
    })
}

/// Commands that change a setting and keep the menu open; the rest close it.
fn menu_sticky(id: u32) -> bool {
    !matches!(id as usize, MENU_EXIT | MENU_CONFIG | MENU_RELOAD | MENU_TASKMGR | MENU_ABOUT | MENU_SETTINGS)
}

fn menu_rebuild() -> Option<HMENU> {
    let st = menu_state()?;
    unsafe { build_menu(&st) }
}

fn menu_command(cmd: u32) {
    match cmd as usize {
        0 => {}
        MENU_EXIT => unsafe { PostQuitMessage(0) },
        MENU_TASKMGR => open_taskmgr(),
        MENU_CONFIG => {
            let verb = wide("open");
            let path = wide(&Config::path().to_string_lossy());
            unsafe {
                ShellExecuteW(None, pcwstr(&verb), pcwstr(&path), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        MENU_ABOUT => {
            with_app(|app| {
                app.popup.hide("about");
                let theme = app.theme();
                let light = app.is_light();
                app.about.show(&theme, light);
            });
        }
        MENU_RELOAD => {
            with_app(|app| app.reload());
        }
        MENU_SETTINGS => {
            with_app(|app| app.open_settings());
        }
        MENU_AUTOSTART => {
            let enable = !util::autostart_enabled();
            if !util::set_autostart(enable) {
                log!("autostart change failed");
            } else {
                log!("autostart {}", if enable { "enabled" } else { "disabled" });
            }
        }
        other => {
            with_app(|app| app.apply_menu(other));
        }
    }
}

fn show_menu() {
    let Some(st) = menu_state() else { return };
    let cmd = unsafe {
        let Some(menu) = build_menu(&st) else {
            menu::free_bitmaps();
            return;
        };
        let _ = SetForegroundWindow(st.hidden);
        let r = menu::track(st.hidden, menu, st.anchor, st.align, menu_sticky, menu_command, menu_rebuild);
        let _ = PostMessageW(st.hidden, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        menu::free_bitmaps();
        r
    };
    menu_command(cmd);
}

impl App {
    /// A click on a toggling row in the popup: flip the item in the config, save, re-render.
    fn popup_action(&mut self, action: popup::Action) {
        match &action {
            popup::Action::Copy(text) => {
                if util::set_clipboard(text) {
                    self.popup.flash_copied(text);
                    self.render_popup(false);
                } else {
                    util::log("clipboard: copy failed");
                }
                return;
            }
            popup::Action::ToggleNetSeries(rx) => {
                // at least one series stays on
                if *rx {
                    if self.cfg.net_chart_tx {
                        self.cfg.net_chart_rx = !self.cfg.net_chart_rx;
                    }
                } else if self.cfg.net_chart_rx {
                    self.cfg.net_chart_tx = !self.cfg.net_chart_tx;
                }
                self.cfg.save();
                self.render_popup(false);
                return;
            }
            popup::Action::LockProc(row, p) => {
                self.popup.toggle_lock(*row, p.clone());
                self.render_popup(false);
                return;
            }
            popup::Action::KillProc(pid) => {
                if !metrics::processes::kill(*pid) {
                    util::log(&format!("kill {pid}: refused"));
                }
                self.popup.unlock(*pid);
                self.render_popup(false);
                return;
            }
            _ => {}
        }
        // at least one interface / disk must stay on, otherwise the widget shows nothing
        match action {
            popup::Action::ToggleNet(alias) => {
                let last = self.metrics.net_ifaces.iter().filter(|i| i.enabled).count() <= 1
                    && self.metrics.net_ifaces.iter().any(|i| i.enabled && i.alias == alias);
                if last {
                    return;
                }
                toggle(&mut self.cfg.net_disabled, alias.clone());
                // show the change at once; the collector's next sample agrees
                let off = self.cfg.net_disabled.contains(&alias);
                for i in self.metrics.net_ifaces.iter_mut().filter(|i| i.alias == alias) {
                    i.enabled = !off;
                }
            }
            popup::Action::ToggleDisk(model) => {
                let last = self.metrics.disks.iter().filter(|d| d.enabled).count() <= 1
                    && self.metrics.disks.iter().any(|d| d.enabled && d.model == model);
                if last {
                    return;
                }
                toggle(&mut self.cfg.disks_disabled, model.clone());
                let off = self.cfg.disks_disabled.contains(&model);
                for d in self.metrics.disks.iter_mut().filter(|d| d.model == model) {
                    d.enabled = !off;
                }
            }
            _ => return,
        }
        self.cfg.save();
        self.reconfigure();
        self.render_popup(false);
    }

    fn apply_menu(&mut self, cmd: usize) {
        match cmd {
            MENU_LABELS => self.cfg.labels = !self.cfg.labels,
            c if (MENU_STRETCH_BASE..MENU_STRETCH_BASE + widgets::CATALOG.len()).contains(&c) => {
                toggle(&mut self.cfg.stretched, widgets::CATALOG[c - MENU_STRETCH_BASE].1.to_string());
            }
            MENU_SPLIT_PE => self.cfg.split_pe_cores = !self.cfg.split_pe_cores,
            c if (MENU_MOVE_BASE..MENU_MOVE_BASE + widgets::MODULES.len() * 2).contains(&c) => {
                let mi = (c - MENU_MOVE_BASE) / 2;
                let dir = if (c - MENU_MOVE_BASE) % 2 == 0 { -1 } else { 1 };
                widgets::move_module(&mut self.cfg.widgets, widgets::MODULES[mi], dir);
            }
            MENU_GROUP_PE => self.cfg.group_pe_cores = !self.cfg.group_pe_cores,
            c if (MENU_LANG_BASE..=MENU_LANG_BASE + i18n::LOCALES.len()).contains(&c) => {
                self.cfg.language = if c == MENU_LANG_BASE { "auto".to_string() } else { i18n::LOCALES[c - MENU_LANG_BASE - 1].0.to_string() };
                i18n::set_language(&self.cfg.language);
            }
            c if (MENU_TOP_BASE..MENU_TOP_BASE + TOP_COUNTS.len()).contains(&c) => {
                self.cfg.top_processes = TOP_COUNTS[c - MENU_TOP_BASE];
            }
            c if (MENU_CHART_BASE..MENU_CHART_BASE + CHART_WIDTHS.len()).contains(&c) => {
                self.cfg.chart_width = CHART_WIDTHS[c - MENU_CHART_BASE].0;
            }
            c if (MENU_BAR_BASE..MENU_BAR_BASE + widgets::BAR_WIDTHS.len()).contains(&c) => {
                self.cfg.bar_width = widgets::BAR_WIDTHS[c - MENU_BAR_BASE].0.to_string();
            }
            c if (MENU_FONT_BASE..MENU_FONT_BASE + FONT_SIZES.len()).contains(&c) => {
                self.cfg.font_scale = FONT_SIZES[c - MENU_FONT_BASE].0;
            }
            c if (MENU_ACCENT_BASE..MENU_ACCENT_BASE + widgets::PALETTE.len()).contains(&c) => {
                self.cfg.accent = widgets::PALETTE[c - MENU_ACCENT_BASE].0.to_string();
            }
            c if (MENU_COLOR_BASE..MENU_COLOR_BASE + widgets::MODULES.len() * MENU_COLOR_STRIDE).contains(&c) => {
                let mi = (c - MENU_COLOR_BASE) / MENU_COLOR_STRIDE;
                let entry = (c - MENU_COLOR_BASE) % MENU_COLOR_STRIDE;
                let value = if entry < COLOR_MODES.len() {
                    COLOR_MODES[entry].0
                } else {
                    widgets::PALETTE.get(entry - COLOR_MODES.len()).map(|p| p.0).unwrap_or("accent")
                };
                let key = module_key(widgets::MODULES[mi]).to_string();
                if value == "accent" {
                    self.cfg.colors.remove(&key);
                } else {
                    self.cfg.colors.insert(key, value.to_string());
                }
                self.cfg.utilization_colors = false;
            }
            MENU_POS_RIGHT => self.cfg.position = "right".into(),
            MENU_POS_LEFT => self.cfg.position = "left".into(),
            MENU_THEME_AUTO => self.cfg.theme = "auto".into(),
            MENU_THEME_DARK => self.cfg.theme = "dark".into(),
            MENU_THEME_LIGHT => self.cfg.theme = "light".into(),
            c if (MENU_OPACITY_BASE..MENU_OPACITY_BASE + POPUP_OPACITY.len()).contains(&c) => {
                self.cfg.popup_opacity = POPUP_OPACITY[c - MENU_OPACITY_BASE].0.into();
                self.popup.opacity = backdrop::Opacity::from_config(&self.cfg.popup_opacity);
            }
            c if (MENU_INTERVAL_BASE..MENU_INTERVAL_BASE + config::INTERVALS.len()).contains(&c) => {
                self.cfg.update_ms = config::INTERVALS[c - MENU_INTERVAL_BASE];
            }
            c if (MENU_WIDGET_BASE..MENU_WIDGET_BASE + widgets::CATALOG.len()).contains(&c) => {
                widgets::toggle_widget(&mut self.cfg.widgets, widgets::CATALOG[c - MENU_WIDGET_BASE].1);
            }
            _ => return,
        }
        self.cfg.save();
        self.widgets = widgets::build(&self.cfg.widgets);
        self.reconfigure();
        if self.cfg.position == "left" {
            if let Some(tb) = &self.taskbar {
                tb.restore();
            }
        }
        self.tick();
    }
}
