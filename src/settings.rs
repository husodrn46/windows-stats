//! The settings window: a navigation column on the left, cards of settings on the right, in
//! the look of the Windows 11 Settings app. Drawn with the same canvas as the strip (DirectWrite
//! text, antialiased shapes) in immediate mode: every paint lays the page out again and records
//! the clickable areas with what a click does (`Effect`); `App::apply_settings` carries it out,
//! saves the config and refreshes the strip, so a change is visible at once.

use crate::alerts::Metric;
use crate::canvas::{Canvas, Color, FontSpec, FW_NORMAL, FW_SEMIBOLD};
use crate::config::Config;
use crate::i18n::{t, tf};
use crate::metrics::MetricsData;
use crate::util::{pcwstr, wide};
use crate::widgets::{Kind, Theme};
use windows::Win32::Foundation::{HINSTANCE, HWND, RECT};
use windows::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::*;

const W_DIP: f32 = 900.0;
const H_DIP: f32 = 640.0;
const NAV_DIP: f32 = 200.0;

/// What a click on a control does.
pub enum Effect {
    Page(usize),
    /// change the config (saved and applied by the app)
    Cfg(Box<dyn Fn(&mut Config)>),
    Autostart(bool),
    /// open a file, folder or URL with the shell
    Open(String),
    /// pick the language from a menu at the pointer
    LanguageMenu,
}

struct Hit {
    rect: RECT,
    effect: Effect,
}

pub struct Settings {
    pub hwnd: HWND,
    canvas: Canvas,
    pub visible: bool,
    pub page: usize,
    scroll: i32,
    /// content height of the last paint, for clamping the scroll
    content_h: i32,
    hits: Vec<Hit>,
    mouse: Option<(i32, i32)>,
}

pub const PAGES: &[&str] = &["settings.page_general", "settings.page_appearance", "settings.page_widgets", "settings.page_sensors", "settings.page_alerts", "settings.page_privacy", "settings.page_about"];

