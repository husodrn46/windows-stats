//! Where Explorer's own taskbar items are, read through UI Automation. Windows 11 draws the task
//! list, Start, search and the widgets button in XAML: they are not windows FindWindowEx could
//! see. The strip uses their bounds to stay out of the way of the task list.
//!
//! UI Automation talks to Explorer across processes and may block while Explorer is busy, so it
//! is only ever used from its own thread (`Watcher`), never from the UI thread that owns the
//! strip inside the taskbar.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::core::{BSTR, VARIANT};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_None, CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition, IUIAutomationElement, TreeScope_Descendants, UIA_AutomationIdPropertyId,
    UIA_BoundingRectanglePropertyId, UIA_ClassNamePropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the hidden window when the occupied span of the taskbar changed.
pub const WM_APP_TASKBAR_ITEMS: u32 = WM_APP + 4;

#[derive(Clone, Debug)]
pub struct Item {
    pub class: String,
    pub name: String,
    pub id: String,
    /// screen coordinates
    pub rect: RECT,
}

fn bstr(r: windows::core::Result<BSTR>) -> String {
    r.map(|b| b.to_string()).unwrap_or_default()
}

fn uia() -> windows::core::Result<IUIAutomation> {
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
}

/// Every UI Automation element below the taskbar window (diagnostics: `--taskbar-items`).
pub fn all_items(tray: HWND) -> windows::core::Result<Vec<Item>> {
    unsafe {
        let a = uia()?;
        let root = a.ElementFromHandle(tray)?;
        let all = root.FindAll(TreeScope_Descendants, &a.CreateTrueCondition()?)?;
        let mut out = Vec::new();
        for i in 0..all.Length()? {
            let e: IUIAutomationElement = all.GetElement(i)?;
            out.push(Item { class: bstr(e.CurrentClassName()), name: bstr(e.CurrentName()), id: bstr(e.CurrentAutomationId()), rect: e.CurrentBoundingRectangle().unwrap_or_default() });
        }
        Ok(out)
    }
}

/// The items that matter for placing the strip, in one cross-process call: task buttons and the
/// fixed buttons around them, with only their class, id and bounds (cached, no live references).
pub struct Query {
    uia: IUIAutomation,
    cond: IUIAutomationCondition,
    cache: IUIAutomationCacheRequest,
}

/// Automation ids of the fixed taskbar buttons (Windows 11).
const BUTTON_IDS: &[&str] = &["StartButton", "SearchButton", "TaskViewButton", "WidgetsButton", "CopilotButton", "ChatButton"];
const TASK_BUTTON_CLASS: &str = "Taskbar.TaskListButtonAutomationPeer";

impl Query {
    pub fn new() -> windows::core::Result<Query> {
        unsafe {
            let uia = uia()?;
            let mut cond = uia.CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from(TASK_BUTTON_CLASS))?;
            for id in BUTTON_IDS {
                let c = uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(*id))?;
                cond = uia.CreateOrCondition(&cond, &c)?;
            }
            let cache = uia.CreateCacheRequest()?;
            cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
            cache.AddProperty(UIA_ClassNamePropertyId)?;
            cache.AddProperty(UIA_AutomationIdPropertyId)?;
            cache.SetAutomationElementMode(AutomationElementMode_None)?;
            Ok(Query { uia, cond, cache })
        }
    }

    pub fn items(&self, tray: HWND) -> windows::core::Result<Vec<Item>> {
        unsafe {
            let root = self.uia.ElementFromHandle(tray)?;
            let arr = root.FindAllBuildCache(TreeScope_Descendants, &self.cond, &self.cache)?;
            let mut out = Vec::new();
            for i in 0..arr.Length()? {
                let e = arr.GetElement(i)?;
                out.push(Item { class: bstr(e.CachedClassName()), name: String::new(), id: bstr(e.CachedAutomationId()), rect: e.CachedBoundingRectangle().unwrap_or_default() });
            }
            Ok(out)
        }
    }
}

/// Run `f` on a fresh MTA thread (COM initialised there) and wait for it.
pub fn with_com<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    std::thread::spawn(move || unsafe {
        let ok = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let r = f();
        if ok {
            CoUninitialize();
        }
        r
    })
    .join()
    .ok()
}

/// Horizontal spans of the taskbar's own items, screen x coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Occupied {
    /// right edge of the items at the left end (widgets / weather button), if any
    pub left_end: Option<i32>,
    /// the task list and the buttons around it (Start, search, task view): (first, last)
    pub middle: Option<(i32, i32)>,
}

