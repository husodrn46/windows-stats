//! Small, optional motion: values on the strip glide to each new sample, the detail panel
//! slides in. Off when the user turns "Animations" off or Windows' own animation effects are
//! off (Settings > Accessibility > Visual effects). Kept short so the strip stays cheap: a
//! glide is a handful of strip redraws after each sample, the panel moves its window only.

use crate::metrics::MetricsData;

/// How long values glide to a new sample, and how often the strip is redrawn meanwhile.
pub const GLIDE_MS: u64 = 280;
pub const GLIDE_FRAME_MS: u32 = 40;
/// How long the panel takes to slide in, its frame interval and distance (DIP).
pub const PANEL_MS: u64 = 170;
pub const PANEL_FRAME_MS: u32 = 15;
pub const PANEL_SLIDE_DIP: f32 = 14.0;

/// Windows' "Animation effects" switch (SPI_GETCLIENTAREAANIMATION); on when it cannot be read.
pub fn system_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut on = windows::Win32::Foundation::BOOL(1);
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
    ok.is_err() || on.as_bool()
}

/// 0..1 progress to 0..1 position, fast start and soft landing (cubic ease-out).
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Progress of an animation that started at `start` and lasts `ms`.
pub fn progress(start: std::time::Instant, ms: u64) -> f32 {
    (start.elapsed().as_secs_f32() * 1000.0 / ms as f32).min(1.0)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn mix_opt(a: Option<f32>, b: Option<f32>, t: f32) -> Option<f32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(mix(a, b, t)),
        // a reading that appears or goes away does not glide
        (_, b) => b,
    }
}

/// The current values the strip shows, the part of a sample that glides. Histories (the
/// charts) and everything in the panels stay exactly as measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Glide {
    cpu_total: f32,
    cpu_cores: Vec<f32>,
    mem_pct: f32,
    mem_used: f64,
    gpu: Option<f32>,
    net_rx: f64,
    net_tx: f64,
    disk_read: f64,
    disk_write: f64,
    battery: Option<f32>,
    cpu_temp: Option<f32>,
    gpu_temp: Option<f32>,
}

impl Glide {
    pub fn of(m: &MetricsData) -> Glide {
        Glide {
            cpu_total: m.cpu_total,
            cpu_cores: m.cpu_cores.clone(),
            mem_pct: m.mem.pct,
            mem_used: m.mem.used as f64,
            gpu: m.gpu,
            net_rx: m.net_rx,
            net_tx: m.net_tx,
            disk_read: m.disk_read,
            disk_write: m.disk_write,
            battery: m.battery.as_ref().map(|b| b.level),
            cpu_temp: m.cpu_temp,
            gpu_temp: m.gpu_temp,
        }
    }

    /// `t` of the way from `a` to `b` (0: a, 1: b).
    pub fn between(a: &Glide, b: &Glide, t: f32) -> Glide {
        let d = t as f64;
        // a different core count (a new sample shape) does not glide
        let cores = if a.cpu_cores.len() == b.cpu_cores.len() {
            a.cpu_cores.iter().zip(&b.cpu_cores).map(|(x, y)| mix(*x, *y, t)).collect()
        } else {
            b.cpu_cores.clone()
        };
        Glide {
            cpu_total: mix(a.cpu_total, b.cpu_total, t),
            cpu_cores: cores,
            mem_pct: mix(a.mem_pct, b.mem_pct, t),
            mem_used: a.mem_used + (b.mem_used - a.mem_used) * d,
            gpu: mix_opt(a.gpu, b.gpu, t),
            net_rx: a.net_rx + (b.net_rx - a.net_rx) * d,
            net_tx: a.net_tx + (b.net_tx - a.net_tx) * d,
            disk_read: a.disk_read + (b.disk_read - a.disk_read) * d,
            disk_write: a.disk_write + (b.disk_write - a.disk_write) * d,
            battery: mix_opt(a.battery, b.battery, t),
            cpu_temp: mix_opt(a.cpu_temp, b.cpu_temp, t),
            gpu_temp: mix_opt(a.gpu_temp, b.gpu_temp, t),
        }
    }

