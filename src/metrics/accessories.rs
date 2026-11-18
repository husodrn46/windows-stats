//! Read-only Windows Bluetooth battery properties. No radio discovery, device commands,
//! driver, elevation, shell process, or vendor protocol. Run the OS calls off the collector.
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessoryBattery {
    /// Stable local device/container identity, never sent to telemetry or MCP.
    pub id: String,
    pub name: String,
    pub level: u8,
}

#[derive(Clone, Debug)]
struct Observation {
    id: String,
    name: String,
    level: Option<u8>,
    connected: bool,
}

fn normalize(rows: Vec<Observation>) -> Vec<AccessoryBattery> {
    let mut devices = BTreeMap::new();
    for row in rows {
        let Some(level) = row.level.filter(|n| *n <= 100) else {
            continue;
        };
        if !row.connected || row.id.is_empty() {
            continue;
        }
        // A classic Bluetooth peripheral can expose several service nodes. Conflicting
        // levels are not averaged or guessed: retain the lowest OS-reported value.
        let id = row.id.to_ascii_uppercase();
        let entry = devices.entry(id.clone()).or_insert(AccessoryBattery {
            id,
            name: row
                .name
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect(),
            level,
        });
        entry.level = entry.level.min(level);
    }
    let mut devices: Vec<_> = devices.into_values().collect();
    devices.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    devices
}

