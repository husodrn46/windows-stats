//! `WindowsStats.exe --mcp`: a read-only Model Context Protocol server for AI apps (LM Studio,
//! Claude Desktop, VS Code and other MCP clients, also with local models such as Ollama's).
//!
//! The AI app starts this process and talks JSON-RPC to it over stdin/stdout; it opens no
//! port and ends when the app closes the pipes. It measures only when asked: a request takes
//! fresh readings, then samples once a second for two minutes (for `recent_history`) and goes
//! idle again. Names of running programs come only from `top_processes`, whose description
//! says so; network addresses, adapter and Wi-Fi names, drive labels and file paths never.

use crate::metrics::{Metrics, MetricsData};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Protocol revisions this server speaks, newest first.
const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// After a request: keep sampling this long, once a second.
const ACTIVE: Duration = Duration::from_secs(120);
const TICK: Duration = Duration::from_secs(1);
/// Readings older than this are refreshed before answering.
const FRESH: Duration = Duration::from_millis(1500);
const HISTORY: usize = 120;
const MAX_PROCESSES: usize = 15;

/// What the tools need from the machine; the protocol side is tested with a fake.
pub trait Host {
    fn overview(&mut self) -> Value;
    fn history(&mut self, seconds: u64) -> Value;
    fn processes(&mut self, by_memory: bool, limit: usize) -> Value;
    fn ai_sessions(&mut self, limit: usize) -> Value;
}

fn tools() -> Value {
    let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
    json!([
        {
            "name": "system_overview",
            "title": "System overview",
            "description": "Current load of this Windows computer, measured now: CPU (total, per logical processor, clock), memory, GPU load and video memory (VRAM), disk activity and free space per drive, network throughput, battery and the temperatures Windows reports. Use it to see whether a model fits in memory or what slows the machine down.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": read_only
        },
        {
            "name": "recent_history",
            "title": "Recent history",
            "description": "CPU, memory, GPU, disk and network figures once per second for the recent past. Windows Stats measures only while it is being asked, so the history starts at the first request and covers at most the last two minutes.",
            "inputSchema": {
                "type": "object",
                "properties": { "seconds": { "type": "integer", "minimum": 5, "maximum": 120, "default": 60, "description": "How far back, in seconds." } },
                "additionalProperties": false
            },
            "annotations": read_only
        },
        {
            "name": "top_processes",
            "title": "Top processes (program names)",
            "description": "The programs using the most CPU or memory right now, with their names. This reveals which programs are running: if this AI app uses a cloud model, the names are sent to that service.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sort_by": { "type": "string", "enum": ["cpu", "memory"], "default": "cpu" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_PROCESSES, "default": 5 }
                },
                "additionalProperties": false
            },
            "annotations": read_only
        },
        {
            "name": "ai_sessions",
            "title": "Local AI sessions",
            "description": "Local AI model sessions Windows Stats noticed on this computer (Ollama, llama.cpp, KoboldCpp, LocalAI): runtime, model, when, how long, how long the runtime was busy, and the highest CPU, GPU, video memory, memory and temperatures while the model ran, including a session running now. Empty unless the user allowed session following in Windows Stats.",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 20, "default": 5, "description": "How many finished sessions, newest first." } },
                "additionalProperties": false
            },
            "annotations": read_only
        }
    ])
}

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool result: the JSON as text (every client reads that) and as structured content.
fn tool_result(value: Value, summary: Option<String>) -> Value {
    let pretty = serde_json::to_string_pretty(&value).unwrap_or_default();
    let text = match summary {
        Some(s) => format!("{s}\n\n{pretty}"),
        None => pretty,
    };
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": value, "isError": false })
}

