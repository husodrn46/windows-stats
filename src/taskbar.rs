//! Locating the Explorer taskbar and hosting our window inside it:
//! SetParent(our_hwnd, Shell_TrayWnd), then keep it positioned next to the notification area.
//!
//! Care for Explorer's stability: the strip only reads Explorer's windows (FindWindowEx,
//! GetWindowRect) and moves its own window. The one exception is Windows 10, where the task
//! list (ReBarWindow32) is narrowed so it does not run under the strip; it is put back on exit
//! and whenever the strip moves away. On Windows 11 the task list is XAML and is never touched.

use crate::util::{pcwstr, wide};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::MapWindowPoints;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

pub struct Taskbar {
    /// true: the strip is a top-level always-on-top window over the taskbar (Windows 7);
    /// false: it is a layered child of Shell_TrayWnd (Windows 8 and later)
    pub floating: bool,
    pub tray: HWND,
    pub notify: Option<HWND>,
    /// Windows 10 only: the task-list host we shrink to make room.
    pub rebar: Option<HWND>,
    pub rebar_original: Option<RECT>,
    /// last rect handed to SetWindowPos (taskbar client coords), to skip repeating it
    placed: std::cell::Cell<Option<RECT>>,
}

fn find_child(parent: HWND, class: &str) -> Option<HWND> {
    let c = wide(class);
    unsafe { FindWindowExW(parent, None, pcwstr(&c), PCWSTR::null()).ok() }
}

pub fn find(floating: bool) -> Option<Taskbar> {
    let cls = wide("Shell_TrayWnd");
    let tray = unsafe { FindWindowExW(None, None, pcwstr(&cls), PCWSTR::null()).ok()? };
    let notify = find_child(tray, "TrayNotifyWnd");
    // Windows 11 keeps an unused ReBarWindow32 around; only Windows 10 lays the task list out in it
    let rebar = if crate::util::is_windows11() { None } else { find_child(tray, "ReBarWindow32") };
    let rebar_original = rebar.and_then(|r| child_rect(tray, r));
    Some(Taskbar { floating, tray, notify, rebar, rebar_original, placed: std::cell::Cell::new(None) })
}

/// Rect of `child` expressed in `parent` client coordinates.
pub fn child_rect(parent: HWND, child: HWND) -> Option<RECT> {
    unsafe {
        let mut rc = RECT::default();
        GetWindowRect(child, &mut rc).ok()?;
        let mut pts = [POINT { x: rc.left, y: rc.top }, POINT { x: rc.right, y: rc.bottom }];
        MapWindowPoints(None, parent, &mut pts);
        Some(RECT { left: pts[0].x, top: pts[0].y, right: pts[1].x, bottom: pts[1].y })
    }
}

impl Taskbar {
    pub fn alive(&self) -> bool {
        unsafe { IsWindow(self.tray).as_bool() }
    }