impl Settings {
    pub fn new(hinstance: HINSTANCE, class: &str) -> Option<Self> {
        let cls = wide(class);
        let title = wide(&t("settings.title"));
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                pcwstr(&cls),
                pcwstr(&title),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                10,
                10,
                None,
                None,
                hinstance,
                None,
            )
            .ok()?
        };
        // tests open a given page: WINDOWS_STATS_SETTINGS_PAGE=<0..5>
        let page = std::env::var("WINDOWS_STATS_SETTINGS_PAGE").ok().and_then(|v| v.parse().ok()).unwrap_or(0).min(PAGES.len() - 1);
        Some(Self { hwnd, canvas: Canvas::new(), visible: false, page, scroll: 0, content_h: 0, hits: Vec::new(), mouse: None })
    }

    fn dpi(&self) -> u32 {
        crate::util::dpi_for_window(self.hwnd).unwrap_or(96)
    }

    /// Bring the window up (centred on the primary work area the first time).
    pub fn show(&mut self, light: bool) {
        let s = self.dpi() as f32 / 96.0;
        unsafe {
            let title = wide(&t("settings.title"));
            let _ = SetWindowTextW(self.hwnd, pcwstr(&title));
            // dark title bar to match the content
            let dark: i32 = if light { 0 } else { 1 };
            let _ = DwmSetWindowAttribute(self.hwnd, windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(20), &dark as *const _ as *const _, 4);
            if !self.visible {
                let (w, h) = ((W_DIP * s) as i32, (H_DIP * s) as i32);
                let mut work = RECT::default();
                let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
                let x = work.left + (work.right - work.left - w) / 2;
                let y = work.top + (work.bottom - work.top - h) / 2;
                let _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOZORDER);
            }
            let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = InvalidateRect(self.hwnd, None, false);
        }
        self.visible = true;
    }

    pub fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.visible = false;
    }

    pub fn invalidate(&self) {
        if self.visible {
            unsafe {
                let _ = InvalidateRect(self.hwnd, None, false);
            }
        }
    }

    pub fn wheel(&mut self, delta: i32) {
        let step = (40.0 * self.dpi() as f32 / 96.0) as i32;
        self.scroll = (self.scroll - delta.signum() * step).max(0);
        self.invalidate();
    }

    /// Mouse moved (client coords): true when the hover changed.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let before = self.hit_index(self.mouse);
        self.mouse = Some((x, y));
        before != self.hit_index(self.mouse)
    }

    pub fn mouse_leave(&mut self) {
        self.mouse = None;
        self.invalidate();
    }

    fn hit_index(&self, at: Option<(i32, i32)>) -> Option<usize> {
        let (x, y) = at?;
        self.hits.iter().position(|h| x >= h.rect.left && x < h.rect.right && y >= h.rect.top && y < h.rect.bottom)
    }

    pub fn over_control(&self, x: i32, y: i32) -> bool {
        self.hit_index(Some((x, y))).is_some()
    }

    /// The effect of a click at (x, y), taken out of the hit list (it is rebuilt on the next paint).
    pub fn click(&mut self, x: i32, y: i32) -> Option<Effect> {
        let i = self.hit_index(Some((x, y)))?;
        let hit = self.hits.swap_remove(i);
        if let Effect::Page(p) = hit.effect {
            self.page = p;
            self.scroll = 0;
            self.invalidate();
            return None;
        }
        Some(hit.effect)
    }

    /// WM_PAINT: lay the page out with the current config and data, then copy it to the window.
    pub fn paint(&mut self, base: &Theme, cfg: &Config, m: &MetricsData, autostart: bool) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let (w, h) = ((rc.right - rc.left).max(1), (rc.bottom - rc.top).max(1));
        let mut th = base.clone();
        th.scale = self.dpi() as f32 / 96.0;
        self.canvas.resize(w, h);
        self.hits.clear();
        let light = th.light;
        let pal = Palette::new(light, th.accent);
        self.canvas.fill_rect(0, 0, w, h, pal.bg);

        // page content, scrolled
        let nav = th.s(NAV_DIP);
        let max_scroll = (self.content_h - h + th.s(24.0)).max(0);
        self.scroll = self.scroll.min(max_scroll);
        let hover = self.mouse;
        let mut ui = Ui { c: &mut self.canvas, t: &th, pal: &pal, x: nav + th.s(24.0), w: w - nav - th.s(48.0), y: th.s(20.0) - self.scroll, hits: &mut self.hits, mouse: hover, clip_top: 0 };
        match self.page {
            0 => page_general(&mut ui, cfg, autostart),
            1 => page_appearance(&mut ui, cfg),
            2 => page_widgets(&mut ui, cfg, m),
            3 => page_sensors(&mut ui, cfg, m),
            4 => page_alerts(&mut ui, cfg, m),
            5 => page_privacy(&mut ui, cfg),
            _ => page_about(&mut ui),
        }
        self.content_h = ui.y + self.scroll;

        // navigation column on top of everything scrolled under it
        self.canvas.fill_rect(0, 0, nav, h, pal.nav);
        let nf = FontSpec { px: th.s(13.0), weight: FW_NORMAL };
        let tf = FontSpec { px: th.s(18.0), weight: FW_SEMIBOLD };
        self.canvas.draw_text("Windows Stats", th.s(20.0), th.s(18.0), tf, th.text);
        let mut y = th.s(64.0);
        let item_h = th.s(36.0);
        for (i, key) in PAGES.iter().enumerate() {
            let r = RECT { left: th.s(8.0), top: y, right: nav - th.s(8.0), bottom: y + item_h };
            let hovered = hover.map_or(false, |(mx, my)| mx >= r.left && mx < r.right && my >= r.top && my < r.bottom);
            if i == self.page || hovered {
                let a = if i == self.page { 0.10 } else { 0.05 };
                self.canvas.fill_round_rect(r.left, r.top, r.right - r.left, item_h, th.s(5.0) as f32, th.text.alpha(a));
            }
            if i == self.page {
                self.canvas.fill_round_rect(r.left, y + th.s(10.0), th.s(3.0), item_h - th.s(20.0), th.s(1.5) as f32, th.accent);
            }
            let label = t(key);
            let lh = self.canvas.measure_text(&label, nf).1;
            self.canvas.draw_text(&label, r.left + th.s(14.0), y + (item_h - lh) / 2, nf, th.text);
            self.hits.push(Hit { rect: r, effect: Effect::Page(i) });
            y += item_h + th.s(2.0);
        }
        let vf = FontSpec { px: th.s(11.0), weight: FW_NORMAL };
        let ver = tf_ver();
        let vh = self.canvas.measure_text(&ver, vf).1;
        self.canvas.draw_text(&ver, th.s(20.0), h - vh - th.s(14.0), vf, th.dim);

        if let Some(dir) = std::env::var_os("WINDOWS_STATS_DEBUG_DUMP") {
            let _ = self.canvas.save_bmp(&std::path::Path::new(&dir).join(format!("settings-{}.bmp", self.page)));
        }
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            self.canvas.paint_dc(hdc);
            let _ = EndPaint(self.hwnd, &ps);
        }
    }
}

fn tf_ver() -> String {
    tf("about.version", &[("v", crate::about::VERSION)])
}

/// Colours of the window, close to the Windows 11 Settings app.
struct Palette {
    bg: Color,
    nav: Color,
    card: Color,
    card_border: Color,
    control: Color,
    accent: Color,
}

impl Palette {
    fn new(light: bool, accent: Color) -> Self {
        if light {
            Palette { bg: Color::rgb(0xF3, 0xF3, 0xF3), nav: Color::rgb(0xEE, 0xEE, 0xEE), card: Color::rgb(0xFF, 0xFF, 0xFF), card_border: Color::rgba(0, 0, 0, 22), control: Color::rgba(0, 0, 0, 14), accent }
        } else {
            Palette { bg: Color::rgb(0x20, 0x20, 0x20), nav: Color::rgb(0x1B, 0x1B, 0x1B), card: Color::rgb(0x2B, 0x2B, 0x2B), card_border: Color::rgba(255, 255, 255, 14), control: Color::rgba(255, 255, 255, 18), accent }
        }
    }
}