fn tool_error(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

/// Answer one JSON-RPC message; None for notifications (and for responses we never asked for).
pub fn respond(msg: &Value, host: &mut dyn Host) -> Option<Value> {
    let Some(obj) = msg.as_object() else {
        return Some(error(&Value::Null, -32600, "Invalid Request"));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // a response from the client, or garbage with an id
        return id.map(|id| error(&id, -32600, "Invalid Request"));
    };
    // notifications (no id) get no answer
    let id = id?;
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = if PROTOCOLS.contains(&asked) { asked } else { PROTOCOLS[0] };
            ok(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "windows-stats", "title": "Windows Stats", "version": crate::about::VERSION },
                    "instructions": "Read-only monitor of this Windows PC. Call system_overview for current CPU, memory, GPU/VRAM, disk, network and battery figures, recent_history for the last minutes, and top_processes only when program names are needed."
                }),
            )
        }
        "ping" => ok(&id, json!({})),
        "tools/list" => ok(&id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call(name, &args, host) {
                Ok(result) => ok(&id, result),
                Err(Rpc::InvalidParams(m)) => error(&id, -32602, &m),
                Err(Rpc::Tool(m)) => ok(&id, tool_error(&m)),
            }
        }
        _ => error(&id, -32601, "Method not found"),
    })
}

enum Rpc {
    InvalidParams(String),
    Tool(String),
}

fn call(name: &str, args: &Value, host: &mut dyn Host) -> Result<Value, Rpc> {
    let args = args.as_object().cloned().unwrap_or_default();
    let allow = |keys: &[&str], args: &Map<String, Value>| -> Result<(), Rpc> {
        match args.keys().find(|k| !keys.contains(&k.as_str())) {
            Some(k) => Err(Rpc::Tool(format!("unknown argument '{k}'"))),
            None => Ok(()),
        }
    };
    match name {
        "system_overview" => {
            allow(&[], &args)?;
            let v = host.overview();
            let summary = overview_summary(&v);
            Ok(tool_result(v, Some(summary)))
        }
        "recent_history" => {
            allow(&["seconds"], &args)?;
            let seconds = match args.get("seconds") {
                None => 60,
                Some(s) => s.as_u64().filter(|s| (5..=120).contains(s)).ok_or_else(|| Rpc::Tool("seconds must be a whole number from 5 to 120".into()))?,
            };
            Ok(tool_result(host.history(seconds), None))
        }
        "top_processes" => {
            allow(&["sort_by", "limit"], &args)?;
            let by_memory = match args.get("sort_by").map(|v| v.as_str()) {
                None | Some(Some("cpu")) => false,
                Some(Some("memory")) => true,
                _ => return Err(Rpc::Tool("sort_by must be \"cpu\" or \"memory\"".into())),
            };
            let limit = match args.get("limit") {
                None => 5,
                Some(l) => l.as_u64().filter(|l| (1..=MAX_PROCESSES as u64).contains(l)).ok_or_else(|| Rpc::Tool(format!("limit must be a whole number from 1 to {MAX_PROCESSES}")))? as usize,
            };
            Ok(tool_result(host.processes(by_memory, limit), None))
        }
        "ai_sessions" => {
            allow(&["limit"], &args)?;
            let limit = match args.get("limit") {
                None => 5,
                Some(l) => l.as_u64().filter(|l| (1..=20).contains(l)).ok_or_else(|| Rpc::Tool("limit must be a whole number from 1 to 20".into()))? as usize,
            };
            Ok(tool_result(host.ai_sessions(limit), None))
        }
        _ => Err(Rpc::InvalidParams(format!("Unknown tool: {name}"))),
    }
}

fn gb(bytes: f64) -> String {
    format!("{:.1} GB", bytes / 1e9)
}

/// One line a small model can read before the JSON.
fn overview_summary(v: &Value) -> String {
    let n = |p: &str| v.pointer(p).and_then(Value::as_f64);
    let mut parts = Vec::new();
    if let Some(c) = n("/cpu/load_pct") {
        parts.push(format!("CPU {c:.0}%"));
    }
    if let (Some(u), Some(t)) = (n("/memory/used_bytes"), n("/memory/total_bytes")) {
        parts.push(format!("RAM {} of {}", gb(u), gb(t)));
    }
    match (n("/gpu/load_pct"), n("/gpu/vram_used_bytes"), n("/gpu/vram_budget_bytes")) {
        (Some(g), Some(u), Some(b)) if b > 0.0 => parts.push(format!("GPU {g:.0}%, VRAM {} of {}", gb(u), gb(b))),
        (Some(g), _, _) => parts.push(format!("GPU {g:.0}%")),
        _ => {}
    }
    parts.join(" · ")
}

