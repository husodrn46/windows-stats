//! Text through DirectWrite.
//!
//! The strip sits on a translucent taskbar, so ClearType (which has to know the colour behind
//! every pixel) cannot be used, and GDI's grayscale antialiasing is switched off by Segoe UI's
//! gasp table at small sizes. DirectWrite rasterises grayscale-antialiased, hinted glyphs at any
//! size, like the Windows 11 taskbar clock. Each string is laid out with IDWriteTextLayout
//! (shaping, kerning, font fallback, tabular figures), its glyph runs are rasterised into an
//! 8-bit coverage mask, and the canvas blends the mask in the text colour with the contrast and
//! gamma correction Direct2D applies to grayscale text (`text_alpha`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use windows::core::{implement, IUnknown, Result, PCWSTR};
use windows::Win32::Foundation::{BOOL, RECT};
use windows::Win32::Graphics::DirectWrite::*;

/// Font size in pixels (em height) and weight (400 normal, 600 semibold...).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FontSpec {
    pub px: i32,
    pub weight: i32,
}

/// Coverage of a rendered string. `x0`, `y0` place the mask relative to the layout's top-left
/// corner (glyphs may overhang the advance box by a pixel); `w`, `h` of the layout box are
/// what `measure` reports.
pub struct Mask {
    pub x0: i32,
    pub y0: i32,
    pub mw: i32,
    pub mh: i32,
    pub cov: Vec<u8>,
}

/// One glyph run's texture, in layout pixel coordinates.
struct Tex {
    rect: RECT,
    cov: Vec<u8>,
}

#[implement(IDWriteTextRenderer)]
struct MaskRenderer {
    factory: IDWriteFactory2,
    out: Rc<RefCell<Vec<Tex>>>,
}

impl IDWritePixelSnapping_Impl for MaskRenderer_Impl {
    fn IsPixelSnappingDisabled(&self, _ctx: *const core::ffi::c_void) -> Result<BOOL> {
        // snap baselines to whole pixels: crisper text
        Ok(BOOL(0))
    }
    fn GetCurrentTransform(&self, _ctx: *const core::ffi::c_void, transform: *mut DWRITE_MATRIX) -> Result<()> {
        unsafe {
            *transform = DWRITE_MATRIX { m11: 1.0, m12: 0.0, m21: 0.0, m22: 1.0, dx: 0.0, dy: 0.0 };
        }
        Ok(())
    }
    fn GetPixelsPerDip(&self, _ctx: *const core::ffi::c_void) -> Result<f32> {
        // sizes are already in physical pixels
        Ok(1.0)
    }
}

impl IDWriteTextRenderer_Impl for MaskRenderer_Impl {
    fn DrawGlyphRun(
        &self,
        _ctx: *const core::ffi::c_void,
        x: f32,
        y: f32,
        measuring: DWRITE_MEASURING_MODE,
        run: *const DWRITE_GLYPH_RUN,
        _desc: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _effect: Option<&IUnknown>,
    ) -> Result<()> {
        unsafe {
            // natural symmetric + grid fitting: vertically hinted stems (crisp at 9-12 px),
            // grayscale coverage that blends on any background
            let analysis = self.factory.CreateGlyphRunAnalysis(
                run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                measuring,
                DWRITE_GRID_FIT_MODE_ENABLED,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                x,
                y,
            )?;
            let rect = analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1)?;
            let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
            if w <= 0 || h <= 0 {
                return Ok(());
            }
            let mut cov = vec![0u8; (w * h) as usize];
            analysis.CreateAlphaTexture(DWRITE_TEXTURE_ALIASED_1x1, &rect, &mut cov)?;
            self.out.borrow_mut().push(Tex { rect, cov });
        }
        Ok(())
    }
    fn DrawUnderline(&self, _ctx: *const core::ffi::c_void, _x: f32, _y: f32, _u: *const DWRITE_UNDERLINE, _e: Option<&IUnknown>) -> Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(&self, _ctx: *const core::ffi::c_void, _x: f32, _y: f32, _s: *const DWRITE_STRIKETHROUGH, _e: Option<&IUnknown>) -> Result<()> {
        Ok(())
    }
    fn DrawInlineObject(&self, _ctx: *const core::ffi::c_void, _x: f32, _y: f32, _o: Option<&IDWriteInlineObject>, _side: BOOL, _rtl: BOOL, _e: Option<&IUnknown>) -> Result<()> {
        Ok(())
    }
}

/// Font families by preference: Windows 11's Segoe UI Variable has optical sizes for small
/// ("Small") and body ("Text") text; Windows 10 has Segoe UI.
const FAMILY_SMALL: &str = "Segoe UI Variable Small";
const FAMILY_TEXT: &str = "Segoe UI Variable Text";
const FAMILY_FALLBACK: &str = "Segoe UI";
/// Below this pixel size the "Small" optical size is used.
const SMALL_BELOW_PX: i32 = 13;