struct Ui<'a> {
    c: &'a mut Canvas,
    t: &'a Theme,
    pal: &'a Palette,
    /// left edge and width of the content column
    x: i32,
    w: i32,
    y: i32,
    hits: &'a mut Vec<Hit>,
    mouse: Option<(i32, i32)>,
    clip_top: i32,
}

impl Ui<'_> {
    fn f(&self, dip: f32, weight: i32) -> FontSpec {
        FontSpec { px: self.t.s(dip), weight }
    }

    fn hovered(&self, r: &RECT) -> bool {
        self.mouse.map_or(false, |(x, y)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
    }

    fn hit(&mut self, r: RECT, e: Effect) {
        if r.bottom > self.clip_top {
            self.hits.push(Hit { rect: r, effect: e });
        }
    }

    fn heading(&mut self, text: &str) {
        let f = self.f(22.0, FW_SEMIBOLD);
        let (_, h) = self.c.draw_text(text, self.x, self.y, f, self.t.text);
        self.y += h + self.t.s(14.0);
    }

    fn section(&mut self, text: &str) {
        self.y += self.t.s(8.0);
        let f = self.f(13.0, FW_SEMIBOLD);
        let (_, h) = self.c.draw_text(text, self.x, self.y, f, self.t.text);
        self.y += h + self.t.s(8.0);
    }

    fn note(&mut self, text: &str) {
        let f = self.f(12.0, FW_NORMAL);
        for line in wrap(self.c, text, f, self.w) {
            let (_, h) = self.c.draw_text(&line, self.x, self.y, f, self.t.dim);
            self.y += h;
        }
        self.y += self.t.s(10.0);
    }

    /// A card row: title (and a dimmer description) on the left; returns the y of the row and
    /// its height so the caller can place a control on the right.
    fn row(&mut self, title: &str, desc: &str) -> (i32, i32) {
        let cw = self.t.s(250.0).min(self.w / 2);
        self.row_ex(title, desc, cw, 0)
    }

    /// A row with `control_w` px kept free on the right for the text, and `extra_h` px more
    /// height below the text (a second line of controls); the text sits at the top then.
    fn row_ex(&mut self, title: &str, desc: &str, control_w: i32, extra_h: i32) -> (i32, i32) {
        let tf = self.f(13.0, FW_NORMAL);
        let df = self.f(11.5, FW_NORMAL);
        let pad = self.t.s(16.0);
        let text_w = self.w - pad * 2 - control_w;
        let th = self.c.measure_text(title, tf).1;
        let dlines = if desc.is_empty() { Vec::new() } else { wrap(self.c, desc, df, text_w) };
        let dh = self.c.measure_text("X", df).1;
        let h = (pad * 2 + th + dh * dlines.len() as i32).max(self.t.s(64.0)) + extra_h;
        let r = self.t.s(6.0) as f32;
        self.c.fill_round_rect(self.x, self.y, self.w, h, r, self.pal.card);
        self.c.stroke_round_rect_r(self.x, self.y, self.w, h, r, self.pal.card_border);
        let text_h = th + dh * dlines.len() as i32;
        let mut ty = if extra_h > 0 { self.y + pad } else { self.y + (h - text_h) / 2 };
        self.c.draw_text(title, self.x + pad, ty, tf, self.t.text);
        ty += th;
        for l in &dlines {
            self.c.draw_text(l, self.x + pad, ty, df, self.t.dim);
            ty += dh;
        }
        let top = self.y;
        self.y += h + self.t.s(4.0);
        (top, h)
    }

    /// Toggle switch at the right end of a row.
    fn toggle(&mut self, row: (i32, i32), on: bool, e: Effect) {
        let (tw, thh) = (self.t.s(40.0), self.t.s(20.0));
        let x = self.x + self.w - self.t.s(16.0) - tw;
        let y = row.0 + (row.1 - thh) / 2;
        let rad = thh as f32 / 2.0;
        let r = RECT { left: x - self.t.s(8.0), top: row.0, right: self.x + self.w, bottom: row.0 + row.1 };
        let hov = self.hovered(&r);
        if on {
            self.c.fill_round_rect(x, y, tw, thh, rad, if hov { lighten(self.pal.accent) } else { self.pal.accent });
            self.c.fill_circle((x + tw) as f32 - rad, y as f32 + rad, rad - self.t.s(4.0) as f32, Color::rgb(255, 255, 255));
        } else {
            self.c.stroke_round_rect_r(x, y, tw, thh, rad, self.t.dim);
            if hov {
                self.c.fill_round_rect(x, y, tw, thh, rad, self.pal.control);
            }
            self.c.fill_circle(x as f32 + rad, y as f32 + rad, rad - self.t.s(5.0) as f32, self.t.dim);
        }
        let label = t(if on { "settings.on" } else { "settings.off" });
        let f = self.f(12.5, FW_NORMAL);
        let (lw, lh) = self.c.measure_text(&label, f);
        self.c.draw_text(&label, x - self.t.s(10.0) - lw, y + (thh - lh) / 2, f, self.t.text);
        self.hit(r, e);
    }

    /// Segmented choice at the right end of a row; `effects[i]` runs when option i is clicked.
    fn segmented(&mut self, row: (i32, i32), options: &[String], selected: Option<usize>, effects: Vec<Effect>) {
        let f = self.f(12.5, FW_NORMAL);
        let pad = self.t.s(12.0);
        let hh = self.t.s(30.0);
        let widths: Vec<i32> = options.iter().map(|o| self.c.measure_text(o, f).0 + pad * 2).collect();
        let total: i32 = widths.iter().sum();
        let mut x = self.x + self.w - self.t.s(16.0) - total;
        let y = row.0 + (row.1 - hh) / 2;
        let rad = self.t.s(5.0) as f32;
        self.c.fill_round_rect(x, y, total, hh, rad, self.pal.control);
        for (i, (o, e)) in options.iter().zip(effects).enumerate() {
            let r = RECT { left: x, top: y, right: x + widths[i], bottom: y + hh };
            let on = selected == Some(i);
            if on {
                self.c.fill_round_rect(x + 2, y + 2, widths[i] - 4, hh - 4, rad - 1.0, self.pal.accent);
            } else if self.hovered(&r) {
                self.c.fill_round_rect(x + 2, y + 2, widths[i] - 4, hh - 4, rad - 1.0, self.pal.control);
            }
            let (tw, th) = self.c.measure_text(o, f);
            self.c.draw_text(o, x + (widths[i] - tw) / 2, y + (hh - th) / 2, f, if on { Color::rgb(255, 255, 255) } else { self.t.text });
            self.hit(r, e);
            x += widths[i];
        }
    }

    /// A push button; `right` = at the right end of a row, else at the cursor (`self.y`) left.
    fn button(&mut self, row: Option<(i32, i32)>, label: &str, e: Effect) -> RECT {
        let f = self.f(12.5, FW_NORMAL);
        let (tw, th) = self.c.measure_text(label, f);
        let (bw, bh) = (tw + self.t.s(28.0), self.t.s(30.0));
        let (x, y) = match row {
            Some(r) => (self.x + self.w - self.t.s(16.0) - bw, r.0 + (r.1 - bh) / 2),
            None => (self.x, self.y),
        };
        let r = RECT { left: x, top: y, right: x + bw, bottom: y + bh };
        let rad = self.t.s(5.0) as f32;
        self.c.fill_round_rect(x, y, bw, bh, rad, if self.hovered(&r) { self.t.text.alpha(0.14) } else { self.pal.control });
        self.c.stroke_round_rect_r(x, y, bw, bh, rad, self.pal.card_border);
        self.c.draw_text(label, x + (bw - tw) / 2, y + (bh - th) / 2, f, self.t.text);
        self.hit(r, e);
        if row.is_none() {
            self.y += bh + self.t.s(8.0);
        }
        r
    }

    /// [−] value [+] at the right end of a row, `right_offset` px further left.
    fn stepper(&mut self, row: (i32, i32), right_offset: i32, value: &str, minus: Effect, plus: Effect) {
        self.stepper_w(row, right_offset, 76.0, ("\u{2212}", "+"), value, minus, plus);
    }

    /// `stepper` with a value field `value_dip` wide and its own button symbols.
    fn stepper_w(&mut self, row: (i32, i32), right_offset: i32, value_dip: f32, symbols: (&str, &str), value: &str, minus: Effect, plus: Effect) {
        let f = self.f(12.5, FW_NORMAL);
        let bw = self.t.s(30.0);
        let vw = self.t.s(value_dip);
        let total = bw * 2 + vw;
        let x = self.x + self.w - self.t.s(16.0) - total - right_offset;
        let y = row.0 + (row.1 - bw) / 2;
        let rad = self.t.s(5.0) as f32;
        self.c.fill_round_rect(x, y, total, bw, rad, self.pal.control);
        for (i, (sym, e)) in [(symbols.0, minus), (symbols.1, plus)].into_iter().enumerate() {
            let bx = if i == 0 { x } else { x + bw + vw };
            let r = RECT { left: bx, top: y, right: bx + bw, bottom: y + bw };
            if self.hovered(&r) {
                self.c.fill_round_rect(bx + 2, y + 2, bw - 4, bw - 4, rad - 1.0, self.t.text.alpha(0.12));
            }
            let (sw, sh) = self.c.measure_text(sym, f);
            self.c.draw_text(sym, bx + (bw - sw) / 2, y + (bw - sh) / 2, f, self.t.text);
            self.hit(r, e);
        }
        let (tw, th) = self.c.measure_text(value, f);
        self.c.draw_text(value, x + bw + (vw - tw) / 2, y + (bw - th) / 2, f, self.t.text);
    }

    /// Colour squares at the right end of a row.
    fn swatches(&mut self, row: (i32, i32), current: &str) {
        let s = self.t.s(22.0);
        let gap = self.t.s(6.0);
        let n = crate::widgets::PALETTE.len() as i32;
        let mut x = self.x + self.w - self.t.s(16.0) - n * s - (n - 1) * gap;
        let y = row.0 + (row.1 - s) / 2;
        for (name, title, c) in crate::widgets::PALETTE {
            let col = crate::widgets::named_color(name).unwrap_or(*c);
            let r = RECT { left: x, top: y, right: x + s, bottom: y + s };
            self.c.fill_round_rect(x, y, s, s, self.t.s(5.0) as f32, col);
            self.c.stroke_round_rect_r(x, y, s, s, self.t.s(5.0) as f32, self.t.dim.alpha(0.5));
            if *name == current || self.hovered(&r) {
                let o = self.t.s(3.0);
                self.c.stroke_round_rect_r(x - o, y - o, s + o * 2, s + o * 2, self.t.s(7.0) as f32, self.t.text);
            }
            let n = name.to_string();
            let _ = title;
            self.hit(r, Effect::Cfg(Box::new(move |c: &mut Config| c.accent = n.clone())));
            x += s + gap;
        }
    }
}