    /// DPI of the taskbar's monitor (Windows 10+), else the session-wide DPI from the screen DC.
    pub fn dpi(&self) -> u32 {
        if let Some(d) = crate::util::dpi_for_window(self.tray) {
            return d;
        }
        use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSX, ReleaseDC};
        unsafe {
            let hdc = GetDC(None);
            let d = GetDeviceCaps(hdc, LOGPIXELSX);
            ReleaseDC(None, hdc);
            if d <= 0 { 96 } else { d as u32 }
        }
    }

    pub fn client(&self) -> RECT {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.tray, &mut rc);
        }
        rc
    }

    /// True for a taskbar docked at the left or right edge (taller than wide).
    pub fn vertical(&self) -> bool {
        let rc = self.client();
        rc.bottom - rc.top > rc.right - rc.left
    }

    /// Size across the taskbar: its height when horizontal, its width when vertical.
    pub fn thickness(&self) -> i32 {
        let rc = self.client();
        if self.vertical() {
            rc.right - rc.left
        } else {
            rc.bottom - rc.top
        }
    }

    /// True when the taskbar sits above `hwnd` in the z-order (the floating strip is covered).
    pub fn covers(&self, hwnd: HWND) -> bool {
        let mut h = unsafe { GetWindow(hwnd, GW_HWNDPREV) };
        let mut guard = 0;
        while let Ok(w) = h {
            if w == self.tray {
                return true;
            }
            guard += 1;
            if guard > 512 {
                break;
            }
            h = unsafe { GetWindow(w, GW_HWNDPREV) };
        }
        false
    }

    /// Reparent `hwnd` (created as a layered popup) into the taskbar. In floating mode
    /// (Windows 7 cannot host a layered child) the window stays top-level and `place` keeps it
    /// over the taskbar; a WinEvent hook in main.rs re-raises it when Explorer lifts the taskbar.
    pub fn attach(&self, hwnd: HWND) -> bool {
        if self.floating {
            return true;
        }
        unsafe {
            // `as _`: isize on 64-bit, i32 on 32-bit where the crate maps this to SetWindowLongW
            SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS).0 as _);
            // no WM_PARENTNOTIFY: nothing of ours is ever sent synchronously to Explorer's window
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOPARENTNOTIFY.0 as isize);
            self.placed.set(None);
            match SetParent(hwnd, self.tray) {
                Ok(_) => true,
                Err(e) => {
                    crate::log!("SetParent failed: {e}");
                    false
                }
            }
        }
    }

    /// Left edge of the notification area in taskbar client coords (the right end of the room
    /// the strip may use on a horizontal taskbar).
    pub fn notify_left(&self) -> i32 {
        self.notify.and_then(|n| child_rect(self.tray, n)).map(|n| n.left).unwrap_or_else(|| self.client().right)
    }

    /// Screen x of the taskbar's client origin (to bring UI Automation rects into client coords).
    pub fn origin_x(&self) -> i32 {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.tray, &mut pt);
        }
        pt.x
    }

    /// Position the strip; `len` is its size along the taskbar. `span` is the stretch of the
    /// taskbar (client x, start..end) Explorer's own items leave free, when known: the strip sits
    /// at its right end ("right") or left end ("left"). Returns the rect used (client coords).
    pub fn place(&self, hwnd: HWND, len: i32, position: &str, offset_px: i32, span: Option<(i32, i32)>) -> RECT {
        let rc = self.client();
        let notify = self.notify.and_then(|n| child_rect(self.tray, n));
        if self.vertical() {
            return self.place_vertical(hwnd, len, position, offset_px, rc, notify);
        }
        let h = rc.bottom - rc.top;
        let width = len;
        let x = match (position, span) {
            ("left", Some((a, _))) => a + offset_px,
            ("left", None) => rc.left + offset_px,
            (_, Some((_, b))) => b - width - offset_px,
            (_, None) => {
                let anchor = notify.map(|n| n.left).unwrap_or(rc.right);
                anchor - width - offset_px
            }
        };
        let x = x.max(rc.left);
        unsafe {
            if let (Some(rebar), Some(orig)) = (self.rebar, self.rebar_original) {
                // Windows 10: shrink the task list so it does not run underneath us.
                if let Some(cur) = child_rect(self.tray, rebar) {
                    let desired_right = if position == "left" { orig.right } else { x - 2 };
                    let desired_left = if position == "left" { (x + width + 2).max(orig.left) } else { orig.left };
                    if cur.right != desired_right || cur.left != desired_left {
                        let _ = MoveWindow(rebar, desired_left, cur.top, desired_right - desired_left, cur.bottom - cur.top, true);
                    }
                }
            }
            let want = RECT { left: x, top: rc.top, right: x + width, bottom: rc.top + h };
            if self.floating {
                // floating strip: taskbar client coords -> screen, kept above the taskbar
                let mut pt = POINT { x, y: rc.top };
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.tray, &mut pt);
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, pt.x, pt.y, width, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            } else if self.needs_place(hwnd, want) {
                let _ = SetWindowPos(hwnd, HWND_TOP, x, rc.top, width, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
            want
        }
    }

    /// Embedded strip: true unless it already sits at `want`, visible and on top of its siblings
    /// (the XAML content of the Windows 11 taskbar can come up over it after a relayout).
    fn needs_place(&self, hwnd: HWND, want: RECT) -> bool {
        let same = self.placed.get() == Some(want);
        self.placed.set(Some(want));
        if !same {
            return true;
        }
        unsafe {
            let on_top = GetWindow(hwnd, GW_HWNDPREV).map(|h| h.0.is_null()).unwrap_or(true);
            let here = child_rect(self.tray, hwnd).map_or(false, |r| r == want);
            !(on_top && here && IsWindowVisible(hwnd).as_bool())
        }
    }

    /// Vertical taskbar: the strip is a column. "left" is the top end, "right" the end next to
    /// the notification area, like the horizontal layout turned by 90 degrees.
    fn place_vertical(&self, hwnd: HWND, len: i32, position: &str, offset_px: i32, rc: RECT, notify: Option<RECT>) -> RECT {
        let w = rc.right - rc.left;
        let y = match position {
            "left" => rc.top + offset_px,
            _ => notify.map(|n| n.top).unwrap_or(rc.bottom) - len - offset_px,
        };
        let y = y.max(rc.top);
        unsafe {
            if let (Some(rebar), Some(orig)) = (self.rebar, self.rebar_original) {
                if let Some(cur) = child_rect(self.tray, rebar) {
                    let desired_bottom = if position == "left" { orig.bottom } else { y - 2 };
                    let desired_top = if position == "left" { (y + len + 2).max(orig.top) } else { orig.top };
                    if cur.bottom != desired_bottom || cur.top != desired_top {
                        let _ = MoveWindow(rebar, cur.left, desired_top, cur.right - cur.left, desired_bottom - desired_top, true);
                    }
                }
            }
            let want = RECT { left: rc.left, top: y, right: rc.right, bottom: y + len };
            if self.floating {
                let mut pt = POINT { x: rc.left, y };
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.tray, &mut pt);
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, pt.x, pt.y, w, len, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            } else if self.needs_place(hwnd, want) {
                let _ = SetWindowPos(hwnd, HWND_TOP, rc.left, y, w, len, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
            want
        }
    }

    /// Undo the Windows 10 task-list shrink.
    pub fn restore(&self) {
        if let (Some(rebar), Some(orig)) = (self.rebar, self.rebar_original) {
            unsafe {
                let _ = MoveWindow(rebar, orig.left, orig.top, orig.right - orig.left, orig.bottom - orig.top, true);
            }
        }
    }
}

/// Whether the taskbar takes a child window from this process right now: a hidden, empty
/// probe window is made its child and destroyed again at once.
pub fn can_host_children(tray: HWND) -> bool {
    unsafe {
        let cls = wide("STATIC");
        let Ok(probe) = CreateWindowExW(WS_EX_TOOLWINDOW, pcwstr(&cls), PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, None, None) else { return false };
        SetWindowLongPtrW(probe, GWL_STYLE, WS_CHILD.0 as _);
        let ok = SetParent(probe, tray).is_ok();
        let _ = DestroyWindow(probe);
        ok
    }
}

/// Free stretches of a horizontal Windows 11 taskbar in client x: (left of the task list, right
/// of it up to the notification area). `occ` is in screen coordinates, `origin_x` the screen x
/// of the client origin, `margin` the air kept next to Explorer's buttons.
pub fn free_spans(client: RECT, notify_left: i32, occ: crate::taskbar_uia::Occupied, origin_x: i32, margin: i32) -> [(i32, i32); 2] {
    let left_start = occ.left_end.map(|x| x - origin_x + margin).unwrap_or(client.left).max(client.left);
    match occ.middle {
        Some((a, b)) => {
            let (a, b) = (a - origin_x, b - origin_x);
            [(left_start, (a - margin).max(left_start)), ((b + margin).min(notify_left), notify_left)]
        }
        None => [(left_start, notify_left), (left_start, notify_left)],
    }
}

/// How many of the widgets (natural widths, in order) fit, and in which span (index into
/// `spans`): the preferred span when everything fits there, else the other one when everything
/// fits there, else the roomier one with the trailing widgets left out.
pub fn fit(widths: &[i32], edge: i32, gap: i32, spans: [(i32, i32); 2], prefer: usize) -> (usize, usize) {
    let room = |i: usize| (spans[i].1 - spans[i].0).max(0);
    let need = |n: usize| if n == 0 { 0 } else { edge * 2 + widths[..n].iter().sum::<i32>() + gap * (n as i32 - 1) };
    let all = widths.len();
    let other = 1 - prefer;
    if need(all) <= room(prefer) {
        return (all, prefer);
    }
    if need(all) <= room(other) {
        return (all, other);
    }
    let span = if room(other) > room(prefer) { other } else { prefer };
    let mut n = all;
    while n > 0 && need(n) > room(span) {
        n -= 1;
    }
    (n, span)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taskbar_uia::Occupied;

    const CLIENT: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 48 };

    #[test]
    fn spans_around_a_centred_task_list() {
        let occ = Occupied { left_end: Some(170), middle: Some((674, 1247)) };
        let s = free_spans(CLIENT, 1523, occ, 0, 6);
        assert_eq!(s, [(176, 668), (1253, 1523)]);
        // nothing known: the whole width up to the tray, either way
        assert_eq!(free_spans(CLIENT, 1523, Occupied::default(), 0, 6), [(0, 1523), (0, 1523)]);
        // a task list that reaches the tray leaves nothing on the right
        let full = Occupied { left_end: None, middle: Some((100, 1600)) };
        assert_eq!(free_spans(CLIENT, 1523, full, 0, 6)[1], (1523, 1523));
    }

    #[test]
    fn fitting() {
        let widths = [83, 42, 27, 27, 55, 55];
        // total = 12 + 289 + 25 = 326
        assert_eq!(fit(&widths, 6, 5, [(0, 400), (1000, 1400)], 1), (6, 1));
        // the right side is too narrow, the left fits all
        assert_eq!(fit(&widths, 6, 5, [(0, 400), (1300, 1500)], 1), (6, 0));
        // neither fits all: the roomier one, trailing widgets dropped
        let (n, span) = fit(&widths, 6, 5, [(0, 150), (1300, 1500)], 1);
        assert_eq!(span, 1);
        assert!(n < 6 && n > 0);
        // no room at all
        assert_eq!(fit(&widths, 6, 5, [(0, 0), (10, 10)], 1).0, 0);
    }
}