    /// Put these values into a copy of the sample for drawing the strip.
    pub fn apply(&self, m: &mut MetricsData) {
        m.cpu_total = self.cpu_total;
        if self.cpu_cores.len() == m.cpu_cores.len() {
            m.cpu_cores.clone_from(&self.cpu_cores);
        }
        m.mem.pct = self.mem_pct;
        m.mem.used = self.mem_used.max(0.0).round() as u64;
        m.gpu = self.gpu;
        m.net_rx = self.net_rx.max(0.0);
        m.net_tx = self.net_tx.max(0.0);
        m.disk_read = self.disk_read.max(0.0);
        m.disk_write = self.disk_write.max(0.0);
        if let (Some(level), Some(b)) = (self.battery, m.battery.as_mut()) {
            b.level = level;
        }
        m.cpu_temp = self.cpu_temp;
        m.gpu_temp = self.gpu_temp;
    }
}

/// A glide in progress: from what was on screen to the newest sample.
pub struct Glider {
    from: Glide,
    to: Glide,
    start: std::time::Instant,
}

impl Glider {
    /// Start from what the strip shows now (`shown`, mid-glide or not) to `to`; None when there
    /// is nothing to move.
    pub fn start(shown: Glide, to: Glide) -> Option<Glider> {
        (shown != to).then(|| Glider { from: shown, to, start: std::time::Instant::now() })
    }

    /// Values for this frame and whether the glide is over.
    pub fn frame(&self) -> (Glide, bool) {
        let p = progress(self.start, GLIDE_MS);
        (Glide::between(&self.from, &self.to, ease_out(p)), p >= 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_starts_fast_and_lands_softly() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(-1.0), 0.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.25) > 0.5, "most of the way in the first quarter");
        let mut last = 0.0;
        for i in 1..=20 {
            let v = ease_out(i as f32 / 20.0);
            assert!(v > last);
            last = v;
        }
    }

    fn sample(cpu: f32, cores: &[f32], gpu: Option<f32>, rx: f64) -> MetricsData {
        let mut m = MetricsData { cpu_total: cpu, cpu_cores: cores.to_vec(), gpu, net_rx: rx, ..Default::default() };
        m.mem.pct = cpu / 2.0;
        m
    }

    #[test]
    fn values_glide_between_samples() {
        let a = Glide::of(&sample(10.0, &[0.0, 20.0], Some(4.0), 1000.0));
        let b = Glide::of(&sample(50.0, &[40.0, 60.0], Some(8.0), 3000.0));
        assert_eq!(Glide::between(&a, &b, 0.0), a);
        assert_eq!(Glide::between(&a, &b, 1.0), b);
        let mid = Glide::between(&a, &b, 0.5);
        let mut m = sample(50.0, &[40.0, 60.0], Some(8.0), 3000.0);
        mid.apply(&mut m);
        assert_eq!(m.cpu_total, 30.0);
        assert_eq!(m.cpu_cores, vec![20.0, 40.0]);
        assert_eq!(m.gpu, Some(6.0));
        assert_eq!(m.net_rx, 2000.0);
        assert_eq!(m.mem.pct, 15.0);
    }

    #[test]
    fn changes_of_shape_do_not_glide() {
        let a = Glide::of(&sample(10.0, &[0.0, 20.0], None, 0.0));
        let b = Glide::of(&sample(50.0, &[40.0, 60.0, 80.0], Some(8.0), 0.0));
        let mid = Glide::between(&a, &b, 0.5);
        assert_eq!(mid.cpu_cores, vec![40.0, 60.0, 80.0]);
        assert_eq!(mid.gpu, Some(8.0), "a GPU reading that appears shows at once");
        // a copy of the sample with another core count keeps its own cores
        let mut m = sample(0.0, &[1.0], None, 0.0);
        mid.apply(&mut m);
        assert_eq!(m.cpu_cores, vec![1.0]);
    }

    #[test]
    fn nothing_to_move_means_no_glide() {
        let a = Glide::of(&sample(10.0, &[5.0], Some(1.0), 10.0));
        assert!(Glider::start(a.clone(), a.clone()).is_none());
        let b = Glide::of(&sample(11.0, &[5.0], Some(1.0), 10.0));
        let g = Glider::start(a.clone(), b).unwrap();
        let (now, done) = g.frame();
        assert!(!done);
        assert!(now.cpu_total >= 10.0 && now.cpu_total <= 11.0);
    }
}