/// Classify the items: the middle group is everything from Start to the last task button; the
/// left group is what sits left of Start (the widgets button on a centred taskbar).
pub fn occupied(items: &[Item]) -> Occupied {
    let visible = |r: &RECT| r.right > r.left && r.bottom > r.top;
    let is_widgets = |it: &Item| it.id == "WidgetsButton" || it.class.contains("WidgetsButton");
    let is_middle = |it: &Item| !is_widgets(it) && (it.class == TASK_BUTTON_CLASS || BUTTON_IDS.contains(&it.id.as_str()) || it.class.contains("SearchBox"));
    let mut mid: Option<(i32, i32)> = None;
    for it in items.iter().filter(|it| visible(&it.rect) && is_middle(it)) {
        mid = Some(match mid {
            None => (it.rect.left, it.rect.right),
            Some((a, b)) => (a.min(it.rect.left), b.max(it.rect.right)),
        });
    }
    let left_end = items.iter().filter(|it| visible(&it.rect) && is_widgets(it)).map(|it| it.rect.right).max();
    Occupied { left_end, middle: mid }
}

/// Background watcher. It reads the taskbar items when poked (the hidden window forwards the
/// shell's window created / destroyed notifications, the moments task buttons come and go) and
/// otherwise every 30 seconds (pinned items, a changed setting), and posts
/// `WM_APP_TASKBAR_ITEMS` when the occupied spans changed. Idle, it costs Explorer nothing.
pub struct Watcher {
    latest: Arc<Mutex<Option<Occupied>>>,
    poke: std::sync::mpsc::Sender<()>,
}

/// Re-read this long after a poke: the task list animates new buttons in.
const SETTLE: Duration = Duration::from_millis(450);
const FALLBACK: Duration = Duration::from_secs(30);

impl Watcher {
    pub fn start(hidden: HWND, tray: HWND) -> Watcher {
        let latest: Arc<Mutex<Option<Occupied>>> = Arc::new(Mutex::new(None));
        let (poke, rx) = std::sync::mpsc::channel::<()>();
        let l = latest.clone();
        let (hidden, tray) = (hidden.0 as isize, tray.0 as isize);
        let _ = std::thread::Builder::new().name("taskbar-items".into()).spawn(move || unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let mut last: Option<Occupied> = None;
            let mut failures = 0u32;
            let mut query: Option<Query> = None;
            // reads still to do: the first one now, a second one after a poke once the animation is over
            let mut again = true;
            loop {
                if query.is_none() {
                    query = Query::new().map_err(|e| crate::log!("taskbar items: UI Automation unavailable: {e}")).ok();
                }
                if let Some(q) = &query {
                    match q.items(HWND(tray as *mut _)) {
                        Ok(items) => {
                            failures = 0;
                            let occ = occupied(&items);
                            if last != Some(occ) {
                                last = Some(occ);
                                if let Ok(mut g) = l.lock() {
                                    *g = Some(occ);
                                }
                                let _ = PostMessageW(HWND(hidden as *mut _), WM_APP_TASKBAR_ITEMS, WPARAM(0), LPARAM(0));
                            }
                        }
                        Err(e) => {
                            failures += 1;
                            if failures == 1 || failures % 30 == 0 {
                                crate::log!("taskbar items: UI Automation failed: {e}");
                            }
                        }
                    }
                }
                let wait = if again { SETTLE } else { FALLBACK };
                again = false;
                match rx.recv_timeout(wait) {
                    Ok(()) => {
                        // a burst of pokes (several windows at once) is one read
                        std::thread::sleep(SETTLE);
                        while rx.try_recv().is_ok() {}
                        again = true;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            if com {
                CoUninitialize();
            }
        });
        Watcher { latest, poke }
    }

    /// The task list may have changed: read it again soon.
    pub fn poke(&self) {
        let _ = self.poke.send(());
    }

    pub fn get(&self) -> Option<Occupied> {
        self.latest.lock().ok().and_then(|g| *g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(class: &str, l: i32, r: i32) -> Item {
        Item { class: class.into(), name: String::new(), id: String::new(), rect: RECT { left: l, top: 1032, right: r, bottom: 1080 } }
    }

    #[test]
    fn spans_from_items() {
        let items = vec![
            item("Taskbar.WidgetsButtonAutomationPeer", 0, 160),
            Item { class: "ToggleButton".into(), name: String::new(), id: "StartButton".into(), rect: RECT { left: 680, top: 1032, right: 724, bottom: 1080 } },
            Item { class: "ToggleButton".into(), name: String::new(), id: "WidgetsButton".into(), rect: RECT { left: 0, top: 1032, right: 170, bottom: 1080 } },
            item("Taskbar.TaskListButtonAutomationPeer", 724, 768),
            item("Taskbar.TaskListButtonAutomationPeer", 1196, 1240),
            item("SystemTray.NormalButton", 1600, 1640),
            // collapsed items have empty rects
            item("Taskbar.TaskListButtonAutomationPeer", 0, 0),
        ];
        let o = occupied(&items);
        assert_eq!(o.left_end, Some(170));
        assert_eq!(o.middle, Some((680, 1240)));
    }
}