fn lighten(c: Color) -> Color {
    Color::rgba(c.r + (255 - c.r) / 5, c.g + (255 - c.g) / 5, c.b + (255 - c.b) / 5, c.a)
}

/// Word-wrap `text` to `max_w` pixels.
fn wrap(c: &mut Canvas, text: &str, f: FontSpec, max_w: i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let cand = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if line.is_empty() || c.measure_text(&cand, f).0 <= max_w {
            line = cand;
        } else {
            lines.push(std::mem::replace(&mut line, word.to_string()));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn cfg_fx(f: impl Fn(&mut Config) + 'static) -> Effect {
    Effect::Cfg(Box::new(f))
}

fn page_general(ui: &mut Ui, cfg: &Config, autostart: bool) {
    ui.heading(&t("settings.page_general"));
    let r = ui.row(&t("menu.start_with_windows"), &t("settings.autostart_desc"));
    ui.toggle(r, autostart, Effect::Autostart(!autostart));

    let r = ui.row(&t("menu.language"), &t("settings.language_desc"));
    let current = if cfg.language == "auto" { t("menu.lang_system") } else { crate::i18n::LOCALES.iter().find(|l| l.0 == cfg.language).map(|l| l.1.to_string()).unwrap_or_default() };
    ui.button(Some(r), &format!("{current}  \u{25BE}"), Effect::LanguageMenu);

    let r = ui.row(&t("menu.theme"), "");
    let opts = [("auto", "menu.theme_auto"), ("dark", "menu.theme_dark"), ("light", "menu.theme_light")];
    let sel = opts.iter().position(|o| o.0 == cfg.theme);
    ui.segmented(r, &opts.iter().map(|o| t(o.1)).collect::<Vec<_>>(), sel, opts.iter().map(|o| {
        let v = o.0.to_string();
        cfg_fx(move |c| c.theme = v.clone())
    }).collect());

    let r = ui.row(&t("menu.update_interval"), &t("settings.interval_desc"));
    let sel = crate::config::INTERVALS.iter().position(|v| *v == cfg.update_ms);
    ui.segmented(r, &crate::config::INTERVALS.iter().map(|ms| crate::interval_title(*ms)).collect::<Vec<_>>(), sel, crate::config::INTERVALS.iter().map(|ms| {
        let v = *ms;
        cfg_fx(move |c| c.update_ms = v)
    }).collect());

    ui.section(&t("settings.taskbar"));
    let r = ui.row(&t("menu.position"), &t("settings.position_desc"));
    let sel = Some(if cfg.position == "left" { 0 } else { 1 });
    ui.segmented(r, &[t("settings.left"), t("settings.right")], sel, vec![cfg_fx(|c| c.position = "left".into()), cfg_fx(|c| c.position = "right".into())]);

    let r = ui.row(&t("settings.mode"), &t("settings.mode_desc"));
    let sel = Some(if cfg.mode == "floating" { 1 } else { 0 });
    ui.segmented(r, &[t("settings.mode_auto"), t("settings.mode_floating")], sel, vec![cfg_fx(|c| c.mode = "auto".into()), cfg_fx(|c| c.mode = "floating".into())]);

    ui.section(&t("settings.measuring"));
    let r = ui.row(&t("settings.cpu_utility"), &t("settings.cpu_utility_desc"));
    let on = cfg.cpu_utility;
    ui.toggle(r, on, cfg_fx(move |c| c.cpu_utility = !on));

    let r = ui.row(&t("menu.top_processes"), &t("settings.top_desc"));
    let sel = crate::TOP_COUNTS.iter().position(|n| *n == cfg.top_processes);
    ui.segmented(r, &crate::TOP_COUNTS.iter().map(|n| n.to_string()).collect::<Vec<_>>(), sel, crate::TOP_COUNTS.iter().map(|n| {
        let v = *n;
        cfg_fx(move |c| c.top_processes = v)
    }).collect());
}

fn page_appearance(ui: &mut Ui, cfg: &Config) {
    ui.heading(&t("settings.page_appearance"));
    let r = ui.row(&t("menu.accent_colour"), "");
    ui.swatches(r, &cfg.accent);

    let r = ui.row(&t("menu.font_size"), &t("settings.font_size_desc"));
    ui.stepper(
        r,
        0,
        &crate::font_size_label(cfg.font_scale),
        cfg_fx(|c| c.font_scale = crate::font_step(c.font_scale, -1)),
        cfg_fx(|c| c.font_scale = crate::font_step(c.font_scale, 1)),
    );

    // installed families of the list, plus a family set in config.toml
    let mut families: Vec<String> = crate::FONT_FAMILIES.iter().filter(|f| f.is_empty() || crate::canvas::font_installed(f)).map(|f| f.to_string()).collect();
    if !families.contains(&cfg.font_family) {
        families.push(cfg.font_family.clone());
    }
    let cur = families.iter().position(|f| *f == cfg.font_family).unwrap_or(0);
    let pick = |i: usize| {
        let v = families[i % families.len()].clone();
        cfg_fx(move |c| c.font_family = v.clone())
    };
    let shown = if cfg.font_family.is_empty() { t("settings.font_default") } else { cfg.font_family.clone() };
    let r = ui.row(&t("settings.font_family"), "");
    ui.stepper_w(r, 0, 150.0, ("\u{2039}", "\u{203A}"), &shown, pick(cur + families.len() - 1), pick(cur + 1));

    let r = ui.row(&t("menu.chart_width"), "");
    let sel = crate::CHART_WIDTHS.iter().position(|c| c.0 == cfg.chart_width);
    ui.segmented(r, &crate::CHART_WIDTHS.iter().map(|c| t(c.1)).collect::<Vec<_>>(), sel, crate::CHART_WIDTHS.iter().map(|c| {
        let v = c.0;
        cfg_fx(move |cf| cf.chart_width = v)
    }).collect());

    let r = ui.row(&t("menu.bar_width"), "");
    let sel = crate::widgets::BAR_WIDTHS.iter().position(|b| b.0 == cfg.bar_width);
    ui.segmented(r, &crate::widgets::BAR_WIDTHS.iter().map(|b| t(b.2)).collect::<Vec<_>>(), sel, crate::widgets::BAR_WIDTHS.iter().map(|b| {
        let v = b.0.to_string();
        cfg_fx(move |c| c.bar_width = v.clone())
    }).collect());

    let r = ui.row(&t("menu.labels"), &t("settings.labels_desc"));
    let on = cfg.labels;
    ui.toggle(r, on, cfg_fx(move |c| c.labels = !on));

    let r = ui.row(&t("menu.popup_opacity"), "");
    let sel = crate::POPUP_OPACITY.iter().position(|o| o.0 == cfg.popup_opacity);
    ui.segmented(r, &crate::POPUP_OPACITY.iter().map(|o| t(o.1)).collect::<Vec<_>>(), sel, crate::POPUP_OPACITY.iter().map(|o| {
        let v = o.0.to_string();
        cfg_fx(move |c| c.popup_opacity = v.clone())
    }).collect());

    ui.section(&t("module.cpu"));
    let r = ui.row(&t("menu.show_pe_splitter"), "");
    let on = cfg.split_pe_cores;
    ui.toggle(r, on, cfg_fx(move |c| c.split_pe_cores = !on));
    let r = ui.row(&t("menu.group_pe_cores"), "");
    let on = cfg.group_pe_cores;
    ui.toggle(r, on, cfg_fx(move |c| c.group_pe_cores = !on));
}

fn page_widgets(ui: &mut Ui, cfg: &Config, m: &MetricsData) {
    ui.heading(&t("settings.page_widgets"));
    ui.note(&t("settings.widgets_desc"));
    ui.section(&t("settings.shown"));
    let n = cfg.widgets.len();
    for (i, name) in cfg.widgets.iter().enumerate() {
        let Some((kind, _, title)) = crate::widgets::CATALOG.iter().find(|(_, n, _)| n == name) else { continue };
        let r = ui.row(&format!("{}  \u{00B7}  {}", kind.title(), t(title)), "");
        let bw = ui.t.s(34.0);
        let (y, h) = r;
        // remove, down, up from the right
        let mut x = ui.x + ui.w - ui.t.s(16.0) - bw;
        for (sym, enabled, e) in [
            ("\u{2715}", true, {
                let nm = name.clone();
                cfg_fx(move |c| c.widgets.retain(|w| *w != nm))
            }),
            ("\u{2193}", i + 1 < n, cfg_fx(move |c| if i + 1 < c.widgets.len() { c.widgets.swap(i, i + 1) })),
            ("\u{2191}", i > 0, cfg_fx(move |c| if i > 0 && i < c.widgets.len() { c.widgets.swap(i, i - 1) })),
        ] {
            let rr = RECT { left: x, top: y + (h - bw) / 2, right: x + bw, bottom: y + (h + bw) / 2 };
            let rad = ui.t.s(5.0) as f32;
            let hov = ui.hovered(&rr) && enabled;
            ui.c.fill_round_rect(rr.left, rr.top, bw, bw, rad, if hov { ui.t.text.alpha(0.14) } else { ui.pal.control });
            let f = ui.f(13.0, FW_NORMAL);
            let (sw, sh) = ui.c.measure_text(sym, f);
            ui.c.draw_text(sym, x + (bw - sw) / 2, rr.top + (bw - sh) / 2, f, if enabled { ui.t.text } else { ui.t.dim.alpha(0.4) });
            if enabled {
                ui.hit(rr, e);
            }
            x -= bw + ui.t.s(6.0);
        }
    }
    ui.section(&t("settings.available"));
    for (kind, name, title) in crate::widgets::CATALOG {
        if cfg.widgets.iter().any(|w| w == name) {
            continue;
        }
        let available = match kind {
            Kind::Gpu => m.gpu_available,
            Kind::Battery => m.battery_available,
            _ => true,
        };
        let desc = if available { String::new() } else { t("popup.not_available") };
        let r = ui.row(&format!("{}  \u{00B7}  {}", kind.title(), t(title)), &desc);
        if available {
            let nm = name.to_string();
            ui.button(Some(r), &t("settings.add"), cfg_fx(move |c| crate::widgets::toggle_widget(&mut c.widgets, &nm)));
        }
    }
    ui.section(&t("menu.colour"));
    for k in crate::widgets::MODULES {
        if *k == Kind::Battery {
            continue;
        }
        let key = crate::module_key(*k);
        let current = cfg.colors.get(key).map(String::as_str).unwrap_or("accent");
        let r = ui.row(&k.title(), "");
        let modes = [("accent", "menu.accent"), ("utilization", "menu.by_utilisation"), ("mono", "menu.monochrome")];
        let sel = modes.iter().position(|m| m.0 == current);
        ui.segmented(r, &modes.iter().map(|m| t(m.1)).collect::<Vec<_>>(), sel, modes.iter().map(|md| {
            let (v, k) = (md.0.to_string(), key.to_string());
            cfg_fx(move |c| {
                if v == "accent" {
                    c.colors.remove(&k);
                } else {
                    c.colors.insert(k.clone(), v.clone());
                }
                c.utilization_colors = false;
            })
        }).collect());
    }
}

fn page_alerts(ui: &mut Ui, cfg: &Config, m: &MetricsData) {
    ui.heading(&t("settings.page_alerts"));
    ui.note(&t("settings.alerts_desc"));
    for (i, rule) in cfg.alerts.iter().enumerate() {
        let measured = rule.metric.value(m).is_some();
        let cond = if rule.above { tf("settings.alert_above", &[("limit", &rule.metric.format(rule.threshold)), ("seconds", &rule.seconds.to_string())]) } else { tf("settings.alert_below", &[("limit", &rule.metric.format(rule.threshold))]) };
        let desc = if measured { cond } else { format!("{cond} \u{00B7} {}", t("settings.not_measured")) };
        let second = ui.t.s(46.0);
        let full = ui.row_ex(&t(rule.metric.key()), &desc, ui.t.s(150.0), second);
        // first line: the switch; second line: limit and duration
        let top = (full.0, full.1 - second);
        let r = (full.0 + full.1 - second - ui.t.s(8.0), second);
        let on = rule.enabled;
        ui.toggle(top, on, cfg_fx(move |c| if let Some(x) = c.alerts.get_mut(i) { x.enabled = !on }));
        let lf = ui.f(12.0, FW_NORMAL);
        let lw = ui.c.measure_text(&t("settings.limit"), lf);
        let lx = ui.x + ui.w - ui.t.s(16.0) - ui.t.s(136.0) - ui.t.s(10.0) - lw.0;
        ui.c.draw_text(&t("settings.limit"), lx, r.0 + (r.1 - lw.1) / 2, lf, ui.t.dim);
        let step = 5.0;
        let (lo, hi) = if rule.metric.is_temperature() { (30.0, 110.0) } else { (5.0, 100.0) };
        let off = 0;
        ui.stepper(
            r,
            off,
            &rule.metric.format(rule.threshold),
            cfg_fx(move |c| if let Some(x) = c.alerts.get_mut(i) { x.threshold = (x.threshold - step).max(lo) }),
            cfg_fx(move |c| if let Some(x) = c.alerts.get_mut(i) { x.threshold = (x.threshold + step).min(hi) }),
        );
        if rule.above {
            let off2 = ui.t.s(190.0);
            let dw = ui.c.measure_text(&t("settings.duration"), lf);
            let dx = ui.x + ui.w - ui.t.s(16.0) - off2 - ui.t.s(136.0) - ui.t.s(10.0) - dw.0;
            ui.c.draw_text(&t("settings.duration"), dx, r.0 + (r.1 - dw.1) / 2, lf, ui.t.dim);
            ui.stepper(
                r,
                off2,
                &format!("{} {}", rule.seconds, t("settings.seconds_short")),
                cfg_fx(move |c| if let Some(x) = c.alerts.get_mut(i) { x.seconds = x.seconds.saturating_sub(10) }),
                cfg_fx(move |c| if let Some(x) = c.alerts.get_mut(i) { x.seconds = (x.seconds + 10).min(600) }),
            );
        }
    }
    let _ = Metric::ALL;
}

fn page_privacy(ui: &mut Ui, cfg: &Config) {
    ui.heading(&t("settings.page_privacy"));
    ui.note(&t("settings.privacy_desc"));
    let r = ui.row(&t("settings.ping"), &t("settings.ping_desc"));
    let hosts = ["", "1.1.1.1", "8.8.8.8"];
    let sel = hosts.iter().position(|h| *h == cfg.ping_host);
    ui.segmented(r, &[t("settings.off"), "1.1.1.1".into(), "8.8.8.8".into()], sel, hosts.iter().map(|h| {
        let v = h.to_string();
        cfg_fx(move |c| c.ping_host = v.clone())
    }).collect());
    ui.section(&t("settings.files"));
    let dir = crate::util::config_dir();
    ui.button(None, &t("settings.open_folder"), Effect::Open(dir.display().to_string()));
    ui.button(None, &t("menu.open_config"), Effect::Open(Config::path().display().to_string()));
    ui.button(None, &t("settings.open_log"), Effect::Open(crate::util::log_path().display().to_string()));
}

fn page_about(ui: &mut Ui) {
    ui.heading(&t("settings.page_about"));
    let f = ui.f(13.0, FW_NORMAL);
    for line in [tf_ver(), tf("about.based_on", &[("author", crate::about::UPSTREAM_AUTHOR)]), t("settings.inspired")] {
        let (_, h) = ui.c.draw_text(&line, ui.x, ui.y, f, ui.t.text);
        ui.y += h + ui.t.s(4.0);
    }
    ui.y += ui.t.s(12.0);
    let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf()));
    let local = |name: &str| dir.as_ref().map(|d| d.join(name)).filter(|p| p.exists()).map(|p| p.display().to_string());
    ui.button(None, &t("about.license"), Effect::Open(local("LICENSE.txt").unwrap_or_else(|| crate::about::UPSTREAM_LICENSE_URL.to_string())));
    if let Some(p) = local("THIRD-PARTY-NOTICES.txt") {
        ui.button(None, &t("settings.notices"), Effect::Open(p));
    }
    ui.button(None, "github.com/mgvs/winstats", Effect::Open(crate::about::UPSTREAM_URL.to_string()));
    ui.button(None, "github.com/exelban/stats", Effect::Open("https://github.com/exelban/stats".to_string()));
}

