//! Round-trip time to a host, measured only while the network popup is open: the latency and
//! jitter rows. Sends ICMP echo only to the configured host. ICMP failures do not trigger
//! alternative protocols or probes to the adapter's DNS servers or gateways.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::IpHelper::{IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY};

/// replies the jitter is averaged over
const WINDOW: usize = 10;
/// consecutive misses before the stale measurement is hidden
const GIVE_UP: u32 = 3;
const TIMEOUT_MS: u32 = 2000;

struct State {
    active: bool,
    host: String,
    /// the user-configured host currently being measured
    target: String,
    rtts: VecDeque<f32>,
    last: Option<f32>,
    /// consecutive probes without a reply
    lost: u32,
}

pub struct Ping {
    st: Arc<(Mutex<State>, Condvar)>,
}

impl Ping {
    pub fn new(host: &str) -> Self {
        let st = Arc::new((
            Mutex::new(State { active: false, host: host.to_string(), target: String::new(), rtts: VecDeque::new(), last: None, lost: 0 }),
            Condvar::new(),
        ));
        let worker = st.clone();
        let _ = std::thread::Builder::new().name("ping".into()).spawn(move || run(worker));
        Self { st }
    }

    /// Measure while `active`; `host` is the configured target, "" switches the probe off.
    pub fn set(&self, active: bool, host: &str) {
        let (m, cv) = &*self.st;
        let mut s = m.lock().unwrap();
        let on = active && !host.is_empty();
        if s.host != host || (s.active && !on) {
            s.host = host.to_string();
            s.target.clear();
            s.rtts.clear();
            s.last = None;
            s.lost = 0;
        }
        if s.active != on {
            s.active = on;
            cv.notify_all();
        }
    }

    /// (latency of the last reply, mean change between consecutive replies, target in use), ms.
    /// The values are None before the first reply or while nothing answers.
    pub fn read(&self) -> (Option<f32>, Option<f32>, String) {
        let s = self.st.0.lock().unwrap();
        if s.lost >= GIVE_UP {
            return (None, None, s.target.clone());
        }
        let jitter = if s.rtts.len() >= 2 {
            let sum: f32 = s.rtts.iter().zip(s.rtts.iter().skip(1)).map(|(a, b)| (a - b).abs()).sum();
            Some(sum / (s.rtts.len() - 1) as f32)
        } else {
            None
        };
        (s.last, jitter, s.target.clone())
    }
}

/// A probe has exactly one possible protocol and destination: ICMP to the user-selected
/// host. Keeping the plan typed makes adding an implicit fallback a deliberate change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Icmp(Ipv4Addr),
}

fn configured_probe(host: &str) -> Option<Probe> {
    resolve(host).map(Probe::Icmp)
}

fn run(st: Arc<(Mutex<State>, Condvar)>) {
    let (m, cv) = &*st;
    let mut handle: Option<HANDLE> = None;
    let mut host = String::new();
    let mut probe: Option<Probe> = None;
    loop {
        let cur_host = {
            let mut s = m.lock().unwrap();
            while !s.active {
                s = cv.wait(s).unwrap();
            }
            s.host.clone()
        };
        if cur_host != host {
            host = cur_host;
            probe = configured_probe(&host);
        }
        if handle.is_none() {
            handle = unsafe { IcmpCreateFile() }.ok();
        }

        // An unanswered ICMP echo is only a lost sample. Never probe another address,
        // open a TCP connection, or inspect adapter DNS / gateway configuration.
        let rtt = match (handle, probe) {
            (Some(h), Some(Probe::Icmp(ip))) => echo(h, ip),
            _ => None,
        };
        let label = if probe.is_some() { host.clone() } else { String::new() };
        {
            let mut s = m.lock().unwrap();
            if s.active && s.host == host {
                s.target = label;
                match rtt {
                    Some(ms) => {
                        s.last = Some(ms);
                        s.lost = 0;
                        s.rtts.push_back(ms);
                        while s.rtts.len() > WINDOW {
                            s.rtts.pop_front();
                        }
                    }
                    None => {
                        s.lost = s.lost.saturating_add(1);
                        if s.lost == GIVE_UP {
                            s.last = None;
                            s.rtts.clear();
                        }
                    }
                }
            }
        }
        // One probe a second; interruptible so turning off takes effect after
        // an in-flight echo completes.
        let s = m.lock().unwrap();
        let _ = cv.wait_timeout(s, Duration::from_secs(1)).unwrap();
    }
}

fn resolve(host: &str) -> Option<Ipv4Addr> {
    format!("{host}:0").to_socket_addrs().ok()?.find_map(|a| match a.ip() {
        IpAddr::V4(v4) => Some(v4),
        _ => None,
    })
}

/// One echo request; the time until the reply in ms, None on timeout or error.
fn echo(h: HANDLE, ip: Ipv4Addr) -> Option<f32> {
    let data = [0x61u8; 32];
    let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + data.len() + 8];
    let dst = u32::from_ne_bytes(ip.octets());
    let t0 = Instant::now();
    let n = unsafe { IcmpSendEcho(h, dst, data.as_ptr() as *const _, data.len() as u16, None, reply.as_mut_ptr() as *mut _, reply.len() as u32, TIMEOUT_MS) };
    if n == 0 {
        return None;
    }
    let r = unsafe { &*(reply.as_ptr() as *const ICMP_ECHO_REPLY) };
    if r.Status != 0 {
        return None;
    }
    Some(t0.elapsed().as_secs_f32() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_ipv4_produces_only_icmp_to_that_address() {
        let expected = Ipv4Addr::new(192, 0, 2, 42);
        assert_eq!(configured_probe("192.0.2.42"), Some(Probe::Icmp(expected)));
    }

    #[test]
    fn changing_the_host_replaces_the_destination() {
        let first = configured_probe("192.0.2.42");
        let second = configured_probe("198.51.100.7");
        assert_ne!(first, second);
        assert_eq!(second, Some(Probe::Icmp(Ipv4Addr::new(198, 51, 100, 7))));
    }

    #[test]
    fn empty_host_cannot_trigger_network_probes() {
        assert_eq!(configured_probe(""), None);
    }
}
