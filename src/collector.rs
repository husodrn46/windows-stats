//! Background collection. The `Metrics` collectors (NtQuerySystemInformation, PDH, process
//! lists, DXGI, SMBIOS, disk enumeration) live on their own thread, so the UI thread, which
//! owns a child window of Explorer's taskbar, never waits for them: a stalled UI thread could
//! stall the taskbar as well. Every sample is published as a `MetricsData` snapshot and the
//! hidden window is told with `WM_APP_DATA`; settings changes reach the thread as commands.

use crate::metrics::{Metrics, MetricsData};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the hidden window when a new snapshot is waiting in `Collector::take`.
pub const WM_APP_DATA: u32 = WM_APP + 3;

/// What the collector is set up with; also what a config reload sends.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub interval_ms: u32,
    pub disk: String,
    pub top_n: usize,
    pub net_disabled: Vec<String>,
    pub disks_disabled: Vec<String>,
    pub group_pe: bool,
    pub cpu_utility: bool,
    /// run the LibreHardwareMonitor helper
    pub lhm: bool,
    pub lhm_elevated: bool,
}

impl Settings {
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        Self {
            interval_ms: cfg.update_ms,
            disk: cfg.disk.clone(),
            top_n: cfg.top_processes as usize,
            net_disabled: cfg.net_disabled.clone(),
            disks_disabled: cfg.disks_disabled.clone(),
            group_pe: cfg.group_pe_cores,
            cpu_utility: cfg.cpu_utility,
            lhm: cfg.lhm_sensors,
            lhm_elevated: cfg.lhm_elevated,
        }
    }
}

pub enum Cmd {
    /// new interval / disk / lists: applied in place, the collectors are kept
    Configure(Settings),
    /// a popup is open: collect the per-process lists, per-disk rates, drives...
    Details(bool),
    /// a popup just opened: fresh per-process baseline and a sample right away
    BeginDetails,
    /// the network interfaces changed (cable, Wi-Fi, adapter): re-read them now
    RefreshNet,
    /// system suspend: stop sampling until `Resume`
    Suspend,
    /// back from suspend (or the display came on): re-baseline the counters, then go on
    Resume,
}

