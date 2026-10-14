//! Threshold notifications as a small card in the corner above the taskbar, drawn like the
//! popups. Self-drawn rather than a Windows toast: unpackaged apps need a registered app id and
//! a Start menu shortcut for those, and this works the same on every machine.

use crate::canvas::{Canvas, Color, FontSpec, FW_NORMAL, FW_SEMIBOLD};
use crate::util::{pcwstr, wide};
use crate::widgets::Theme;
use windows::Win32::Foundation::{HINSTANCE, HWND, POINT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::WindowsAndMessaging::*;

/// timer that hides the card
pub const TOAST_TIMER: usize = 9;
const SHOW_MS: u32 = 8000;
const W_DIP: f32 = 330.0;

pub struct Toast {
    pub hwnd: HWND,
    canvas: Canvas,
    pub visible: bool,
}

impl Toast {
    pub fn new(hinstance: HINSTANCE, class: &str) -> Option<Self> {
        let cls = wide(class);
        let title = wide("Windows Stats notification");
        let hwnd = unsafe {
            CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, pcwstr(&cls), pcwstr(&title), WS_POPUP, 0, 0, 10, 10, None, None, hinstance, None).ok()?
        };
        Some(Self { hwnd, canvas: Canvas::new(), visible: false })
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                let _ = KillTimer(self.hwnd, TOAST_TIMER);
            }
            self.visible = false;
        }
    }

    /// Show `title` / `body` in the bottom-right corner of the primary monitor's work area.
    pub fn show(&mut self, t: &Theme, light: bool, title: &str, body: &str) {
        let w = t.s(W_DIP);
        let pad = t.s(14.0);
        let tf = FontSpec { px: t.s(13.0), weight: FW_SEMIBOLD };
        let bf = FontSpec { px: t.s(12.0), weight: FW_NORMAL };
        // word-wrap the body to the card width
        let max_w = w - pad * 2 - t.s(18.0);
        let mut lines: Vec<String> = Vec::new();
        let mut line = String::new();
        for word in body.split(' ') {
            let cand = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if line.is_empty() || self.canvas.measure_text(&cand, bf).0 <= max_w {
                line = cand;
            } else {
                lines.push(std::mem::replace(&mut line, word.to_string()));
            }
        }
        lines.push(line);
        let th = self.canvas.measure_text(title, tf).1;
        let lh = self.canvas.measure_text("X", bf).1;
        let h = pad * 2 + th + t.s(4.0) + lh * lines.len() as i32;
        self.canvas.resize(w, h);
        self.canvas.clear();
        let r = t.s(8.0) as f32;
        let bg = if light { Color::rgba(0xF9, 0xF9, 0xF9, 250) } else { Color::rgba(0x2B, 0x2B, 0x2B, 250) };
        self.canvas.fill_round_rect(0, 0, w, h, r, bg);
        self.canvas.stroke_round_rect_r(0, 0, w, h, r, t.dim.alpha(0.35));
        // accent mark on the left
        self.canvas.fill_round_rect(pad, pad + t.s(2.0), t.s(4.0), h - pad * 2 - t.s(4.0), t.s(2.0) as f32, Color::rgb(0xFF, 0x98, 0x00));
        let x = pad + t.s(14.0);
        self.canvas.draw_text(title, x, pad, tf, t.text);
        let mut y = pad + th + t.s(4.0);
        for l in &lines {
            self.canvas.draw_text(l, x, y, bf, t.dim);
            y += lh;
        }
        unsafe {
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY), &mut mi);
            let work = mi.rcWork;
            let margin = t.s(12.0);
            let (px, py) = (work.right - w - margin, work.bottom - h - margin);
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, px, py, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            if let Err(e) = self.canvas.present_at(self.hwnd, px, py) {
                crate::log!("toast UpdateLayeredWindow failed: {e}");
            }
            SetTimer(self.hwnd, TOAST_TIMER, SHOW_MS, None);
        }
        self.visible = true;
    }
}
