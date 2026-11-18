//! A program whose memory keeps growing for hours: the slow leak that ends with the whole machine
//! paging. Committed private memory (what the program has asked Windows for, which trimming does
//! not hide) is sampled every 10 minutes while the alert rule is on; a program counts when it grew
//! steadily over at least an hour of the last six.

use crate::metrics::processes::Entry;
use std::collections::{HashMap, VecDeque};

/// Time between samples, seconds.
pub const STEP_SECS: u64 = 600;
/// How far back growth is measured.
const WINDOW_SECS: u64 = 6 * 3600;
/// Growth only counts over at least this much history.
const MIN_SPAN_SECS: u64 = 3600;

/// Seconds between samples. A development build can run the whole watch 120 times faster
/// (WINDOWS_STATS_GROWTH_FAST=1: a sample every 5 s, an hour becomes 30 s) to try it on a real
/// leaking program.
pub fn step_secs() -> u64 {
    STEP_SECS / fast()
}

fn fast() -> u64 {
    #[cfg(debug_assertions)]
    if std::env::var_os("WINDOWS_STATS_GROWTH_FAST").is_some() {
        return 120;
    }
    1
}
/// A step counts as growth when it adds at least this much.
const STEP_BYTES: u64 = 1024 * 1024;

/// The program that grew the most.
#[derive(Clone, Debug, PartialEq)]
pub struct Grower {
    pub pid: u32,
    pub name: String,
    /// bytes added over `secs`
    pub grown: u64,
    /// committed private memory now, bytes
    pub now: u64,
    pub secs: u64,
}

struct Track {
    name: String,
    /// (seconds since the watch started, committed bytes)
    points: VecDeque<(u64, u64)>,
}

#[derive(Default)]
pub struct Growth {
    tracks: HashMap<u32, Track>,
}

impl Growth {
    /// Add a sample of every process taken at `t` seconds.
    pub fn observe(&mut self, procs: &[Entry], t: u64) {
        let mut alive = std::collections::HashSet::with_capacity(procs.len());
        for p in procs {
            alive.insert(p.pid);
            let track = self.tracks.entry(p.pid).or_insert_with(|| Track { name: p.name.clone(), points: VecDeque::new() });
            if track.name != p.name {
                // the id was reused by another program: start over
                *track = Track { name: p.name.clone(), points: VecDeque::new() };
            }
            track.points.push_back((t, p.commit));
            while track.points.front().is_some_and(|(at, _)| t.saturating_sub(*at) > WINDOW_SECS / fast()) {
                track.points.pop_front();
            }
        }
        self.tracks.retain(|pid, _| alive.contains(pid));
    }

    /// Forget everything (the rule was switched off, the system slept).
    pub fn clear(&mut self) {
        self.tracks.clear();
    }

    /// The program that grew the most, steadily, over the window; None while nothing does.
    pub fn worst(&self) -> Option<Grower> {
        self.tracks.iter().filter_map(|(pid, t)| steady_growth(&t.points).map(|(grown, now, secs)| Grower { pid: *pid, name: t.name.clone(), grown, now, secs: secs * fast() })).max_by_key(|g| g.grown)
    }
}

/// (bytes grown, bytes now, seconds) when the samples rise steadily: at least an hour of history,
/// seven in ten steps up, and the latest value near the highest (a program that already gave its
/// memory back is not leaking).
fn steady_growth(points: &VecDeque<(u64, u64)>) -> Option<(u64, u64, u64)> {
    let (&(t0, first), &(t1, last)) = (points.front()?, points.back()?);
    if t1.saturating_sub(t0) < MIN_SPAN_SECS / fast() || last <= first {
        return None;
    }
    let steps = points.len() - 1;
    let ups = points.iter().zip(points.iter().skip(1)).filter(|((_, a), (_, b))| *b >= a + STEP_BYTES).count();
    let peak = points.iter().map(|(_, v)| *v).max()?;
    (ups * 10 >= steps * 7 && last * 10 >= peak * 9).then_some((last - first, last, t1 - t0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    fn entry(pid: u32, name: &str, commit: u64) -> Entry {
        Entry { pid, parent: 0, name: name.into(), cpu_time: 0, private: commit, commit }
    }

    #[test]
    fn steady_growth_over_an_hour_is_reported() {
        let mut g = Growth::default();
        // 200 MB every 10 minutes for an hour; a busy but stable program next to it
        for i in 0..=6u64 {
            g.observe(&[entry(10, "leaky.exe", 500 * MB + i * 200 * MB), entry(20, "busy.exe", 900 * MB + (i % 2) * 300 * MB)], i * STEP_SECS);
        }
        let w = g.worst().unwrap();
        assert_eq!((w.name.as_str(), w.grown, w.now, w.secs), ("leaky.exe", 1200 * MB, 1700 * MB, 3600));
    }

    #[test]
    fn short_history_jumps_and_give_backs_are_not_leaks() {
        let mut g = Growth::default();
        for i in 0..=5u64 {
            g.observe(&[entry(10, "a.exe", 100 * MB + i * 300 * MB)], i * STEP_SECS);
        }
        assert_eq!(g.worst(), None, "50 minutes is not enough history");
        // one big jump, then flat: mostly no growth steps
        let mut g = Growth::default();
        for i in 0..=8u64 {
            g.observe(&[entry(10, "b.exe", if i < 1 { 100 * MB } else { 2000 * MB })], i * STEP_SECS);
        }
        assert_eq!(g.worst(), None);
        // grew, then released most of it
        let mut g = Growth::default();
        for i in 0..=8u64 {
            g.observe(&[entry(10, "c.exe", if i < 8 { 100 * MB + i * 200 * MB } else { 600 * MB })], i * STEP_SECS);
        }
        assert_eq!(g.worst(), None);
    }

    #[test]
    fn reused_ids_and_exited_programs_start_over() {
        let mut g = Growth::default();
        for i in 0..=6u64 {
            g.observe(&[entry(10, "a.exe", 100 * MB + i * 200 * MB)], i * STEP_SECS);
        }
        assert!(g.worst().is_some());
        g.observe(&[entry(10, "other.exe", 50 * MB)], 7 * STEP_SECS);
        assert_eq!(g.worst(), None);
        g.observe(&[], 8 * STEP_SECS);
        assert!(g.tracks.is_empty());
    }

    #[test]
    fn old_samples_leave_the_window() {
        let mut g = Growth::default();
        for i in 0..=48u64 {
            g.observe(&[entry(10, "a.exe", 100 * MB + i * 10 * MB)], i * STEP_SECS);
        }
        let w = g.worst().unwrap();
        assert_eq!(w.secs, WINDOW_SECS);
        assert_eq!(w.grown, 36 * 10 * MB);
    }
}