fn pct(v: f32) -> f64 {
    ((v as f64) * 1000.0).round() / 10.0
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// The overview from a sample, without names that could identify the user or the network.
pub fn overview_json(d: &MetricsData) -> Value {
    let clocks: Vec<u32> = d.cpu_freq.iter().copied().filter(|f| *f > 0).collect();
    let battery = d.battery.as_ref().map(|b| {
        json!({
            "level_pct": pct(b.level),
            "charging": b.charging,
            "on_ac_power": b.on_ac,
            "remaining_minutes": b.remaining_secs.map(|s| s / 60),
        })
    });
    json!({
        "cpu": {
            "name": d.cpu_name,
            "load_pct": pct(d.cpu_total),
            "system_pct": pct(d.cpu_system),
            "user_pct": pct(d.cpu_user),
            "logical_processors": d.cpu_cores.len(),
            "per_processor_pct": d.cpu_cores.iter().map(|c| pct(*c)).collect::<Vec<_>>(),
            "clock_mhz_average": if clocks.is_empty() { Value::Null } else { json!(clocks.iter().map(|c| *c as u64).sum::<u64>() / clocks.len() as u64) },
            "clock_mhz_max": clocks.iter().max(),
            "temperature_c": d.cpu_temp.map(|t| round1(t as f64)),
        },
        "memory": {
            "used_bytes": d.mem.used,
            "total_bytes": d.mem.total,
            "load_pct": pct(d.mem.pct),
            "commit_used_bytes": d.mem.commit_used,
            "commit_limit_bytes": d.mem.commit_total,
        },
        "gpu": {
            "name": d.gpu_name,
            "available": d.gpu_available,
            "load_pct": d.gpu.map(pct),
            "vram_used_bytes": d.vram_used,
            "vram_budget_bytes": d.vram_budget,
            "dedicated_vram_bytes": d.gpu_primary.as_ref().map(|a| a.dedicated),
            "temperature_c": d.gpu_temp.map(|t| round1(t as f64)),
        },
        "disk_activity": { "read_bytes_per_s": d.disk_read.round(), "write_bytes_per_s": d.disk_write.round() },
        "physical_disks": d.disks.iter().map(|k| json!({
            "model": k.model,
            "bus": k.bus,
            "ssd": k.ssd,
            "size_bytes": k.size,
            "temperature_c": k.temp_c.map(|t| round1(t as f64)),
        })).collect::<Vec<_>>(),
        "drives": d.drives.iter().map(|v| json!({
            "drive": v.name,
            "type": if v.kind.is_empty() { "fixed" } else { v.kind },
            "file_system": v.fs,
            "free_bytes": v.free,
            "total_bytes": v.total,
        })).collect::<Vec<_>>(),
        "network": { "download_bytes_per_s": d.net_rx.round(), "upload_bytes_per_s": d.net_tx.round() },
        "battery": battery,
    })
}

/// A session for AI apps: percentages, bytes, °C and seconds since 1970 (UTC).
pub fn session_json(s: &crate::aiwatch::Session) -> Value {
    json!({
        "runtime": s.runtime,
        "model": s.model,
        "model_vram_bytes": s.model_vram,
        "started_unix": s.start,
        "ended_unix": s.end,
        "duration_seconds": s.seconds,
        "busy_seconds": s.busy_seconds,
        "cpu_peak_pct": pct(s.peak_cpu),
        "cpu_average_pct": pct(s.avg_cpu),
        "gpu_peak_pct": s.peak_gpu.map(pct),
        "gpu_average_pct": s.avg_gpu.map(pct),
        "vram_peak_bytes": s.peak_vram,
        "memory_peak_bytes": s.peak_ram,
        "runtime_memory_peak_bytes": s.peak_runtime_ram,
        "runtime_cpu_peak_pct": pct(s.peak_runtime_cpu),
        "cpu_temperature_peak_c": s.peak_cpu_temp.map(|t| round1(t as f64)),
        "gpu_temperature_peak_c": s.peak_gpu_temp.map(|t| round1(t as f64)),
        "disk_temperature_peak_c": s.peak_disk_temp.map(|t| round1(t as f64)),
    })
}

/// One second of the history.
#[derive(Clone, Copy)]
struct Point {
    at: Instant,
    cpu: f32,
    mem: f32,
    gpu: Option<f32>,
    rx: f64,
    tx: f64,
    read: f64,
    write: f64,
}

/// The real host: measures with the same collectors as the strip.
struct Machine {
    metrics: Option<Metrics>,
    sampled: Option<Instant>,
    procs_at: Option<Instant>,
    active_until: Option<Instant>,
    history: VecDeque<Point>,
}

impl Machine {
    fn new() -> Self {
        Machine { metrics: None, sampled: None, procs_at: None, active_until: None, history: VecDeque::new() }
    }

    fn metrics(&mut self) -> &mut Metrics {
        self.metrics.get_or_insert_with(|| {
            let cfg = crate::config::Config::load_readonly();
            let mut m = Metrics::new(&cfg.disk, MAX_PROCESSES, &cfg.net_disabled, &cfg.disks_disabled);
            m.cpu_utility = cfg.cpu_utility;
            m
        })
    }

    fn record(&mut self) {
        let Some(m) = self.metrics.as_ref() else { return };
        let d = &m.data;
        if self.history.len() >= HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(Point { at: Instant::now(), cpu: d.cpu_total, mem: d.mem.pct, gpu: d.gpu, rx: d.net_rx, tx: d.net_tx, read: d.disk_read, write: d.disk_write });
    }

    /// Readings no older than FRESH; after an idle spell two samples half a second apart, so
    /// the rates describe now and not the time since the last request.
    fn fresh(&mut self) {
        let now = Instant::now();
        if self.sampled.map_or(false, |t| now.duration_since(t) < FRESH) {
            self.active_until = Some(now + ACTIVE);
            return;
        }
        let idle = self.sampled.map_or(true, |t| now.duration_since(t) > Duration::from_secs(3));
        if idle {
            self.metrics().update(false);
            std::thread::sleep(Duration::from_millis(500));
        }
        self.metrics().update(false);
        self.sampled = Some(Instant::now());
        self.active_until = Some(Instant::now() + ACTIVE);
        self.record();
    }

    /// While active: the next sample is due in…; None when idle (wait for a request).
    fn next_wait(&self) -> Option<Duration> {
        let until = self.active_until?;
        let now = Instant::now();
        if now >= until {
            return None;
        }
        Some(self.sampled.map_or(Duration::ZERO, |t| (t + TICK).saturating_duration_since(now)))
    }

    fn tick(&mut self) {
        if self.active_until.map_or(true, |u| Instant::now() >= u) {
            self.active_until = None;
            return;
        }
        self.metrics().update(false);
        self.sampled = Some(Instant::now());
        self.record();
    }
}

impl Host for Machine {
    fn overview(&mut self) -> Value {
        self.fresh();
        let m = self.metrics();
        m.refresh_details();
        overview_json(&m.data)
    }

    fn history(&mut self, seconds: u64) -> Value {
        self.fresh();
        let now = Instant::now();
        let points: Vec<Value> = self
            .history
            .iter()
            .filter(|p| now.duration_since(p.at).as_secs() < seconds)
            .map(|p| {
                json!({
                    "seconds_ago": round1(now.duration_since(p.at).as_secs_f64()),
                    "cpu_pct": pct(p.cpu),
                    "memory_pct": pct(p.mem),
                    "gpu_pct": p.gpu.map(pct),
                    "download_bytes_per_s": p.rx.round(),
                    "upload_bytes_per_s": p.tx.round(),
                    "disk_read_bytes_per_s": p.read.round(),
                    "disk_write_bytes_per_s": p.write.round(),
                })
            })
            .collect();
        json!({
            "interval_seconds": 1,
            "note": "Measured only while Windows Stats is being asked: the history starts at the first request and keeps the last two minutes.",
            "samples": points,
        })
    }

    fn processes(&mut self, by_memory: bool, limit: usize) -> Value {
        self.fresh();
        // per-process CPU needs two looks a second apart; once warm, every sample keeps it
        let warm = self.procs_at.map_or(false, |t| t.elapsed() < Duration::from_secs(3));
        if !warm {
            self.metrics().begin_details();
            std::thread::sleep(Duration::from_secs(1));
        }
        self.metrics().update(true);
        self.procs_at = Some(Instant::now());
        self.sampled = Some(Instant::now());
        let d = &self.metrics().data;
        let list = if by_memory { &d.top_mem } else { &d.top_cpu };
        json!({
            "sorted_by": if by_memory { "memory" } else { "cpu" },
            "processes": list.iter().take(limit).map(|p| json!({
                "name": p.name,
                "cpu_pct": pct(p.cpu),
                "memory_bytes": p.mem,
            })).collect::<Vec<_>>(),
        })
    }

    fn ai_sessions(&mut self, limit: usize) -> Value {
        // written by the running Windows Stats; nothing to measure here
        let cfg = crate::config::Config::load_readonly();
        json!({
            "following": cfg.ai_watch == Some(true),
            "running_now": crate::aiwatch::live().as_ref().map(session_json),
            "sessions": crate::aiwatch::recent(limit).iter().map(session_json).collect::<Vec<_>>(),
        })
    }
}

/// Serve until the client closes stdin.
pub fn run() {
    let (tx, rx) = mpsc::channel::<String>();
    // stdin lines on their own thread, so sampling continues between requests
    let _ = std::thread::Builder::new().name("mcp-stdin".into()).spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(l) = line else { break };
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    crate::log!("mcp: server started");
    let mut machine = Machine::new();
    let mut out = std::io::stdout().lock();
    loop {
        let got = match machine.next_wait() {
            Some(wait) => rx.recv_timeout(wait),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match got {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let reply = match serde_json::from_str::<Value>(line) {
                    Ok(msg) => respond(&msg, &mut machine),
                    Err(_) => Some(error(&Value::Null, -32700, "Parse error")),
                };
                if let Some(reply) = reply {
                    // one JSON object per line, nothing else on stdout
                    if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => machine.tick(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    crate::log!("mcp: client closed the connection");
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        calls: Vec<String>,
    }

    impl Host for Fake {
        fn overview(&mut self) -> Value {
            self.calls.push("overview".into());
            overview_json(&sample())
        }
        fn history(&mut self, seconds: u64) -> Value {
            self.calls.push(format!("history {seconds}"));
            json!({ "samples": [] })
        }
        fn processes(&mut self, by_memory: bool, limit: usize) -> Value {
            self.calls.push(format!("processes {by_memory} {limit}"));
            json!({ "processes": [] })
        }
        fn ai_sessions(&mut self, limit: usize) -> Value {
            self.calls.push(format!("ai {limit}"));
            json!({ "sessions": [] })
        }
    }

    fn sample() -> MetricsData {
        let mut d = MetricsData { cpu_name: "Test CPU".into(), cpu_total: 0.25, cpu_cores: vec![0.5, 0.0], gpu: Some(0.1), gpu_name: "Test GPU".into(), vram_used: 2_000_000_000, vram_budget: 8_000_000_000, net_rx: 1234.4, ..Default::default() };
        d.mem.used = 8_000_000_000;
        d.mem.total = 16_000_000_000;
        d.mem.pct = 0.5;
        d.net_ip = "192.168.1.20".into();
        d.net_iface = "Wi-Fi".into();
        d
    }

    fn rpc(host: &mut Fake, text: &str) -> Option<Value> {
        respond(&serde_json::from_str(text).unwrap(), host)
    }

    #[test]
    fn handshake_and_tool_list() {
        let mut h = Fake { calls: vec![] };
        let r = rpc(&mut h, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{}}}"#).unwrap();
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "windows-stats");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        // an unknown revision gets the newest one we speak
        let r = rpc(&mut h, r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#).unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOLS[0]);
        assert!(rpc(&mut h, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none(), "notifications get no answer");
        let r = rpc(&mut h, r#"{"jsonrpc":"2.0","id":"x","method":"tools/list"}"#).unwrap();
        let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["system_overview", "recent_history", "top_processes", "ai_sessions"]);
        for t in r["result"]["tools"].as_array().unwrap() {
            assert_eq!(t["annotations"]["readOnlyHint"], true, "{}", t["name"]);
            assert_eq!(t["inputSchema"]["type"], "object");
        }
        let warn = r["result"]["tools"][2]["description"].as_str().unwrap();
        assert!(warn.contains("cloud"), "top_processes says where names go");
        assert!(h.calls.is_empty(), "nothing measured for the handshake");
    }

    #[test]
    fn calling_tools() {
        let mut h = Fake { calls: vec![] };
        let r = rpc(&mut h, r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"system_overview","arguments":{}}}"#).unwrap();
        let res = &r["result"];
        assert_eq!(res["isError"], false);
        assert_eq!(res["structuredContent"]["cpu"]["load_pct"], 25.0);
        assert_eq!(res["structuredContent"]["memory"]["load_pct"], 50.0);
        let text = res["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("CPU 25% · RAM 8.0 GB of 16.0 GB · GPU 10%, VRAM 2.0 GB of 8.0 GB"), "{text}");
        assert!(!text.contains("192.168") && !text.contains("Wi-Fi"), "no addresses or adapter names");

        rpc(&mut h, r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"recent_history"}}"#).unwrap();
        rpc(&mut h, r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"recent_history","arguments":{"seconds":30}}}"#).unwrap();
        rpc(&mut h, r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"top_processes","arguments":{"sort_by":"memory","limit":3}}}"#).unwrap();
        rpc(&mut h, r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"ai_sessions","arguments":{"limit":2}}}"#).unwrap();
        assert_eq!(h.calls, ["overview", "history 60", "history 30", "processes true 3", "ai 2"]);
        let s = crate::aiwatch::Session { runtime: "Ollama".into(), model: Some("llama3.1:8b".into()), peak_cpu: 0.94, peak_gpu_temp: Some(71.04), ..Default::default() };
        let v = session_json(&s);
        assert_eq!(v["cpu_peak_pct"], 94.0);
        assert_eq!(v["gpu_temperature_peak_c"], 71.0);
        assert_eq!(v["model"], "llama3.1:8b");
    }

    #[test]
    fn bad_input_is_refused_without_measuring() {
        let mut h = Fake { calls: vec![] };
        for (args, expect) in [
            (r#"{"seconds":500}"#, "seconds"),
            (r#"{"seconds":"60"}"#, "seconds"),
            (r#"{"minutes":1}"#, "unknown argument"),
        ] {
            let r = rpc(&mut h, &format!(r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"recent_history","arguments":{args}}}}}"#)).unwrap();
            assert_eq!(r["result"]["isError"], true, "{args}");
            assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains(expect), "{args}");
        }
        let r = rpc(&mut h, r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"top_processes","arguments":{"limit":99}}}"#).unwrap();
        assert_eq!(r["result"]["isError"], true);
        let r = rpc(&mut h, r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"delete_files"}}"#).unwrap();
        assert_eq!(r["error"]["code"], -32602);
        let r = rpc(&mut h, r#"{"jsonrpc":"2.0","id":10,"method":"resources/list"}"#).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        let r = respond(&json!([1, 2]), &mut h).unwrap();
        assert_eq!(r["error"]["code"], -32600);
        assert!(h.calls.is_empty());
    }
}