pub struct Collector {
    tx: Sender<Cmd>,
    latest: Arc<Mutex<Option<MetricsData>>>,
    /// a WM_APP_DATA is in the hidden window's queue and not yet handled
    posted: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Collector {
    /// Start the thread and wait for its first sample (the strip is not up yet, so waiting here
    /// blocks nothing). `None` when the thread could not start or produced nothing in time.
    pub fn start(hidden: HWND, settings: Settings) -> (Collector, Option<MetricsData>) {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let latest: Arc<Mutex<Option<MetricsData>>> = Arc::new(Mutex::new(None));
        let posted = Arc::new(AtomicBool::new(false));
        let (first_tx, first_rx) = mpsc::channel::<MetricsData>();
        let hwnd = hidden.0 as isize;
        let (l, p) = (latest.clone(), posted.clone());
        let thread = std::thread::Builder::new()
            .name("collector".into())
            .spawn(move || run(rx, l, p, hwnd, settings, first_tx))
            .map_err(|e| crate::log!("collector thread failed to start: {e}"))
            .ok();
        let first = first_rx.recv_timeout(Duration::from_secs(20)).ok();
        if first.is_none() {
            crate::log!("collector: no first sample");
        }
        (Collector { tx, latest, posted, thread }, first)
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// The newest snapshot, if one arrived since the last call.
    pub fn take(&self) -> Option<MetricsData> {
        self.posted.store(false, Ordering::Release);
        self.latest.lock().ok().and_then(|mut l| l.take())
    }

    /// Stop the thread (it finishes the sample in progress first).
    pub fn stop(self) {
        let Collector { tx, thread, .. } = self;
        // closing the channel ends the loop
        drop(tx);
        if let Some(t) = thread {
            let _ = t.join();
        }
    }
}

fn new_metrics(s: &Settings) -> Metrics {
    let mut m = Metrics::new(&s.disk, s.top_n, &s.net_disabled, &s.disks_disabled);
    m.set_group_pe(s.group_pe);
    m.cpu_utility = s.cpu_utility;
    m
}

fn run(rx: Receiver<Cmd>, latest: Arc<Mutex<Option<MetricsData>>>, posted: Arc<AtomicBool>, hwnd: isize, mut settings: Settings, first_tx: Sender<MetricsData>) {
    let started = Instant::now();
    let mut metrics = new_metrics(&settings);
    metrics.update(false);
    crate::log!("collector: ready in {} ms", started.elapsed().as_millis());
    let _ = first_tx.send(metrics.data.clone());

    let publish = |m: &Metrics| {
        if let Ok(mut l) = latest.lock() {
            *l = Some(m.data.clone());
        }
        // one message in the queue at a time: a busy UI thread gets the newest sample, not a backlog
        if !posted.swap(true, Ordering::AcqRel) {
            unsafe {
                if PostMessageW(HWND(hwnd as *mut _), WM_APP_DATA, WPARAM(0), LPARAM(0)).is_err() {
                    posted.store(false, Ordering::Release);
                }
            }
        }
    };

    let mut details = false;
    let mut suspended = false;
    // the optional sensor helper lives with the collectors
    let mut host: Option<crate::sensorhost::SensorHost> = None;
    let sync_host = |host: &mut Option<crate::sensorhost::SensorHost>, s: &Settings| {
        if s.lhm && host.is_none() {
            *host = crate::sensorhost::SensorHost::start(s.interval_ms.max(2000), s.lhm_elevated);
        } else if !s.lhm {
            *host = None;
        }
    };
    sync_host(&mut host, &settings);
    let interval = |s: &Settings| Duration::from_millis(s.interval_ms.clamp(250, 60_000) as u64);
    let mut next = Instant::now() + interval(&settings);
    let mut slow_logged = 0u32;
    loop {
        let wait = next.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Cmd::Configure(s)) => {
                if s.disk != settings.disk {
                    // the free-space readout follows another drive: simplest to start over
                    metrics = new_metrics(&s);
                    metrics.update(details);
                    publish(&metrics);
                } else {
                    if s.group_pe != settings.group_pe {
                        metrics.set_group_pe(s.group_pe);
                    }
                    if s.top_n != settings.top_n {
                        metrics.set_top_n(s.top_n);
                    }
                    if s.net_disabled != settings.net_disabled {
                        metrics.set_net_disabled(&s.net_disabled);
                    }
                    if s.disks_disabled != settings.disks_disabled {
                        metrics.set_disks_disabled(&s.disks_disabled);
                    }
                    metrics.cpu_utility = s.cpu_utility;
                    if s != settings {
                        publish(&metrics);
                    }
                }
                if s.interval_ms != settings.interval_ms {
                    next = Instant::now() + interval(&s);
                }
                if s.lhm_elevated != settings.lhm_elevated {
                    // restart the helper with the other rights
                    host = None;
                }
                sync_host(&mut host, &s);
                settings = s;
            }
            Ok(Cmd::Details(on)) => details = on,
            Ok(Cmd::BeginDetails) => {
                details = true;
                metrics.begin_details();
                publish(&metrics);
            }
            Ok(Cmd::RefreshNet) => {
                metrics.refresh_net();
                crate::log!("net: interfaces changed, {} listed", metrics.data.net_ifaces.len());
                publish(&metrics);
            }
            Ok(Cmd::Suspend) => {
                suspended = true;
                crate::log!("collector: suspended");
            }
            Ok(Cmd::Resume) => {
                if suspended {
                    // the deltas over the sleep would be meaningless: take a fresh baseline
                    // without publishing it, the next tick measures from here
                    suspended = false;
                    metrics.update(false);
                    next = Instant::now() + interval(&settings);
                    crate::log!("collector: resumed");
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !suspended {
                    let t0 = Instant::now();
                    metrics.update(details);
                    match &host {
                        Some(h) => {
                            let (r, info) = h.readings();
                            let first = metrics.data.ext_sensors.is_empty() && !r.is_empty();
                            metrics.data.apply_helper(r, info);
                            if first {
                                let temps = metrics.data.ext_sensors.iter().filter(|x| x.kind == "Temperature").count();
                                let fans = metrics.data.ext_sensors.iter().filter(|x| x.kind == "Fan").count();
                                crate::log!(
                                    "sensor helper: {} readings ({temps} temperatures, {fans} fans), elevated={}, pawnio={}, cpu_temp={:?}",
                                    metrics.data.ext_sensors.len(),
                                    info.elevated,
                                    info.pawnio,
                                    metrics.data.cpu_temp
                                );
                            }
                        }
                        None => metrics.data.apply_helper(Vec::new(), Default::default()),
                    }
                    metrics.data.helper_enabled = settings.lhm;
                    publish(&metrics);
                    let took = t0.elapsed();
                    if took > Duration::from_millis(250) && slow_logged < 20 {
                        slow_logged += 1;
                        crate::log!("collector: slow sample, {} ms (details={details})", took.as_millis());
                    }
                }
                next += interval(&settings);
                let now = Instant::now();
                if next <= now {
                    // fell behind (the machine was busy or asleep): no burst of catch-up samples
                    next = now + interval(&settings);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    crate::log!("collector: stopped");
}
