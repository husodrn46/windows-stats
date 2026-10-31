//! Premultiplied BGRA framebuffer with simple primitives, DirectWrite text (GDI as a fallback),
//! and presentation through UpdateLayeredWindow (per-pixel alpha).

use crate::util::{pcwstr, wide};
use std::collections::HashMap;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub fn alpha(self, a: f32) -> Self {
        Self { a: (self.a as f32 * a.clamp(0.0, 1.0)).round() as u8, ..self }
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 && s.len() != 8 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        if s.len() == 6 {
            Some(Self::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
        } else {
            Some(Self::rgba((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8))
        }
    }
}

pub const WHITE: Color = Color::rgb(255, 255, 255);
pub const GREEN: Color = Color::rgb(0x4C, 0xAF, 0x50);
pub const YELLOW: Color = Color::rgb(0xFF, 0xC1, 0x07);
pub const ORANGE: Color = Color::rgb(0xFF, 0x98, 0x00);
pub const RED: Color = Color::rgb(0xF4, 0x43, 0x36);

/// Colour for a 0..1 utilisation value: base below 0.6, then yellow, orange, red.
pub fn usage_color(v: f32, base: Color) -> Color {
    if v < 0.6 {
        base
    } else if v < 0.75 {
        YELLOW
    } else if v < 0.9 {
        ORANGE
    } else {
        RED
    }
}

pub use crate::dwtext::FontSpec;
use std::cell::RefCell;

thread_local! {
    /// One DirectWrite engine (and glyph cache) for every canvas of the UI thread; None when
    /// DirectWrite is unavailable, then each canvas falls back to GDI.
    static DW: RefCell<Option<crate::dwtext::TextEngine>> = RefCell::new(crate::dwtext::TextEngine::new());
    /// Font family of the GDI fallback (Font setting, empty = Segoe UI).
    static GDI_FAMILY: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Use `name` for all text of this thread (empty = the default). Returns false when the family
/// is not installed and the default is used instead.
pub fn set_font_family(name: &str) -> bool {
    GDI_FAMILY.with(|f| *f.borrow_mut() = name.to_string());
    DW.with(|e| e.borrow_mut().as_mut().map(|e| e.set_family(name))).unwrap_or(true)
}

/// Whether a font family is installed (DirectWrite; assumed true when it is unavailable).
pub fn font_installed(name: &str) -> bool {
    DW.with(|e| e.borrow().as_ref().map(|e| e.has_family(name))).unwrap_or(true)
}

pub const FW_NORMAL: i32 = 400;
pub const FW_SEMIBOLD: i32 = 600;

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    /// premultiplied BGRA
    px: Vec<u32>,
    text: TextRenderer,
    dib: Option<Dib>,
}

impl Canvas {
    pub fn new() -> Self {
        Self { w: 0, h: 0, px: Vec::new(), text: TextRenderer::new(), dib: None }
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        let w = w.max(1);
        let h = h.max(1);
        if w != self.w || h != self.h {
            self.w = w;
            self.h = h;
            self.px = vec![0; (w * h) as usize];
            self.dib = None;
        }
    }

    pub fn clear(&mut self) {
        self.px.iter_mut().for_each(|p| *p = 0);
    }

    /// The pixels as a 32-bit BMP (premultiplied BGRA, alpha kept), for debugging.
    pub fn save_bmp(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(54 + self.px.len() * 4);
        let size = (self.px.len() * 4) as u32;
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(54 + size).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&self.w.to_le_bytes());
        out.extend_from_slice(&(-self.h).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        for p in &self.px {
            out.extend_from_slice(&p.to_le_bytes());
        }
        std::fs::write(path, out)
    }

    #[inline]
    fn blend_px(&mut self, x: i32, y: i32, c: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let a = (c.a as f32 / 255.0) * coverage;
        if a <= 0.0 {
            return;
        }
        let idx = (y * self.w + x) as usize;
        let d = self.px[idx];
        let db = (d & 0xFF) as f32;
        let dg = ((d >> 8) & 0xFF) as f32;
        let dr = ((d >> 16) & 0xFF) as f32;
        let da = ((d >> 24) & 0xFF) as f32;
        let inv = 1.0 - a;
        let nb = c.b as f32 * a + db * inv;
        let ng = c.g as f32 * a + dg * inv;
        let nr = c.r as f32 * a + dr * inv;
        let na = 255.0 * a + da * inv;
        self.px[idx] = (na.round() as u32) << 24
            | (nr.round() as u32) << 16
            | (ng.round() as u32) << 8
            | (nb.round() as u32);
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                self.blend_px(xx, yy, c, 1.0);
            }
        }
    }

    /// Fill a bar growing up from `bottom` (exclusive) with fractional height:
    /// the top-most partial pixel row gets partial coverage.
    pub fn fill_bar(&mut self, x: i32, bottom: i32, w: i32, h: f32, c: Color) {
        if h <= 0.0 {
            return;
        }
        let full = h.floor() as i32;
        let frac = h - full as f32;
        self.fill_rect(x, bottom - full, w, full, c);
        if frac > 0.02 {
            let yy = bottom - full - 1;
            for xx in x.max(0)..(x + w).min(self.w) {
                self.blend_px(xx, yy, c, frac);
            }
        }
    }

    /// 1px outline with softened corners.
    pub fn stroke_round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        if w < 3 || h < 3 {
            return;
        }
        let x2 = x + w - 1;
        let y2 = y + h - 1;
        for xx in (x + 1)..x2 {
            self.blend_px(xx, y, c, 1.0);
            self.blend_px(xx, y2, c, 1.0);
        }
        for yy in (y + 1)..y2 {
            self.blend_px(x, yy, c, 1.0);
            self.blend_px(x2, yy, c, 1.0);
        }
        for (cx, cy) in [(x, y), (x2, y), (x, y2), (x2, y2)] {
            self.blend_px(cx, cy, c, 0.35);
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, c: Color) {
        let x0 = (cx - r - 1.0).floor() as i32;
        let x1 = (cx + r + 1.0).ceil() as i32;
        let y0 = (cy - r - 1.0).floor() as i32;
        let y1 = (cy + r + 1.0).ceil() as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (r - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(x, y, c, cov);
                }
            }
        }
    }

    /// Area chart in the style of Stats: an antialiased line over a fill that fades towards the
    /// baseline. `values` are 0..1, right-aligned (newest at the right edge), resampled to the
    /// columns like `line_chart`. `fill` carries the colour and opacity right under the line;
    /// at the baseline a fifth of it is left. With `down` the chart hangs from the top edge
    /// (the lower half of a mirrored chart).
    pub fn area_chart(&mut self, x: i32, y: i32, w: i32, h: i32, values: &[f32], line: Color, fill: Color, line_w: f32, down: bool) {
        if w <= 1 || h <= 0 || values.is_empty() {
            return;
        }
        let pts = chart_points(x, y, w, h, values, down);
        if pts.is_empty() {
            return;
        }
        let base = if down { y as f32 } else { (y + h) as f32 };
        // fill, column by column, from the line to the baseline
        for &(px, py) in &pts {
            let col = (px - 0.5).round() as i32;
            let span = (base - py).abs().max(1.0);
            let (from, to) = if down { (y, py.ceil() as i32) } else { (py.floor() as i32, y + h) };
            for yy in from.max(y)..to.min(y + h) {
                let (top, bot) = (yy as f32, yy as f32 + 1.0);
                // part of the pixel on the filled side of the line
                let cov = if down { (py - top).clamp(0.0, 1.0) } else { (bot - py).clamp(0.0, 1.0) };
                let dist = if down { (py - bot).max(0.0) } else { (top - py).max(0.0) };
                let fade = 1.0 - 0.8 * (dist / span).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(col, yy, fill, cov * fade);
                }
            }
        }
        self.polyline(&pts, line, line_w, y, y + h);
    }

    /// Mirrored chart for a pair of rates (Stats' network chart): `up` grows upwards from the
    /// middle, `down` downwards, each half scaled to its own maximum (at least `floor`).
    pub fn mirror_chart(&mut self, x: i32, y: i32, w: i32, h: i32, up: &[f32], down: &[f32], cu: Color, cd: Color, floor: f32, line_w: f32) {
        let mid = h / 2;
        let norm = |v: &[f32]| -> Vec<f32> {
            let max = v.iter().cloned().fold(floor, f32::max);
            v.iter().map(|x| x / max).collect()
        };
        self.area_chart(x, y, w, mid, &norm(up), cu, cu.alpha(0.45), line_w, false);
        self.area_chart(x, y + h - mid, w, mid, &norm(down), cd, cd.alpha(0.45), line_w, true);
    }

    /// Antialiased polyline through `pts` (pixel-centre x, y), clipped to rows `y0..y1`: each
    /// pixel takes its distance to the nearest segment, so joints are not drawn twice.
    fn polyline(&mut self, pts: &[(f32, f32)], c: Color, width: f32, y0: i32, y1: i32) {
        let hw = width / 2.0;
        if pts.len() == 1 {
            let (px, py) = pts[0];
            self.draw_line(px - 0.5, py, px + 0.5, py, width, c);
            return;
        }
        let x_min = (pts[0].0 - hw - 1.0).floor() as i32;
        let x_max = (pts[pts.len() - 1].0 + hw + 1.0).ceil() as i32;
        let mut seg = 0usize;
        for xx in x_min..=x_max {
            let cx = xx as f32 + 0.5;
            // segments that can reach this column
            while seg + 1 < pts.len() - 1 && pts[seg + 1].0 < cx - hw - 1.0 {
                seg += 1;
            }
            let mut lo = f32::MAX;
            let mut hi = f32::MIN;
            let mut k = seg;
            while k + 1 < pts.len() && pts[k].0 <= cx + hw + 1.0 {
                lo = lo.min(pts[k].1.min(pts[k + 1].1));
                hi = hi.max(pts[k].1.max(pts[k + 1].1));
                k += 1;
            }
            if lo > hi {
                continue;
            }
            for yy in ((lo - hw - 1.0).floor() as i32).max(y0)..=((hi + hw + 1.0).ceil() as i32).min(y1 - 1) {
                let cy = yy as f32 + 0.5;
                let mut d = f32::MAX;
                let mut k = seg;
                while k + 1 < pts.len() && pts[k].0 <= cx + hw + 1.0 {
                    let (ax, ay) = pts[k];
                    let (bx, by) = pts[k + 1];
                    let (dx, dy) = (bx - ax, by - ay);
                    let len2 = dx * dx + dy * dy;
                    let t = if len2 > 0.0 { (((cx - ax) * dx + (cy - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                    let (qx, qy) = (ax + dx * t - cx, ay + dy * t - cy);
                    d = d.min((qx * qx + qy * qy).sqrt());
                    k += 1;
                }
                let cov = (hw + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(xx, yy, c, cov);
                }
            }
        }
    }

    /// Pie slice: angles in radians, 0 at 12 o'clock, clockwise; `start <= end`, end - start <= 2π.
    pub fn fill_pie(&mut self, cx: f32, cy: f32, r: f32, start: f32, end: f32, c: Color) {
        let x0 = (cx - r - 1.0).floor() as i32;
        let x1 = (cx + r + 1.0).ceil() as i32;
        let y0 = (cy - r - 1.0).floor() as i32;
        let y1 = (cy + r + 1.0).ceil() as i32;
        let tau = std::f32::consts::PI * 2.0;
        for y in y0..=y1 {
            for x in x0..=x1 {
                // 2x2 supersampling keeps the radial edges smooth
                let mut cov = 0.0;
                for (sx, sy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let dx = x as f32 + sx - cx;
                    let dy = y as f32 + sy - cy;
                    let d = (dx * dx + dy * dy).sqrt();
                    if d > r {
                        continue;
                    }
                    let mut a = dx.atan2(-dy);
                    if a < 0.0 {
                        a += tau;
                    }
                    if a >= start && a < end {
                        cov += 0.25;
                    }
                }
                if cov > 0.0 {
                    self.blend_px(x, y, c, cov);
                }
            }
        }
    }

    /// Antialiased filled rounded rectangle.
    pub fn fill_round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, c: Color) {
        let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0).max(0.0);
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                let cov = round_rect_coverage(xx, yy, x, y, w, h, r);
                if cov > 0.0 {
                    self.blend_px(xx, yy, c, cov);
                }
            }
        }
    }

    /// Soft drop shadow of a rounded rectangle fading out over `blur` px; only painted outside
    /// the rectangle itself, so a translucent card drawn on top is not darkened.
    pub fn fill_shadow(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, blur: f32, dy: i32, c: Color) {
        let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0).max(0.0);
        let (cx, cy) = (x as f32 + w as f32 / 2.0, (y + dy) as f32 + h as f32 / 2.0);
        let (hx, hy) = (w as f32 / 2.0 - r, h as f32 / 2.0 - r);
        let b = blur.ceil() as i32;
        for yy in (y + dy - b).max(0)..(y + dy + h + b).min(self.h) {
            for xx in (x - b).max(0)..(x + w + b).min(self.w) {
                let qx = (xx as f32 + 0.5 - cx).abs() - hx;
                let qy = (yy as f32 + 0.5 - cy).abs() - hy;
                let d = (qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r).max(0.0);
                let f = (1.0 - d / blur).clamp(0.0, 1.0);
                let cov = f * f * (1.0 - round_rect_coverage(xx, yy, x, y, w, h, r));
                if cov > 0.0 {
                    self.blend_px(xx, yy, c, cov);
                }
            }
        }
    }

    /// Antialiased 1px rounded outline.
    pub fn stroke_round_rect_r(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, c: Color) {
        let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0).max(0.0);
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                let outer = round_rect_coverage(xx, yy, x, y, w, h, r);
                let inner = round_rect_coverage(xx, yy, x + 1, y + 1, w - 2, h - 2, (r - 1.0).max(0.0));
                let cov = (outer - inner).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(xx, yy, c, cov);
                }
            }
        }
    }

    /// Composite another (premultiplied) canvas onto this one at (x, y).
    pub fn blit(&mut self, src: &Canvas, x: i32, y: i32) {
        for sy in 0..src.h {
            let dy = y + sy;
            if dy < 0 || dy >= self.h {
                continue;
            }
            for sx in 0..src.w {
                let dx = x + sx;
                if dx < 0 || dx >= self.w {
                    continue;
                }
                let s = src.px[(sy * src.w + sx) as usize];
                let sa = (s >> 24) & 0xFF;
                if sa == 0 {
                    continue;
                }
                let idx = (dy * self.w + dx) as usize;
                let d = self.px[idx];
                let inv = 255 - sa;
                let ch = |shift: u32| ((s >> shift) & 0xFF) + ((d >> shift) & 0xFF) * inv / 255;
                self.px[idx] = ch(24).min(255) << 24 | ch(16).min(255) << 16 | ch(8).min(255) << 8 | ch(0).min(255);
            }
        }
    }

    /// Draw premultiplied BGRA pixels (`sw` x `sh`) scaled to `w` x `h` at (x, y): bilinear when
    /// shrinking or growing a little, which is what 32 px icons drawn at 24 px need.
    pub fn draw_image(&mut self, src: &[u32], sw: i32, sh: i32, x: i32, y: i32, w: i32, h: i32) {
        if sw <= 0 || sh <= 0 || w <= 0 || h <= 0 || src.len() < (sw * sh) as usize {
            return;
        }
        for dy in 0..h {
            let ty = y + dy;
            if ty < 0 || ty >= self.h {
                continue;
            }
            let fy = ((dy as f32 + 0.5) * sh as f32 / h as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
            let y0 = fy.floor() as i32;
            let y1 = (y0 + 1).min(sh - 1);
            let wy = fy - y0 as f32;
            for dx in 0..w {
                let tx = x + dx;
                if tx < 0 || tx >= self.w {
                    continue;
                }
                let fx = ((dx as f32 + 0.5) * sw as f32 / w as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
                let x0 = fx.floor() as i32;
                let x1 = (x0 + 1).min(sw - 1);
                let wx = fx - x0 as f32;
                let sample = |shift: u32| -> f32 {
                    let p = |xx: i32, yy: i32| ((src[(yy * sw + xx) as usize] >> shift) & 0xFF) as f32;
                    let top = p(x0, y0) * (1.0 - wx) + p(x1, y0) * wx;
                    let bot = p(x0, y1) * (1.0 - wx) + p(x1, y1) * wx;
                    top * (1.0 - wy) + bot * wy
                };
                let sa = sample(24);
                if sa <= 0.5 {
                    continue;
                }
                let (sr, sg, sb) = (sample(16), sample(8), sample(0));
                let idx = (ty * self.w + tx) as usize;
                let d = self.px[idx];
                let inv = 1.0 - sa / 255.0;
                let ch = |s: f32, shift: u32| (s + ((d >> shift) & 0xFF) as f32 * inv).round().min(255.0) as u32;
                self.px[idx] = ch(sa, 24) << 24 | ch(sr, 16) << 16 | ch(sg, 8) << 8 | ch(sb, 0);
            }
        }
    }

    /// Antialiased straight line of `width` px between two points.
    pub fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Color) {
        let hw = width / 2.0;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        let minx = (x0.min(x1) - hw - 1.0).floor() as i32;
        let maxx = (x0.max(x1) + hw + 1.0).ceil() as i32;
        let miny = (y0.min(y1) - hw - 1.0).floor() as i32;
        let maxy = (y0.max(y1) + hw + 1.0).ceil() as i32;
        for y in miny..=maxy {
            for x in minx..=maxx {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = if len2 > 0.0 { (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let (cx, cy) = (x0 + dx * t, y0 + dy * t);
                let d = ((px - cx) * (px - cx) + (py - cy) * (py - cy)).sqrt();
                let cov = (hw + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_px(x, y, c, cov);
                }
            }
        }
    }

    /// Height of the last non-transparent row + 1 (0 when empty).
    pub fn used_height(&self) -> i32 {
        for y in (0..self.h).rev() {
            let row = &self.px[(y * self.w) as usize..((y + 1) * self.w) as usize];
            if row.iter().any(|p| p >> 24 != 0) {
                return y + 1;
            }
        }
        0
    }

    /// Draw text truncated with an ellipsis to fit `max_w`.
    pub fn draw_text_clipped(&mut self, text: &str, x: i32, y: i32, max_w: i32, font: FontSpec, c: Color) -> (i32, i32) {
        if self.measure_text(text, font).0 <= max_w {
            return self.draw_text(text, x, y, font, c);
        }
        let chars: Vec<char> = text.chars().collect();
        let mut n = chars.len();
        while n > 1 {
            n -= 1;
            let s: String = chars[..n].iter().collect::<String>().trim_end().to_string() + "…";
            if self.measure_text(&s, font).0 <= max_w {
                return self.draw_text(&s, x, y, font, c);
            }
        }
        (0, 0)
    }

    pub fn measure_text(&mut self, text: &str, font: FontSpec) -> (i32, i32) {
        if let Some(m) = DW.with(|e| e.borrow_mut().as_mut().map(|e| e.measure(text, font))) {
            return m;
        }
        self.text.measure(text, font)
    }

    /// Draw text with its top-left corner at (x, y). Returns the layout size (advance width,
    /// line height), the same as `measure_text`.
    pub fn draw_text(&mut self, text: &str, x: i32, y: i32, font: FontSpec, c: Color) -> (i32, i32) {
        let dw = DW.with(|e| e.borrow_mut().as_mut().map(|e| (e.measure(text, font), e.render(text, font))));
        if let Some((size, mask)) = dw {
            if let Some(m) = mask {
                // coverage -> alpha the way Direct2D shades grayscale text (contrast + gamma)
                let mut lut = [0f32; 256];
                for (i, v) in lut.iter_mut().enumerate() {
                    *v = crate::dwtext::text_alpha(i as f32 / 255.0, c.r, c.g, c.b);
                }
                for gy in 0..m.mh {
                    for gx in 0..m.mw {
                        let cov = m.cov[(gy * m.mw + gx) as usize];
                        if cov > 0 {
                            self.blend_px(x + m.x0 + gx, y + m.y0 + gy, c, lut[cov as usize]);
                        }
                    }
                }
            }
            return size;
        }
        let Some(g) = self.text.render(text, font) else { return (0, 0) };
        for gy in 0..g.h {
            for gx in 0..g.w {
                let cov = g.cov[(gy * g.w + gx) as usize] as f32 / 255.0;
                if cov > 0.0 {
                    self.blend_px(x + gx, y + gy, c, cov);
                }
            }
        }
        (g.w, g.h)
    }

    /// Push the framebuffer to a layered window (keeps the window position).
    pub fn present(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        self.present_impl(hwnd, None)
    }

    /// Push the framebuffer to a top-level layered window and move it to screen (x, y).
    pub fn present_at(&mut self, hwnd: HWND, x: i32, y: i32) -> windows::core::Result<()> {
        self.present_impl(hwnd, Some(POINT { x, y }))
    }

    /// Copy the framebuffer, alpha included, onto a window DC (a non-layered window whose
    /// client area DWM composites with that alpha over a backdrop).
    pub fn paint_dc(&mut self, hdc: HDC) {
        if self.dib.as_ref().map(|d| d.w != self.w || d.h != self.h).unwrap_or(true) {
            match Dib::new(self.w, self.h) {
                Ok(d) => self.dib = Some(d),
                Err(_) => return,
            }
        }
        let dib = self.dib.as_mut().unwrap();
        dib.copy_from(&self.px);
        unsafe {
            let _ = BitBlt(hdc, 0, 0, self.w, self.h, dib.hdc, 0, 0, SRCCOPY);
        }
    }

    fn present_impl(&mut self, hwnd: HWND, dst: Option<POINT>) -> windows::core::Result<()> {
        if self.dib.as_ref().map(|d| d.w != self.w || d.h != self.h).unwrap_or(true) {
            self.dib = Some(Dib::new(self.w, self.h)?);
        }
        let dib = self.dib.as_mut().unwrap();
        dib.copy_from(&self.px);
        let size = SIZE { cx: self.w, cy: self.h };
        let src = POINT { x: 0, y: 0 };
        // AC_SRC_OVER = 0, AC_SRC_ALPHA = 1
        let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                dst.as_ref().map(|p| p as *const POINT),
                Some(&size),
                dib.hdc,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }
}

/// Points of a chart: one per column, at the column centre, newest at the right edge. Fewer
/// samples than columns are stretched across the width (linear interpolation); more are cut to
/// the last `w`. y is the line's height for the value (from the bottom, or from the top `down`).
fn chart_points(x: i32, y: i32, w: i32, h: i32, values: &[f32], down: bool) -> Vec<(f32, f32)> {
    let n = values.len();
    let col_y = |v: f32| {
        let v = v.clamp(0.0, 1.0) * h as f32;
        if down { y as f32 + v } else { (y + h) as f32 - v }
    };
    if n >= 2 && (n as i32) < w {
        return (0..w)
            .map(|i| {
                let pos = i as f32 * (n - 1) as f32 / (w - 1).max(1) as f32;
                let a = pos.floor() as usize;
                let b = (a + 1).min(n - 1);
                let t = pos - a as f32;
                ((x + i) as f32 + 0.5, col_y(values[a] * (1.0 - t) + values[b] * t))
            })
            .collect();
    }
    let take = n.min(w as usize);
    let x0 = x + w - take as i32;
    values[n - take..].iter().enumerate().map(|(i, v)| ((x0 + i as i32) as f32 + 0.5, col_y(*v))).collect()
}

/// Pixel coverage (0..1) of a rounded rectangle, evaluated at the pixel centre with a 1px-wide AA ramp.
fn round_rect_coverage(px: i32, py: i32, x: i32, y: i32, w: i32, h: i32, r: f32) -> f32 {
    if w <= 0 || h <= 0 {
        return 0.0;
    }
    let cx = px as f32 + 0.5;
    let cy = py as f32 + 0.5;
    let left = x as f32;
    let top = y as f32;
    let right = (x + w) as f32;
    let bottom = (y + h) as f32;
    if cx < left || cx > right || cy < top || cy > bottom {
        return 0.0;
    }
    // distance to the nearest corner circle centre, only matters inside the corner squares
    let qx = if cx < left + r { left + r } else if cx > right - r { right - r } else { cx };
    let qy = if cy < top + r { top + r } else if cy > bottom - r { bottom - r } else { cy };
    let dx = cx - qx;
    let dy = cy - qy;
    if dx == 0.0 || dy == 0.0 {
        return 1.0;
    }
    let d = (dx * dx + dy * dy).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

/// A 32bpp top-down DIB section selected into a memory DC.
struct Dib {
    w: i32,
    h: i32,
    hdc: HDC,
    hbm: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
}

impl Dib {
    fn new(w: i32, h: i32) -> windows::core::Result<Self> {
        unsafe {
            let hdc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let hbm = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(hdc, hbm);
            Ok(Self { w, h, hdc, hbm, old, bits: bits as *mut u32 })
        }
    }
    fn copy_from(&mut self, px: &[u32]) {
        unsafe {
            std::ptr::copy_nonoverlapping(px.as_ptr(), self.bits, (self.w * self.h) as usize);
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.old);
            let _ = DeleteObject(self.hbm);
            let _ = DeleteDC(self.hdc);
        }
    }
}

struct Glyphs {
    w: i32,
    h: i32,
    cov: Vec<u8>,
}

/// Renders text through GDI (grayscale antialiasing) into a coverage mask.
struct TextRenderer {
    hdc: HDC,
    fonts: HashMap<(FontSpec, String), HFONT>,
}

impl TextRenderer {
    fn new() -> Self {
        Self { hdc: unsafe { CreateCompatibleDC(None) }, fonts: HashMap::new() }
    }

    fn font(&mut self, spec: FontSpec) -> HFONT {
        let family = GDI_FAMILY.with(|f| f.borrow().clone());
        *self.fonts.entry((spec, family.clone())).or_insert_with(|| unsafe {
            let face = wide(if family.is_empty() { "Segoe UI" } else { &family });
            CreateFontW(
                -spec.px,
                0,
                0,
                0,
                spec.weight,
                0,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_TT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                // Segoe UI's gasp table forbids grayscale AA at small sizes (text came out jagged);
                // ClearType is allowed at every size, and we average its RGB coverage below.
                CLEARTYPE_QUALITY.0 as u32,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                pcwstr(&face),
            )
        })
    }

    fn measure(&mut self, text: &str, spec: FontSpec) -> (i32, i32) {
        let font = self.font(spec);
        let t: Vec<u16> = text.encode_utf16().collect();
        let mut sz = SIZE::default();
        unsafe {
            let old = SelectObject(self.hdc, font);
            let _ = GetTextExtentPoint32W(self.hdc, &t, &mut sz);
            SelectObject(self.hdc, old);
        }
        (sz.cx, sz.cy)
    }

    fn render(&mut self, text: &str, spec: FontSpec) -> Option<Glyphs> {
        let (w, h) = self.measure(text, spec);
        if w <= 0 || h <= 0 {
            return None;
        }
        let font = self.font(spec);
        let dib = Dib::new(w, h).ok()?;
        let mut t: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            std::ptr::write_bytes(dib.bits, 0, (w * h) as usize);
            let old = SelectObject(dib.hdc, font);
            SetTextColor(dib.hdc, COLORREF(0x00FF_FFFF));
            SetBkMode(dib.hdc, TRANSPARENT);
            let mut rc = RECT { left: 0, top: 0, right: w, bottom: h };
            DrawTextW(dib.hdc, &mut t, &mut rc, DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(dib.hdc, old);
            let src = std::slice::from_raw_parts(dib.bits, (w * h) as usize);
            let cov = src
                .iter()
                .map(|p| {
                    let b = *p & 0xFF;
                    let g = (*p >> 8) & 0xFF;
                    let r = (*p >> 16) & 0xFF;
                    ((r + g + b) / 3) as u8
                })
                .collect();
            Some(Glyphs { w, h, cov })
        }
    }
}

impl Drop for TextRenderer {
    fn drop(&mut self) {
        unsafe {
            for (_, f) in self.fonts.drain() {
                let _ = DeleteObject(f);
            }
            let _ = DeleteDC(self.hdc);
        }
    }
}