fn page_sensors(ui: &mut Ui, cfg: &Config, m: &MetricsData) {
    ui.heading(&t("settings.page_sensors"));
    ui.note(&t("settings.sensors_desc"));
    let available = crate::sensorhost::helper_path().is_some();
    let desc = if available { t("settings.lhm_desc") } else { format!("{} {}", t("settings.lhm_desc"), t("settings.helper_missing")) };
    let r = ui.row(&t("settings.lhm"), &desc);
    let on = cfg.lhm_sensors;
    if available || on {
        ui.toggle(r, on, cfg_fx(move |c| c.lhm_sensors = !on));
    }
    if on {
        ui.note(&t("settings.lhm_admin_disabled"));
    }
    let status = if !on {
        t("popup.helper_off")
    } else if !m.helper.running {
        t("popup.helper_starting")
    } else if m.helper.elevated && m.helper.pawnio {
        t("popup.helper_full")
    } else {
        t("popup.helper_limited")
    };
    let pawn = if m.helper.pawnio { t("settings.pawnio_yes") } else { t("settings.pawnio_no") };
    ui.row(&t("settings.status"), &format!("{status} \u{00B7} {pawn}"));
    ui.section(&t("popup.why_missing"));
    ui.note(&t("settings.pawnio_long"));
    ui.button(None, &t("settings.pawnio_site"), Effect::Open("https://pawnio.eu".to_string()));
    ui.button(None, "LibreHardwareMonitor (MPL-2.0)", Effect::Open("https://github.com/LibreHardwareMonitor/LibreHardwareMonitor".to_string()));
}