/// Only one OS request can be in flight. A slow Bluetooth stack never blocks metric/UI
/// refresh or creates unbounded workers. Results expire rather than becoming fake live data.
pub struct Monitor {
    tx: Option<SyncSender<u64>>,
    rx: Receiver<(u64, Option<Vec<AccessoryBattery>>)>,
    generation: u64,
    pending: bool,
    next: Instant,
    last: Option<Instant>,
    data: Vec<AccessoryBattery>,
}
impl Default for Monitor {
    fn default() -> Self {
        let (tx, requests) = mpsc::sync_channel(1);
        let (results, rx) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("bluetooth-batteries".into())
            .spawn(move || {
                while let Ok(generation) = requests.recv() {
                    if results
                        .send((generation, platform::read().map(normalize)))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        Self {
            tx: worker.ok().map(|_| tx),
            rx,
            generation: 0,
            pending: false,
            next: Instant::now(),
            last: None,
            data: Vec::new(),
        }
    }
}
impl Monitor {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.data.clear();
        self.last = None;
        self.next = Instant::now();
        // Discard a pre-suspend result. The pending request may still finish; tick will
        // consume it, but a new scan is requested below before trusting data again.
        while self.rx.try_recv().is_ok() {
            self.pending = false;
        }
    }
    pub fn tick(&mut self) -> Vec<AccessoryBattery> {
        let now = Instant::now();
        if let Ok((generation, result)) = self.rx.try_recv() {
            self.pending = false;
            if generation == self.generation {
                self.data = result.unwrap_or_default();
                self.last = Some(now);
            } else {
                self.next = now;
            }
        }
        if self
            .last
            .is_some_and(|last| now.duration_since(last) > Duration::from_secs(90))
        {
            self.data.clear();
        }
        if !self.pending && now >= self.next {
            if let Some(tx) = &self.tx {
                if tx.try_send(self.generation).is_ok() {
                    self.pending = true;
                }
            }
            self.next = now + Duration::from_secs(30);
        }
        self.data.clone()
    }
}

#[cfg(windows)]
mod platform {
    use super::Observation;
    use std::ffi::c_void;
    use windows::core::GUID;

    #[repr(C)]
    struct Key {
        fmtid: GUID,
        pid: u32,
    }
    const BATTERY: Key = Key {
        fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5),
        pid: 2,
    };
    const NAME: Key = Key {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };
    const DESCRIPTION: Key = Key {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 2,
    };
    const CONTAINER: Key = Key {
        fmtid: GUID::from_u128(0x8c7ed206_3f8a_4827_b3ab_ae9e1faefc6c),
        pid: 2,
    };
    const SUCCESS: u32 = 0;
    const BUFFER_SMALL: u32 = 0x1a;
    const PRESENT_ENUMERATOR: u32 = 0x100 | 0x1;
    const DISCONNECTED: u32 = 0x02000000;
    const STARTED: u32 = 0x00000008;
    const HAS_PROBLEM: u32 = 0x00000400;
    #[link(name = "cfgmgr32")]
    extern "system" {
        fn CM_Get_Device_ID_List_SizeW(len: *mut u32, filter: *const u16, flags: u32) -> u32;
        fn CM_Get_Device_ID_ListW(
            filter: *const u16,
            buffer: *mut u16,
            len: u32,
            flags: u32,
        ) -> u32;
        fn CM_Locate_DevNodeW(node: *mut u32, id: *const u16, flags: u32) -> u32;
        fn CM_Get_DevNode_Status(status: *mut u32, problem: *mut u32, node: u32, flags: u32)
            -> u32;
        fn CM_Get_DevNode_PropertyW(
            node: u32,
            key: *const Key,
            kind: *mut u32,
            buffer: *mut c_void,
            len: *mut u32,
            flags: u32,
        ) -> u32;
    }
    fn property(node: u32, key: &Key, expected: u32) -> Option<Vec<u8>> {
        let mut kind = 0;
        let mut len = 0;
        unsafe {
            let status =
                CM_Get_DevNode_PropertyW(node, key, &mut kind, std::ptr::null_mut(), &mut len, 0);
            if status != BUFFER_SMALL || len == 0 || len > 8192 {
                return None;
            }
            let mut bytes = vec![0; len as usize];
            if CM_Get_DevNode_PropertyW(
                node,
                key,
                &mut kind,
                bytes.as_mut_ptr().cast(),
                &mut len,
                0,
            ) != SUCCESS
                || kind != expected
                || len as usize > bytes.len()
            {
                return None;
            }
            bytes.truncate(len as usize);
            Some(bytes)
        }
    }
    fn string(node: u32, key: &Key) -> Option<String> {
        let bytes = property(node, key, 0x12)?; // DEVPROP_TYPE_STRING
        if bytes.len() % 2 != 0 {
            return None;
        }
        let wide: Vec<_> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect();
        String::from_utf16(&wide).ok().filter(|s| !s.is_empty())
    }
    pub fn read() -> Option<Vec<Observation>> {
        let mut out = Vec::new();
        for enumerator in ["BTHENUM", "BTHLE", "BTHLEDEVICE"] {
            let filter: Vec<u16> = enumerator.encode_utf16().chain([0]).collect();
            let mut ids = None;
            for _ in 0..2 {
                let mut size = 0;
                if unsafe {
                    CM_Get_Device_ID_List_SizeW(&mut size, filter.as_ptr(), PRESENT_ENUMERATOR)
                } != SUCCESS
                    || size > 1_048_576
                {
                    return None;
                }
                if size == 0 {
                    ids = Some(Vec::new());
                    break;
                }
                let mut buffer = vec![0; size as usize];
                let status = unsafe {
                    CM_Get_Device_ID_ListW(
                        filter.as_ptr(),
                        buffer.as_mut_ptr(),
                        size,
                        PRESENT_ENUMERATOR,
                    )
                };
                if status == SUCCESS {
                    ids = Some(buffer);
                    break;
                }
                if status != BUFFER_SMALL {
                    return None;
                }
            }
            for id in ids?.split(|c| *c == 0).filter(|id| !id.is_empty()) {
                let terminated: Vec<u16> = id.iter().copied().chain([0]).collect();
                let (mut node, mut status, mut problem) = (0, 0, 0);
                unsafe {
                    if CM_Locate_DevNodeW(&mut node, terminated.as_ptr(), 0) != SUCCESS
                        || CM_Get_DevNode_Status(&mut status, &mut problem, node, 0) != SUCCESS
                    {
                        continue;
                    }
                }
                if status & DISCONNECTED != 0 || status & STARTED == 0 || status & HAS_PROBLEM != 0
                {
                    continue;
                }
                let level = property(node, &BATTERY, 3).and_then(|b| {
                    if b.len() == 1 {
                        Some(b[0])
                    } else {
                        None
                    }
                });
                let Some(level) = level.filter(|n| *n <= 100) else {
                    continue;
                };
                let Some(name) = string(node, &NAME).or_else(|| string(node, &DESCRIPTION)) else {
                    continue;
                };
                let identity = match property(node, &CONTAINER, 0x0d)
                    .filter(|b| b.len() == 16 && b.iter().any(|v| *v != 0))
                {
                    Some(bytes) => format!(
                        "bt-container:{}",
                        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                    ),
                    None => format!("bt-node:{}", String::from_utf16_lossy(id)),
                };
                out.push(Observation {
                    id: identity,
                    name,
                    level: Some(level),
                    connected: true,
                });
            }
        }
        Some(out)
    }
}
#[cfg(not(windows))]
mod platform {
    pub fn read() -> Option<Vec<super::Observation>> {
        Some(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: &str, level: Option<u8>, connected: bool) -> Observation {
        Observation {
            id: id.into(),
            name: "Mouse".into(),
            level,
            connected,
        }
    }
    #[test]
    fn unknown_and_invalid_are_not_zero() {
        assert!(normalize(vec![
            row("a", None, true),
            row("b", Some(101), true),
            row("c", Some(255), true)
        ])
        .is_empty());
        assert_eq!(normalize(vec![row("a", Some(0), true)])[0].level, 0);
        assert_eq!(normalize(vec![row("a", Some(100), true)])[0].level, 100);
    }
    #[test]
    fn disconnected_and_duplicate_nodes() {
        let devices = normalize(vec![
            row("A", Some(80), true),
            row("a", Some(70), true),
            row("b", Some(50), false),
        ]);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].level, 70);
    }
    #[test]
    fn reconnect_preserves_identity() {
        let before = normalize(vec![row("a", Some(90), true)]);
        assert!(normalize(vec![row("a", Some(90), false)]).is_empty());
        assert_eq!(
            before[0].id,
            normalize(vec![row("a", Some(85), true)])[0].id
        );
    }
    #[test]
    fn suspend_discards_in_flight_result() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut monitor = Monitor {
            tx: None,
            rx,
            generation: 0,
            pending: true,
            next: Instant::now(),
            last: None,
            data: Vec::new(),
        };
        monitor.invalidate();
        tx.send((0, Some(normalize(vec![row("a", Some(50), true)]))))
            .unwrap();
        assert!(monitor.tick().is_empty());
        assert!(!monitor.pending);
        tx.send((
            monitor.generation,
            Some(normalize(vec![row("a", Some(49), true)])),
        ))
        .unwrap();
        assert_eq!(monitor.tick()[0].level, 49);
    }
    #[test]
    fn stale_results_are_removed() {
        let (_tx, rx) = mpsc::sync_channel(1);
        let mut monitor = Monitor {
            tx: None,
            rx,
            generation: 0,
            pending: false,
            next: Instant::now(),
            last: Some(Instant::now() - Duration::from_secs(91)),
            data: normalize(vec![row("a", Some(40), true)]),
        };
        assert!(monitor.tick().is_empty());
    }
}