/// Entries kept per cache before it is cleared (values change every second: bounded).
const CACHE_MAX: usize = 600;

pub struct TextEngine {
    factory: IDWriteFactory2,
    /// (family for small sizes, family for larger sizes), wide and null-terminated
    families: (Vec<u16>, Vec<u16>),
    formats: HashMap<FontSpec, IDWriteTextFormat>,
    typography: Option<IDWriteTypography>,
    renderer: IDWriteTextRenderer,
    out: Rc<RefCell<Vec<Tex>>>,
    measured: HashMap<(String, FontSpec), (i32, i32)>,
    masks: HashMap<(String, FontSpec), Rc<Mask>>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn has_family(factory: &IDWriteFactory2, name: &str) -> bool {
    unsafe {
        let mut coll: Option<IDWriteFontCollection> = None;
        if factory.GetSystemFontCollection(&mut coll, false).is_err() {
            return false;
        }
        let Some(coll) = coll else { return false };
        let n = wide(name);
        let (mut index, mut exists) = (0u32, BOOL(0));
        coll.FindFamilyName(PCWSTR::from_raw(n.as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool()
    }
}

/// Segoe UI Variable (optical sizes Small / Text) on Windows 11, Segoe UI on Windows 10.
fn default_families(factory: &IDWriteFactory2) -> (Vec<u16>, Vec<u16>) {
    if has_family(factory, FAMILY_SMALL) && has_family(factory, FAMILY_TEXT) {
        (wide(FAMILY_SMALL), wide(FAMILY_TEXT))
    } else {
        (wide(FAMILY_FALLBACK), wide(FAMILY_FALLBACK))
    }
}

impl TextEngine {
    pub fn new() -> Option<Self> {
        unsafe {
            let factory: IDWriteFactory2 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let families = default_families(&factory);
            // tabular figures: numbers keep their width as they change, nothing jiggles
            let typography = factory.CreateTypography().ok();
            if let Some(t) = &typography {
                let _ = t.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 });
            }
            let out = Rc::new(RefCell::new(Vec::new()));
            let renderer: IDWriteTextRenderer = MaskRenderer { factory: factory.clone(), out: out.clone() }.into();
            crate::log!("text: DirectWrite, {}", String::from_utf16_lossy(&families.1[..families.1.len() - 1]));
            Some(Self { factory, families, formats: HashMap::new(), typography, renderer, out, measured: HashMap::new(), masks: HashMap::new() })
        }
    }

    /// Use `name` for all text (empty = the default families). A family that is not installed
    /// falls back to the default; returns whether `name` was used.
    pub fn set_family(&mut self, name: &str) -> bool {
        let wanted = !name.is_empty() && has_family(&self.factory, name);
        if !name.is_empty() && !wanted {
            crate::log!("text: font family {name} is not installed, using the default");
        }
        let families = if wanted { (wide(name), wide(name)) } else { default_families(&self.factory) };
        if families != self.families {
            self.families = families;
            self.formats.clear();
            self.measured.clear();
            self.masks.clear();
            crate::log!("text: font family {}", if wanted { name } else { "default" });
        }
        wanted || name.is_empty()
    }

    /// Whether a font family is installed.
    pub fn has_family(&self, name: &str) -> bool {
        has_family(&self.factory, name)
    }

    fn format(&mut self, spec: FontSpec) -> Option<IDWriteTextFormat> {
        if let Some(f) = self.formats.get(&spec) {
            return Some(f.clone());
        }
        let family = if spec.px < SMALL_BELOW_PX { &self.families.0 } else { &self.families.1 };
        let locale = wide("");
        let f = unsafe {
            self.factory
                .CreateTextFormat(
                    PCWSTR::from_raw(family.as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT(spec.weight.clamp(100, 950)),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    spec.px.max(1) as f32,
                    PCWSTR::from_raw(locale.as_ptr()),
                )
                .ok()?
        };
        unsafe {
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        }
        self.formats.insert(spec, f.clone());
        Some(f)
    }

    fn layout(&mut self, text: &str, spec: FontSpec) -> Option<IDWriteTextLayout> {
        let format = self.format(spec)?;
        let t: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout = self.factory.CreateTextLayout(&t, &format, 100_000.0, 100_000.0).ok()?;
            if let Some(ty) = &self.typography {
                let _ = layout.SetTypography(ty, DWRITE_TEXT_RANGE { startPosition: 0, length: t.len() as u32 });
            }
            Some(layout)
        }
    }

    /// (advance width, line height) in pixels.
    pub fn measure(&mut self, text: &str, spec: FontSpec) -> (i32, i32) {
        if text.is_empty() {
            return (0, self.line_height(spec));
        }
        let key = (text.to_string(), spec);
        if let Some(m) = self.measured.get(&key) {
            return *m;
        }
        let Some(layout) = self.layout(text, spec) else { return (0, 0) };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = layout.GetMetrics(&mut m);
        }
        let size = (m.widthIncludingTrailingWhitespace.ceil() as i32, m.height.ceil() as i32);
        if self.measured.len() > CACHE_MAX {
            self.measured.clear();
        }
        self.measured.insert(key, size);
        size
    }

