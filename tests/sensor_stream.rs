//! Exercises the production protocol on Windows or natively with:
//! rustc --edition=2021 --test tests/sensor_stream.rs -o <test-binary>

#[path = "../src/sensorhost/stream.rs"]
mod stream;

use std::io::{self, Cursor, Read};
use std::sync::{Arc, Mutex};
use stream::{parse_line, read_stream, HelperInfo, Reading, Shared};

fn reading(value: f32) -> Reading {
    Reading {
        hw_type: "Cpu".into(),
        hw: "Test CPU".into(),
        kind: "Temperature".into(),
        name: "CPU Package".into(),
        value,
    }
}

#[test]
fn parses_a_batch_and_rejects_nonfinite_readings() {
    let mut batch = Vec::new();
    let mut info = HelperInfo::default();
    assert!(!parse_line("H\t0.1.2\t0\t1", &mut batch, &mut info));
    assert!(info.running && !info.elevated && info.pawnio);
    assert!(!parse_line("B", &mut batch, &mut info));
    for value in ["47.5", "nan-ish", "NaN", "inf", "-inf"] {
        assert!(!parse_line(
            &format!("S\tCpu\tTest CPU\tTemperature\tCPU Package\t{value}\t/x"),
            &mut batch,
            &mut info
        ));
    }
    assert!(parse_line("E", &mut batch, &mut info));
    assert_eq!(batch, vec![reading(47.5)]);
}

// BufReader asks for the next chunk only after processing all lines in the last one.
// Inspect the actual published state at those boundaries, without a timing-sensitive thread.
struct Chunks {
    shared: Arc<Mutex<Shared>>,
    next: usize,
    fail: bool,
}

impl Read for Chunks {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let chunk = match self.next {
            0 => "H\t0.1.2\t0\t1\nB\nS\tCpu\tTest CPU\tTemperature\tCPU Package\t90\t/x\nE\n",
            1 => {
                let snapshot = self.shared.lock().unwrap();
                assert!(snapshot.info.running && snapshot.info.pawnio);
                assert_eq!(snapshot.readings, vec![reading(90.0)]);
                // A half-written next batch must not replace the published complete one.
                "B\nS\tCpu\tTest CPU\tTemperature\tCPU Package\t100\t/x\n"
            }
            _ => {
                assert_eq!(self.shared.lock().unwrap().readings, vec![reading(90.0)]);
                if self.fail {
                    return Err(io::Error::other("helper pipe failed"));
                }
                ""
            }
        };
        self.next += 1;
        let bytes = chunk.as_bytes();
        out[..bytes.len()].copy_from_slice(bytes);
        Ok(bytes.len())
    }
}

#[test]
fn eof_discards_the_last_complete_batch_and_helper_flags() {
    let shared = Arc::new(Mutex::new(Shared::default()));
    read_stream(
        Chunks {
            shared: shared.clone(),
            next: 0,
            fail: false,
        },
        shared.clone(),
        |_| {},
    );
    let snapshot = shared.lock().unwrap();
    assert!(snapshot.readings.is_empty());
    assert_eq!(snapshot.info, HelperInfo::default());
}

#[test]
fn a_pipe_error_also_discards_stale_readings() {
    let shared = Arc::new(Mutex::new(Shared::default()));
    read_stream(
        Chunks {
            shared: shared.clone(),
            next: 0,
            fail: true,
        },
        shared.clone(),
        |_| {},
    );
    let snapshot = shared.lock().unwrap();
    assert!(snapshot.readings.is_empty());
    assert_eq!(snapshot.info, HelperInfo::default());
}

#[test]
fn an_empty_or_header_only_stream_has_no_live_snapshot() {
    for text in ["", "H\t0.1.2\t1\t1\n"] {
        let shared = Arc::new(Mutex::new(Shared::default()));
        read_stream(Cursor::new(text), shared.clone(), |_| {});
        let snapshot = shared.lock().unwrap();
        assert!(snapshot.readings.is_empty());
        assert_eq!(snapshot.info, HelperInfo::default());
    }
}