    /// Height of one line in `spec`, whatever the text.
    pub fn line_height(&mut self, spec: FontSpec) -> i32 {
        self.measure("X", spec).1
    }

    /// Coverage mask of `text`, cached.
    pub fn render(&mut self, text: &str, spec: FontSpec) -> Option<Rc<Mask>> {
        if text.is_empty() {
            return None;
        }
        let key = (text.to_string(), spec);
        if let Some(m) = self.masks.get(&key) {
            return Some(m.clone());
        }
        let layout = self.layout(text, spec)?;
        self.out.borrow_mut().clear();
        unsafe {
            layout.Draw(None, &self.renderer, 0.0, 0.0).ok()?;
        }
        let texs = std::mem::take(&mut *self.out.borrow_mut());
        if texs.is_empty() {
            return None;
        }
        // union of the run textures
        let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for x in &texs {
            l = l.min(x.rect.left);
            t = t.min(x.rect.top);
            r = r.max(x.rect.right);
            b = b.max(x.rect.bottom);
        }
        let (mw, mh) = (r - l, b - t);
        let mut cov = vec![0u8; (mw * mh) as usize];
        for x in &texs {
            let w = x.rect.right - x.rect.left;
            for row in 0..(x.rect.bottom - x.rect.top) {
                let dst = ((x.rect.top - t + row) * mw + (x.rect.left - l)) as usize;
                let src = (row * w) as usize;
                for i in 0..w as usize {
                    let v = x.cov[src + i];
                    let d = &mut cov[dst + i];
                    *d = (*d).max(v);
                }
            }
        }
        let mask = Rc::new(Mask { x0: l, y0: t, mw, mh, cov });
        if self.masks.len() > CACHE_MAX {
            self.masks.clear();
        }
        self.masks.insert(key, mask.clone());
        Some(mask)
    }
}

/// Final alpha (0..1) for coverage `cov` (0..1) of text in colour (r, g, b), the way Direct2D
/// shades grayscale text: contrast enhancement for dark text, then the alpha correction for a
/// display gamma of 1.8 (DirectWrite's default), which keeps light text on a dark taskbar from
/// looking thin and dark text on a light one from looking heavy.
pub fn text_alpha(cov: f32, r: u8, g: u8, b: u8) -> f32 {
    if cov <= 0.0 {
        return 0.0;
    }
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    // grayscale enhanced contrast 1.0, reduced to nothing for light text (light-on-dark rule)
    let lum = r * 0.30 + g * 0.59 + b * 0.11;
    let k = 1.0 * (lum * -4.0 + 3.0).clamp(0.0, 1.0);
    let a = cov * (k + 1.0) / (cov * k + 1.0);
    // gamma 1.8 correction ratios
    const G: [f32; 4] = [0.1469 / 4.0, -0.8911 / 4.0, 1.4644 / 4.0, -0.3234 / 4.0];
    let f = r * 0.25 + g * 0.5 + b * 0.25;
    (a + a * (1.0 - a) * ((G[0] * f + G[1]) * a + (G[2] * f + G[3]))).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_correction_is_monotonic_and_bounded() {
        for &(r, g, b) in &[(255u8, 255u8, 255u8), (0, 0, 0), (0x2E, 0x7C, 0xF6), (128, 128, 128)] {
            let mut prev = 0.0;
            for i in 0..=255 {
                let a = text_alpha(i as f32 / 255.0, r, g, b);
                assert!((0.0..=1.0).contains(&a));
                assert!(a + 1e-4 >= prev, "not monotonic at {i} for {r},{g},{b}");
                prev = a;
            }
            assert_eq!(text_alpha(0.0, r, g, b), 0.0);
            assert!((text_alpha(1.0, r, g, b) - 1.0).abs() < 1e-4);
        }
        // light text is thickened, dark text is not
        assert!(text_alpha(0.5, 255, 255, 255) > 0.5);
    }

    #[test]
    fn renders_turkish_text() {
        let Some(mut e) = TextEngine::new() else { return };
        let spec = FontSpec { px: 12, weight: 400 };
        let (w, h) = e.measure("İşlemci çğüşöı", spec);
        assert!(w > 40 && h >= 12, "{w}x{h}");
        let m = e.render("İşlemci", spec).expect("mask");
        assert!(m.mw > 0 && m.mh > 0);
        assert!(m.cov.iter().any(|v| *v > 200), "no solid pixels");
        assert!(m.cov.iter().any(|v| *v > 20 && *v < 200), "no antialiased pixels");
    }

    #[test]
    fn digits_are_tabular() {
        let Some(mut e) = TextEngine::new() else { return };
        let spec = FontSpec { px: 11, weight: 600 };
        assert_eq!(e.measure("11.1", spec).0, e.measure("88.8", spec).0);
    }
}
