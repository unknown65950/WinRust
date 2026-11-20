// Alt+Space opens a search card; results come from the `rs --serve` services (one pipe per drive).
// Type to search, Up/Down (or mouse) to pick, Enter to open, Ctrl+Enter to show in Explorer, Esc to close.
// `rs-launcher --autostart` registers it to start at logon.
//
// Everything is drawn by hand into one back buffer (GDI+ for the anti-aliased shapes, GDI for ClearType
// text), so the window can animate: fade + slide on show, a selection highlight that glides between
// rows, and a height that eases as results come and go.
#![windows_subsystem = "windows"]
#![allow(unsafe_op_in_unsafe_fn)]
#[path = "../common.rs"]
mod common;
#[path = "../regsweep.rs"]
mod regsweep;
use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::DwmSetWindowAttribute,
        Gdi::*,
        GdiPlus::{
            FillModeAlternate, GdipAddPathArc, GdipClosePathFigure, GdipCreateFromHDC, GdipCreatePath, GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteGraphics, GdipDeletePath, GdipFillPath,
            GdipSetSmoothingMode, GdiplusStartup, GdiplusStartupInput, GpBrush, GpGraphics, GpPath, GpSolidFill, SmoothingModeAntiAlias,
        },
    },
    Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT},
    System::{LibraryLoader::GetModuleHandleW, Threading::{AttachThreadInput, GetCurrentThreadId}},
    UI::{
        Input::KeyboardAndMouse::*,
        Shell::{
            FO_DELETE, FOF_ALLOWUNDO, FOF_NOERRORUI, FOF_SILENT, SHFILEINFOW, SHFILEOPSTRUCTW, SHFileOperationW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
            ShellExecuteW,
        },
        WindowsAndMessaging::*,
    },
};

type Rgb = (u8, u8, u8);
// Colours of the WinRust style the topbar gear picks (normal, terminal, dock, tiling): the same palette as the bar.
use winrust_common::style::{pal, style, ui_face};
fn cr(c: u32) -> Rgb {
    (c as u8, (c >> 8) as u8, (c >> 16) as u8)
}
fn mixc(a: u32, b: u32, t: f32) -> u32 {
    let ch = |sh: u32| (((a >> sh) & 255) as f32 + ((((b >> sh) & 255) as f32) - ((a >> sh) & 255) as f32) * t) as u32;
    ch(0) | ch(8) << 8 | ch(16) << 16
}
fn c_bg() -> Rgb {
    cr(pal().bg)
}
fn c_border() -> Rgb {
    cr(pal().line)
}
fn c_sel() -> Rgb {
    cr(pal().active)
}
fn c_accent() -> Rgb {
    cr(pal().accent)
}
fn c_text() -> Rgb {
    cr(pal().text)
}
fn c_dim() -> Rgb {
    cr(pal().dim)
}
fn c_faint() -> Rgb {
    cr(mixc(pal().dim, pal().bg, 0.4))
}
fn c_menu_bg() -> Rgb {
    cr(mixc(pal().bg, pal().line, 0.5))
}
fn c_menu_hot() -> Rgb {
    cr(pal().active)
}

// logical pixels; multiplied by the screen scale when used
const W: i32 = 680;
const INPUT_H: i32 = 64;
const ROW_H: i32 = 52;
const PAD: i32 = 8;
const FOOT_H: i32 = 34;
const MAX_ROWS: usize = 7;
const RADIUS: i32 = 14;

const T_DEBOUNCE: usize = 1;
const T_ANIM: usize = 2;
const T_CARET: usize = 3;

const F_INPUT: usize = 0;
const F_NAME: usize = 1;
const F_PATH: usize = 2;
const F_ICON: usize = 3;
const F_HINT: usize = 4;

#[derive(Default)]
struct St {
    scale: f32,
    round_with_region: bool, // DWM could not round the corners (Windows 10): clip with a region instead
    fonts: [isize; 5],
    text: Vec<u16>,
    results: Vec<String>,
    top: usize, // first visible result
    sel: usize,
    sel_f: f32, // animated selection row, relative to `top`
    height: f32,
    th: f32, // target height
    alpha: f32,
    ta: f32, // target alpha
    slide: f32,
    x: i32,
    y: i32,
    last: Option<Instant>,
    caret: bool,
    mouse: (i32, i32),
    icons: HashMap<String, isize>,
    rename: Option<String>, // full path being renamed in the input box
    saved: Vec<u16>,        // the search text to restore when the rename ends
    busy: bool,             // a dialog is up: losing focus must not close the card
    menu: Option<MenuSt>,
    purge: Option<Purge>,
}
thread_local!(static ST: RefCell<St> = RefCell::new(St::default()));

// Keep borrows short: window messages re-enter the proc, so never call Win32 from inside `f`.
fn st<R>(f: impl FnOnce(&mut St) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn rgb(c: Rgb) -> u32 {
    c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16
}
fn argb(c: Rgb) -> u32 {
    0xff00_0000 | (c.0 as u32) << 16 | (c.1 as u32) << 8 | c.2 as u32
}

/// Asks every drive's service, in drive order; a drive whose service is down or busy is skipped.
fn ask(q: &str) -> Vec<String> {
    common::drive_letters()
        .into_iter()
        .filter_map(|d| common::ask(d, q)?.ok())
        .flat_map(|s| s.lines().filter(|l| !l.is_empty() && !l.starts_with("... ")).map(String::from).collect::<Vec<_>>())
        .collect()
}

fn is_app(path: &str) -> bool {
    let l = path.to_lowercase();
    l.ends_with(".lnk") && l.contains("\\start menu\\programs\\")
}

/// (title, subtitle) for a result row.
fn labels(path: &str) -> (String, String) {
    let (dir, file) = path.rsplit_once('\\').unwrap_or(("", path));
    if is_app(path) {
        (file.trim_end_matches(".lnk").trim_end_matches(".LNK").to_string(), "Application".into())
    } else {
        (file.to_string(), dir.to_string())
    }
}

fn target_height(s: &St) -> f32 {
    let p = |v: i32| v as f32 * s.scale;
    let rows = s.results.len().min(MAX_ROWS).max(1) as i32;
    let base = if s.text.is_empty() { p(INPUT_H) } else { p(INPUT_H + PAD + rows * ROW_H + if s.results.is_empty() { PAD } else { FOOT_H }) };
    if s.purge.is_some() {
        p(INPUT_H + PAD + PANEL_H)
    } else if s.menu.is_some() {
        base.max(p(INPUT_H + PAD + MENU_PAD * 2 + MENU.len() as i32 * MENU_ITEM_H + 6))
    } else {
        base
    }
}

unsafe fn start_anim(h: HWND) {
    SetTimer(h, T_ANIM, 15, None);
}

unsafe fn focus(h: HWND) {
    if SetForegroundWindow(h) != 0 && GetForegroundWindow() == h {
        SetFocus(h);
        return;
    }
    let me = GetCurrentThreadId();
    let ft = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
    let attached = ft != 0 && ft != me && AttachThreadInput(me, ft, 1) != 0;
    SetForegroundWindow(h);
    if attached {
        AttachThreadInput(me, ft, 0);
    }
    SetFocus(h);
}

unsafe fn show(h: HWND) {
    let (x, y) = {
        let mut pt: POINT = zeroed();
        GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        let w = st(|s| (W as f32 * s.scale) as i32);
        (mi.rcWork.left + (mi.rcWork.right - mi.rcWork.left - w) / 2, mi.rcWork.top + (mi.rcWork.bottom - mi.rcWork.top) / 5)
    };
    st(|s| {
        s.text.clear();
        s.results.clear();
        (s.rename, s.menu) = (None, None);
        if matches!(s.purge, Some(Purge::Confirm { .. })) {
            s.purge = None;
        }
        (s.top, s.sel, s.sel_f, s.caret) = (0, 0, 0.0, true);
        s.th = target_height(s);
        (s.height, s.alpha, s.ta, s.slide, s.x, s.y, s.last) = (s.th, 0.0, 1.0, 16.0 * s.scale, x, y, None);
    });
    let (w, hgt, slide) = st(|s| ((W as f32 * s.scale) as i32, s.height as i32, s.slide as i32));
    SetLayeredWindowAttributes(h, 0, 0, LWA_ALPHA);
    SetWindowPos(h, HWND_TOPMOST, x, y + slide, w, hgt, SWP_SHOWWINDOW);
    focus(h);
    SetTimer(h, T_CARET, 530, None);
    start_anim(h);
}

unsafe fn hide(h: HWND) {
    st(|s| s.ta = 0.0);
    start_anim(h);
}

fn ease(v: &mut f32, target: f32, k: f32, eps: f32, moving: &mut bool) {
    *v += (target - *v) * k;
    if (*v - target).abs() < eps {
        *v = target;
    } else {
        *moving = true;
    }
}

unsafe fn step(h: HWND) {
    let (alpha, x, y, w, hgt, moving, gone, region) = st(|s| {
        let now = Instant::now();
        let dt = s.last.map_or(0.016, |l| (now - l).as_secs_f32()).min(0.05);
        s.last = Some(now);
        let (k, ks) = (1.0 - (-dt * 20.0).exp(), 1.0 - (-dt * 32.0).exp());
        let mut moving = false;
        let want_row = (s.sel - s.top) as f32;
        ease(&mut s.alpha, s.ta, k, 0.004, &mut moving);
        ease(&mut s.height, s.th, k, 0.6, &mut moving);
        ease(&mut s.slide, 0.0, k, 0.6, &mut moving);
        ease(&mut s.sel_f, want_row, ks, 0.01, &mut moving);
        let gone = s.ta == 0.0 && s.alpha == 0.0;
        (s.alpha, s.x, s.y + s.slide as i32, (W as f32 * s.scale) as i32, s.height.round() as i32, moving, gone, s.round_with_region)
    });
    SetLayeredWindowAttributes(h, 0, (alpha * 255.0).round() as u8, LWA_ALPHA);
    SetWindowPos(h, HWND_TOPMOST, x, y, w, hgt, SWP_NOACTIVATE);
    if region {
        let r = (RADIUS as f32 * st(|s| s.scale)) as i32 * 2;
        SetWindowRgn(h, CreateRoundRectRgn(0, 0, w + 1, hgt + 1, r, r), 1);
    }
    InvalidateRect(h, null(), 0);
    if gone {
        ShowWindow(h, SW_HIDE);
        KillTimer(h, T_ANIM);
        KillTimer(h, T_CARET);
    } else if !moving {
        KillTimer(h, T_ANIM);
    }
}

// The pipe query runs on a worker thread: a service that is slow or stuck must never freeze the card
// (Windows killed the launcher once for exactly that). Only the newest query's answer is applied.
const WM_RESULTS: u32 = WM_APP + 8;
static QUERY_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ANSWER: std::sync::Mutex<Option<(u64, Vec<String>)>> = std::sync::Mutex::new(None);

unsafe fn refresh(h: HWND) {
    use std::sync::atomic::Ordering::SeqCst;
    let q = st(|s| String::from_utf16_lossy(&s.text));
    let seq = QUERY_SEQ.fetch_add(1, SeqCst) + 1;
    if q.trim().is_empty() {
        return apply_results(h, vec![]);
    }
    let hw = h as isize;
    std::thread::spawn(move || {
        let res = ask(&q);
        if QUERY_SEQ.load(SeqCst) == seq {
            *ANSWER.lock().unwrap() = Some((seq, res));
            unsafe { PostMessageW(hw as HWND, WM_RESULTS, 0, 0) };
        }
    });
}

unsafe fn apply_results(h: HWND, res: Vec<String>) {
    st(|s| {
        s.results = res;
        s.menu = None;
        (s.top, s.sel, s.sel_f) = (0, 0, 0.0);
        s.th = target_height(s);
    });
    start_anim(h);
}

unsafe fn open(h: HWND, reveal: bool) {
    let Some(p) = st(|s| if s.rename.is_some() { None } else { s.results.get(s.sel).cloned() }) else { return };
    hide(h);
    if reveal {
        ShellExecuteW(null_mut(), wide("open").as_ptr(), wide("explorer.exe").as_ptr(), wide(&format!("/select,\"{p}\"")).as_ptr(), null(), SW_SHOWNORMAL);
    } else {
        ShellExecuteW(null_mut(), wide("open").as_ptr(), wide(&p).as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

fn move_sel(delta: isize) {
    st(|s| {
        if s.results.is_empty() || s.rename.is_some() {
            return;
        }
        s.sel = (s.sel as isize + delta).clamp(0, s.results.len() as isize - 1) as usize;
        if s.sel >= s.top + MAX_ROWS {
            s.top = s.sel + 1 - MAX_ROWS;
        }
        if s.sel < s.top {
            s.top = s.sel;
        }
    });
}

fn row_at(y: i32) -> Option<usize> {
    st(|s| {
        let rel = y as f32 - (INPUT_H + PAD) as f32 * s.scale;
        let row = (rel / (ROW_H as f32 * s.scale)).floor();
        (s.purge.is_none() && rel >= 0.0 && (row as usize) < MAX_ROWS && s.top + (row as usize) < s.results.len()).then(|| s.top + row as usize)
    })
}

unsafe fn on_char(h: HWND, c: u16) {
    if c >= 0x20 && c != 0x7f && st(|s| s.menu.is_none() && s.purge.is_none()) {
        st(|s| {
            if s.text.len() < 200 {
                s.text.push(c);
            }
        });
        text_changed(h);
    }
}

unsafe fn backspace(h: HWND, word: bool) {
    st(|s| {
        let drop_one = |t: &mut Vec<u16>| {
            if let Some(c) = t.pop() {
                if (0xDC00..0xE000).contains(&c) {
                    t.pop(); // second half of a surrogate pair
                }
            }
        };
        drop_one(&mut s.text);
        if word {
            while s.text.last().is_some_and(|c| *c != 0x20) {
                drop_one(&mut s.text);
            }
        }
        s.caret = true;
    });
    text_changed(h);
}

unsafe fn text_changed(h: HWND) {
    QUERY_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst); // any answer still in flight is for older text
    if st(|s| s.rename.is_some()) {
        // editing a file name: the results must stay put
    } else if st(|s| s.text.is_empty()) {
        KillTimer(h, T_DEBOUNCE);
        st(|s| {
            s.results.clear();
            (s.top, s.sel, s.sel_f) = (0, 0, 0.0);
            s.th = target_height(s);
        });
        start_anim(h);
    } else {
        SetTimer(h, T_DEBOUNCE, 70, None); // wait for a pause in typing before asking
    }
    InvalidateRect(h, null(), 0);
}

/// Fonts and frame of the current WinRust style: at start, and again when the topbar broadcasts a style change.
unsafe fn apply_style(main: HWND) {
    let scale = st(|s| s.scale);
    let font = |px: i32, weight: i32, face: &str| {
        CreateFontW(-((px as f32 * scale).round() as i32), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET as _, 0, 0, CLEARTYPE_QUALITY as _, 0, wide(face).as_ptr()) as isize
    };
    let face = ui_face();
    let fonts = [font(22, 400, face), font(16, 600, face), font(13, 400, face), font(20, 400, winrust_common::style::icon_face()), font(12, 400, face)];
    for f in st(|s| std::mem::replace(&mut s.fonts, fonts)) {
        if f != 0 {
            DeleteObject(f as _);
        }
    }
    // Windows 11: rounded corners (square in the terminal and tiling styles), dark frame, border colour. Windows 10 rejects the corner attribute.
    let square = style().square();
    let (round, dark, border) = (if square { 1i32 } else { 2i32 }, !style().light() as i32, rgb(c_border()) as i32);
    let hr = DwmSetWindowAttribute(main, 33, &round as *const _ as _, 4);
    DwmSetWindowAttribute(main, 20, &dark as *const _ as _, 4);
    DwmSetWindowAttribute(main, 34, &border as *const _ as _, 4);
    st(|s| s.round_with_region = hr != 0 && !square);
    if square {
        SetWindowRgn(main, null_mut(), 1);
    }
}

unsafe extern "system" fn wndproc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if m == winrust_common::style::msg() {
        winrust_common::style::reload();
        apply_style(h);
        InvalidateRect(h, null(), 0);
        return 0;
    }
    match m {
        WM_HOTKEY => {
            if IsWindowVisible(h) != 0 && st(|s| s.ta) > 0.0 { hide(h) } else { show(h) }
            0
        }
        WM_ACTIVATE => {
            if (wp & 0xffff) as u32 == WA_INACTIVE && st(|s| s.ta > 0.0 && !s.busy && !matches!(s.purge, Some(Purge::Scanning(_) | Purge::Working(_)))) {
                hide(h);
            }
            0
        }
        WM_TIMER => {
            match wp {
                T_DEBOUNCE => {
                    KillTimer(h, T_DEBOUNCE);
                    refresh(h);
                }
                T_ANIM => step(h),
                T_CARET => {
                    st(|s| s.caret = !s.caret);
                    InvalidateRect(h, null(), 0);
                }
                _ => {}
            }
            0
        }
        WM_CHAR => {
            on_char(h, wp as u16);
            0
        }
        WM_PURGE => {
            let p = *Box::from_raw(lp as *mut Purge);
            set_purge(h, Some(p));
            0
        }
        WM_KEYDOWN => {
            if st(|s| s.purge.is_some()) {
                let idle = st(|s| matches!(s.purge, Some(Purge::Confirm { .. } | Purge::Done { .. })));
                match wp as u16 {
                    VK_ESCAPE if idle => close_purge(h),
                    VK_ESCAPE => hide(h), // the work goes on; its result is shown on the next open
                    VK_RETURN => purge_press(h, st(|s| match &s.purge {
                        Some(Purge::Confirm { hot, .. } | Purge::Done { hot, .. }) => *hot,
                        _ => 0,
                    })),
                    VK_LEFT | VK_RIGHT | VK_TAB => {
                        let n = st(|s| purge_buttons(s).len());
                        st(|s| {
                            if let Some(Purge::Confirm { hot, .. } | Purge::Done { hot, .. }) = s.purge.as_mut() {
                                *hot = if n > 1 { 1 - *hot } else { 0 };
                            }
                        });
                        InvalidateRect(h, null(), 0);
                    }
                    _ => {}
                }
                return 0;
            }
            if st(|s| s.menu.is_some()) {
                match wp as u16 {
                    VK_ESCAPE => close_menu(h),
                    VK_RETURN => run_menu(h, st(|s| s.menu.as_ref().map_or(0, |m| m.hot))),
                    k @ (VK_UP | VK_DOWN) => {
                        st(|s| {
                            if let Some(m) = s.menu.as_mut() {
                                m.hot = if k == VK_DOWN { (m.hot + 1) % MENU.len() } else { (m.hot + MENU.len() - 1) % MENU.len() };
                            }
                        });
                        InvalidateRect(h, null(), 0);
                    }
                    _ => {}
                }
                return 0;
            }
            let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
            match wp as u16 {
                VK_ESCAPE => {
                    if st(|s| s.rename.is_some()) { end_rename(h, None) } else { hide(h) }
                }
                VK_RETURN => {
                    if st(|s| s.rename.is_some()) { finish_rename(h) } else { open(h, ctrl) }
                }
                VK_DOWN => move_sel(1),
                VK_UP => move_sel(-1),
                VK_NEXT => move_sel(MAX_ROWS as isize),
                VK_PRIOR => move_sel(-(MAX_ROWS as isize)),
                VK_BACK => backspace(h, ctrl),
                _ => return 0,
            }
            start_anim(h);
            0
        }
        WM_MOUSEMOVE => {
            // Windows also sends this when the content under a *still* cursor changes (window appears,
            // rows move), and a resting hand jitters: only a real movement (>= 8 px) may take the
            // selection away from the keyboard.
            let pos = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            if st(|s| s.purge.is_some()) {
                if let Some(i) = purge_hit(pos.0, pos.1) {
                    let changed = st(|s| match s.purge.as_mut() {
                        Some(Purge::Confirm { hot, .. } | Purge::Done { hot, .. }) => std::mem::replace(hot, i) != i,
                        _ => false,
                    });
                    if changed {
                        InvalidateRect(h, null(), 0);
                    }
                }
                return 0;
            }
            if let Some(hit) = menu_hit(pos.0, pos.1) {
                if let Some(i) = hit {
                    if st(|s| s.menu.as_mut().is_some_and(|m| std::mem::replace(&mut m.hot, i) != i)) {
                        InvalidateRect(h, null(), 0);
                    }
                }
                return 0;
            }
            let moved = st(|s| {
                let far = (pos.0 - s.mouse.0).abs() + (pos.1 - s.mouse.1).abs() >= 8;
                if far {
                    s.mouse = pos;
                }
                far
            });
            if let Some(i) = moved.then(|| row_at(pos.1)).flatten() {
                if st(|s| std::mem::replace(&mut s.sel, i)) != i {
                    start_anim(h);
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            if st(|s| s.menu.is_some() || s.purge.is_some()) {
                return 0;
            }
            move_sel(if ((wp >> 16) as i16) > 0 { -1 } else { 1 });
            start_anim(h);
            0
        }
        WM_LBUTTONUP => {
            if st(|s| s.purge.is_some()) {
                if let Some(i) = purge_hit((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32) {
                    purge_press(h, i);
                }
                return 0;
            }
            match menu_hit((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32) {
                Some(Some(i)) => return { run_menu(h, i); 0 },
                Some(None) => return { close_menu(h); 0 },
                None => {}
            }
            if let Some(i) = row_at(((lp >> 16) & 0xffff) as i16 as i32) {
                st(|s| s.sel = i);
                open(h, false);
            }
            0
        }
        WM_RBUTTONUP => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            match row_at(y) {
                Some(i) if st(|s| s.rename.is_none() && s.purge.is_none()) => open_menu(h, i, x, y),
                _ => close_menu(h),
            }
            0
        }
        WM_RESULTS => {
            let a = ANSWER.lock().unwrap().take();
            // drop answers to queries the user has typed past, or that arrive after the card closed
            if let Some((seq, res)) = a {
                if seq == QUERY_SEQ.load(std::sync::atomic::Ordering::SeqCst) && st(|s| s.ta > 0.0 && s.rename.is_none()) {
                    apply_results(h, res);
                }
            }
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(h);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(h, m, wp, lp),
    }
}

// ---------------------------------------------------------------- right-click actions

// item 3 is "Add to Start" / "Remove from Start" (the label is picked when drawing)
const MENU: [&str; 7] = ["Open", "Run as administrator", "Show in folder", "Add to Start", "Rename", "Move to Recycle Bin", "Delete forever + clean registry"];

/// topbar's Start list: one path per line (the same file topbar reads when its Start panel opens).
fn start_file() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("topbar-start.txt")
}

fn start_pins() -> Vec<String> {
    fs::read_to_string(start_file()).map(|s| s.lines().filter(|l| !l.is_empty()).map(String::from).collect()).unwrap_or_default()
}

fn start_pinned(p: &str) -> bool {
    start_pins().iter().any(|x| x.eq_ignore_ascii_case(p))
}

fn start_toggle(p: &str) {
    let mut v = start_pins();
    match v.iter().position(|x| x.eq_ignore_ascii_case(p)) {
        Some(i) => drop(v.remove(i)),
        None => v.push(p.to_string()),
    }
    let _ = fs::write(start_file(), v.join("\n"));
}
const MENU_W: i32 = 270;
const MENU_ITEM_H: i32 = 32;
const MENU_PAD: i32 = 6;
const DANGER: Rgb = (248, 113, 113);

struct MenuSt {
    x: i32,
    y: i32,
    hot: usize,
    path: String,
}

/// Modal box over the card; `busy` keeps the card from closing when it loses focus to the box.
unsafe fn ask_box(h: HWND, text: &str, flags: u32) -> i32 {
    st(|s| s.busy = true);
    let r = MessageBoxW(h, wide(text).as_ptr(), wide("rs launcher").as_ptr(), flags | MB_TOPMOST | MB_SETFOREGROUND);
    st(|s| s.busy = false);
    r
}

/// Drive roots, system and profile folders are never touched from here.
fn protected(p: &str) -> bool {
    let l = p.trim_end_matches('\\').to_lowercase();
    if !l.contains('\\') {
        return true; // "c:"
    }
    let var = |k: &str| std::env::var(k).ok().map(|v| v.trim_end_matches('\\').to_lowercase());
    let win = var("SystemRoot");
    if win.as_ref().is_some_and(|w| l == *w || l.starts_with(&format!("{w}\\"))) {
        return true;
    }
    let profile = var("USERPROFILE");
    let users = profile.as_ref().and_then(|p| p.rsplit_once('\\').map(|(d, _)| d.to_string()));
    ["ProgramFiles", "ProgramFiles(x86)", "ProgramData"].iter().filter_map(|k| var(k)).chain(profile).chain(users).any(|x| l == x)
}

// The menu is drawn inside the card (no system popup: that one is light-themed and its modal loop
// stalls the animation timers). x/y are client pixels of the click.
fn menu_rect(s: &St, m: &MenuSt) -> (i32, i32, i32, i32) {
    let p = |v: i32| (v as f32 * s.scale).round() as i32;
    let (w, h) = (p(MENU_W), p(MENU_PAD) * 2 + MENU.len() as i32 * p(MENU_ITEM_H));
    let l = m.x.min(p(W) - w - p(6)).max(p(6));
    let t = m.y.min(s.height.round() as i32 - h - p(6)).max(0);
    (l, t, l + w, t + h)
}

/// None = no menu open; Some(None) = click outside it; Some(Some(i)) = on item i.
fn menu_hit(x: i32, y: i32) -> Option<Option<usize>> {
    st(|s| {
        let m = s.menu.as_ref()?;
        let (l, t, r, b) = menu_rect(s, m);
        let p = |v: i32| (v as f32 * s.scale).round() as i32;
        Some((x >= l && x < r && y >= t && y < b).then(|| (((y - t - p(MENU_PAD)) / p(MENU_ITEM_H)).max(0) as usize).min(MENU.len() - 1)))
    })
}

unsafe fn open_menu(h: HWND, row: usize, x: i32, y: i32) {
    st(|s| {
        if let Some(path) = s.results.get(row).cloned() {
            s.sel = row;
            s.menu = Some(MenuSt { x, y, hot: 0, path });
            s.th = target_height(s); // room for the menu even under a single result
        }
    });
    start_anim(h);
}

unsafe fn close_menu(h: HWND) {
    st(|s| {
        if s.menu.take().is_some() {
            s.th = target_height(s);
        }
    });
    start_anim(h);
    InvalidateRect(h, null(), 0);
}

unsafe fn run_menu(h: HWND, i: usize) {
    let Some(p) = st(|s| s.menu.as_ref().map(|m| m.path.clone())) else { return };
    close_menu(h);
    match i {
        0 => open(h, false),
        1 => {
            hide(h);
            ShellExecuteW(null_mut(), wide("runas").as_ptr(), wide(&p).as_ptr(), null(), null(), SW_SHOWNORMAL);
        }
        2 => open(h, true),
        3 => start_toggle(&p),
        4 => begin_rename(h, &p),
        _ => remove(h, &p, i == 6),
    }
}

/// Rename happens in the input box itself: it shows the current name, Enter applies, Esc cancels.
unsafe fn begin_rename(h: HWND, p: &str) {
    if protected(p) {
        ask_box(h, "This is a protected system location.", MB_OK | MB_ICONWARNING);
        return;
    }
    KillTimer(h, T_DEBOUNCE);
    st(|s| {
        let name = p.rsplit('\\').next().unwrap_or(p);
        s.saved = std::mem::replace(&mut s.text, name.encode_utf16().collect());
        s.rename = Some(p.to_string());
        s.caret = true;
    });
    InvalidateRect(h, null(), 0);
}

unsafe fn end_rename(h: HWND, renamed_to: Option<String>) {
    st(|s| {
        if let (Some(old), Some(new)) = (s.rename.take(), renamed_to) {
            if let Some(r) = s.results.iter_mut().find(|r| **r == old) {
                *r = new;
            }
        }
        s.text = std::mem::take(&mut s.saved);
    });
    InvalidateRect(h, null(), 0);
}

unsafe fn finish_rename(h: HWND) {
    let Some(old) = st(|s| s.rename.clone()) else { return };
    let new = st(|s| String::from_utf16_lossy(&s.text)).trim().to_string();
    let (dir, cur) = old.rsplit_once('\\').unwrap_or(("", &old));
    if new.is_empty() || new.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
        ask_box(h, "A name cannot be empty or contain \\ / : * ? \" < > |", MB_OK | MB_ICONWARNING);
        return; // stay in rename mode so it can be fixed
    }
    let dst = format!("{dir}\\{new}");
    if new != cur {
        if let Err(e) = fs::rename(&old, &dst) {
            ask_box(h, &format!("Cannot rename:\n{e}"), MB_OK | MB_ICONERROR);
            return;
        }
    }
    end_rename(h, Some(dst));
}

fn recycle(p: &str) -> Result<(), String> {
    let from: Vec<u16> = p.encode_utf16().chain([0, 0]).collect(); // double-NUL list
    let mut op: SHFILEOPSTRUCTW = unsafe { zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = from.as_ptr();
    // no FOF_NOCONFIRMATION: when an item is too big for the bin Windows asks before deleting it for good
    op.fFlags = (FOF_ALLOWUNDO | FOF_SILENT | FOF_NOERRORUI) as u16;
    let rc = unsafe { SHFileOperationW(&mut op) };
    let aborted = op.fAnyOperationsAborted;
    if rc != 0 || aborted != 0 { Err(format!("Recycle Bin refused it (code {rc})")) } else { Ok(()) }
}

fn note(text: &str, flags: u32) {
    unsafe { MessageBoxW(null_mut(), wide(text).as_ptr(), wide("rs launcher").as_ptr(), flags | MB_TOPMOST | MB_SETFOREGROUND) };
}

/// Recycle Bin: reversible, no question asked. "Delete forever" opens the purge panel instead.
unsafe fn remove(h: HWND, p: &str, purge: bool) {
    if protected(p) {
        ask_box(h, "This is a protected system location.", MB_OK | MB_ICONWARNING);
        return;
    }
    if purge {
        return begin_purge(h, p);
    }
    hide(h);
    let p = p.to_string();
    std::thread::spawn(move || {
        if let Err(e) = recycle(&p) {
            note(&format!("Cannot delete:\n{e}"), MB_OK | MB_ICONERROR);
        }
    });
}

// ---------------------------------------------------------------- delete forever
//
// Drawn inside the card, in three stages, never touching anything before the user has seen the list:
//   Scanning  count the files, find the registry entries (read-only, on a thread)
//   Confirm   show all of it; Cancel is the default button
//   Working   delete the files; only if every one is gone: back up the registry entries to a .reg, then remove them
//   Done      what happened, with the backup file one click away

const WM_PURGE: u32 = WM_APP + 7; // lParam = Box<Purge> from a worker thread
const PANEL_H: i32 = 300;
const LIST_MAX: usize = 4;

enum Purge {
    Scanning(String),
    Confirm { path: String, files: u64, bytes: u64, hits: Vec<regsweep::Hit>, hot: usize }, // hot: 0 Cancel, 1 Delete
    Working(String),
    Done { path: String, title: String, lines: Vec<String>, ok: bool, backup: Option<String>, hot: usize }, // hot: 0 Close, 1 Show backup
}

fn post(h: isize, p: Purge) {
    unsafe { PostMessageW(h as HWND, WM_PURGE, 0, Box::into_raw(Box::new(p)) as isize) };
}

/// (files, bytes) under `p`; links are counted, never followed.
fn measure(p: &std::path::Path) -> (u64, u64) {
    use std::os::windows::fs::MetadataExt;
    let Ok(m) = fs::symlink_metadata(p) else { return (0, 0) };
    if m.file_attributes() & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != FILE_ATTRIBUTE_DIRECTORY {
        return (1, m.len());
    }
    fs::read_dir(p).map_or((0, 0), |rd| rd.flatten().map(|e| measure(&e.path())).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1)))
}

/// Deletes `p` bottom-up, carrying on past failures. Read-only files are unlocked first; junctions and
/// symlinks are removed as links (what they point to is never touched). Returns what could not be removed.
fn wipe(p: &std::path::Path, failed: &mut Vec<String>) {
    use std::os::windows::fs::MetadataExt;
    let Ok(m) = fs::symlink_metadata(p) else { return }; // already gone
    let attrs = m.file_attributes();
    let (dir, link) = (attrs & FILE_ATTRIBUTE_DIRECTORY != 0, attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0);
    let before = failed.len();
    if dir && !link {
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                wipe(&e.path(), failed);
            }
        }
        if failed.len() > before {
            return; // a child stayed, so the folder cannot go either; the child is already reported
        }
    }
    if !link && m.permissions().readonly() {
        let mut perm = m.permissions();
        perm.set_readonly(false);
        let _ = fs::set_permissions(p, perm);
    }
    if let Err(e) = if dir { fs::remove_dir(p) } else { fs::remove_file(p) } {
        failed.push(format!("{}  ({})", p.display(), e.kind()));
    }
}

fn size_text(b: u64) -> String {
    let (v, u) = match b {
        0..1024 => return format!("{b} B"),
        ..1_048_576 => (b as f64 / 1024.0, "KB"),
        ..1_073_741_824 => (b as f64 / 1_048_576.0, "MB"),
        _ => (b as f64 / 1_073_741_824.0, "GB"),
    };
    format!("{v:.1} {u}")
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// %LOCALAPPDATA%\rs-search\registry-backups\2026-11-15 10-42-07  name.reg
fn backup_path(p: &str) -> std::path::PathBuf {
    let mut t = unsafe { zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    let t: windows_sys::Win32::Foundation::SYSTEMTIME = t;
    let name: String = p.rsplit('\\').next().unwrap_or("item").chars().map(|c| if c.is_alphanumeric() || " .-_".contains(c) { c } else { '_' }).take(60).collect();
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    std::path::Path::new(&base)
        .join(r"rs-search\registry-backups")
        .join(format!("{:04}-{:02}-{:02} {:02}-{:02}-{:02}  {name}.reg", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond))
}

unsafe fn set_purge(h: HWND, p: Option<Purge>) {
    st(|s| {
        s.menu = None;
        s.purge = p;
        s.th = target_height(s);
    });
    start_anim(h);
    InvalidateRect(h, null(), 0);
}

unsafe fn begin_purge(h: HWND, p: &str) {
    set_purge(h, Some(Purge::Scanning(p.into())));
    let (hw, path) = (h as isize, p.to_string());
    std::thread::spawn(move || {
        let (files, bytes) = measure(std::path::Path::new(&path));
        let hits = regsweep::scan(&path);
        post(hw, Purge::Confirm { path, files, bytes, hits, hot: 0 });
    });
}

unsafe fn run_purge(h: HWND) {
    let Some((path, hits)) = st(|s| match s.purge.take() {
        Some(Purge::Confirm { path, hits, .. }) => Some((path, hits)),
        other => {
            s.purge = other;
            None
        }
    }) else {
        return;
    };
    set_purge(h, Some(Purge::Working(path.clone())));
    let hw = h as isize;
    std::thread::spawn(move || {
        let (files, bytes) = measure(std::path::Path::new(&path));
        let mut failed = Vec::new();
        wipe(std::path::Path::new(&path), &mut failed);
        let done = |title: &str, lines: Vec<String>, ok: bool, backup: Option<String>| Purge::Done { path: path.clone(), title: title.into(), lines, ok, backup, hot: 0 };
        if !failed.is_empty() {
            let mut lines = vec![format!("{} could not be removed (in use or access denied):", plural(failed.len(), "item"))];
            lines.extend(failed.iter().take(3).cloned());
            if failed.len() > 3 {
                lines.push(format!("... and {} more", failed.len() - 3));
            }
            lines.push("The registry was not touched. Close the program using them and try again.".into());
            return post(hw, done("Some files are still there", lines, false, None));
        }
        let mut lines = vec![format!("{} removed ({})", plural(files as usize, "file"), size_text(bytes))];
        if hits.is_empty() {
            lines.push("No registry entries pointed to it.".into());
            return post(hw, done("Deleted", lines, true, None));
        }
        let bak = backup_path(&path);
        if let Err(e) = regsweep::backup(&hits, &bak) {
            lines.push(format!("Registry left as is: the backup could not be written ({e})."));
            return post(hw, done("Deleted, registry kept", lines, false, None));
        }
        let r = regsweep::delete(&hits);
        lines.push(format!("{} removed", plural(r.removed, "registry entry").replace("entrys", "entries")));
        if r.denied > 0 {
            lines.push(format!("{} need administrator rights and were left in place", r.denied));
        }
        post(hw, done("Deleted", lines, r.denied == 0, Some(bak.display().to_string())));
    });
}

/// Enter / click on the panel's button `i`.
unsafe fn purge_press(h: HWND, i: usize) {
    let action = st(|s| match &s.purge {
        Some(Purge::Confirm { .. }) => Some(if i == 1 { 1 } else { 0 }),
        Some(Purge::Done { backup, .. }) => Some(if i == 1 && backup.is_some() { 2 } else { 3 }),
        _ => None,
    });
    match action {
        Some(1) => run_purge(h),
        Some(2) => {
            let b = st(|s| match &s.purge {
                Some(Purge::Done { backup, .. }) => backup.clone(),
                _ => None,
            });
            if let Some(b) = b {
                hide(h);
                ShellExecuteW(null_mut(), wide("open").as_ptr(), wide("explorer.exe").as_ptr(), wide(&format!("/select,\"{b}\"")).as_ptr(), null(), SW_SHOWNORMAL);
            }
        }
        Some(_) => close_purge(h),
        None => {}
    }
}

/// Closes the panel; a path that is gone also leaves the result list.
unsafe fn close_purge(h: HWND) {
    st(|s| {
        if let Some(Purge::Done { path, .. }) = &s.purge {
            if fs::symlink_metadata(path).is_err() {
                let p = path.clone();
                s.results.retain(|r| *r != p);
                s.sel = s.sel.min(s.results.len().saturating_sub(1));
                s.top = s.top.min(s.sel);
            }
        }
    });
    set_purge(h, None);
}

/// Button rectangles of the panel (client px): [left one, right one]; the right one may be absent.
fn purge_buttons(s: &St) -> Vec<(i32, i32, i32, i32)> {
    let p = |v: i32| (v as f32 * s.scale).round() as i32;
    let (w, bottom) = (p(W), p(INPUT_H + PAD + PANEL_H));
    let (bw, bh) = (p(150), p(36));
    let (t, b) = (bottom - p(20) - bh, bottom - p(20));
    let right = (w - p(22) - bw, t, w - p(22), b);
    let left = (right.0 - p(10) - p(110), t, right.0 - p(10), b);
    match &s.purge {
        Some(Purge::Confirm { .. }) => vec![left, right],
        Some(Purge::Done { backup: Some(_), .. }) => vec![left, right],
        Some(Purge::Done { .. }) => vec![(w - p(22) - p(110), t, w - p(22), b)],
        _ => vec![],
    }
}

fn purge_hit(x: i32, y: i32) -> Option<usize> {
    st(|s| purge_buttons(s).iter().position(|&(l, t, r, b)| x >= l && x < r && y >= t && y < b))
}

// ---------------------------------------------------------------- drawing

struct View {
    scale: f32,
    fonts: [isize; 5],
    text: Vec<u16>,
    rows: Vec<(String, String, isize)>, // title, subtitle, icon
    total: usize,
    sel_f: f32,
    caret: bool,
    renaming: bool,
    menu: Option<(i32, i32, usize)>, // left, top, hot item
    menu_pinned: bool, // the menu's file is already in Start: item 3 reads "Remove from Start"
    purge: Option<PView>,
}

/// What the purge panel shows, snapshotted for one paint.
struct PView {
    glyph: (&'static str, Rgb),
    title: (String, Rgb),
    path: String,
    lines: Vec<(String, Rgb, bool)>, // text, colour, small
    note: String,
    buttons: Vec<((i32, i32, i32, i32), &'static str, bool, bool)>, // rect, label, danger, hot
}

fn purge_view(s: &St) -> Option<PView> {
    let rects = purge_buttons(s);
    let btn = |labels: &[(&'static str, bool)], hot: usize| rects.iter().zip(labels).enumerate().map(|(i, (r, (l, d)))| (*r, *l, *d, i == hot)).collect::<Vec<_>>();
    Some(match s.purge.as_ref()? {
        Purge::Scanning(path) => PView {
            glyph: ("\u{E721}", c_dim()),
            title: ("Checking what will be removed\u{2026}".into(), c_text()),
            path: path.clone(),
            lines: vec![("Counting files and searching the registry. Nothing is deleted yet.".into(), c_dim(), false)],
            note: String::new(),
            buttons: vec![],
        },
        Purge::Working(path) => PView {
            glyph: ("\u{E74D}", DANGER),
            title: ("Deleting\u{2026}".into(), c_text()),
            path: path.clone(),
            lines: vec![("Esc hides this card; the result waits for the next Alt+Space.".into(), c_dim(), false)],
            note: String::new(),
            buttons: vec![],
        },
        Purge::Confirm { path, files, bytes, hits, hot } => {
            let mut lines = vec![(format!("{}  \u{00B7}  {}", plural(*files as usize, "file"), size_text(*bytes)), c_text(), false)];
            if hits.is_empty() {
                lines.push(("No registry entries point to it.".into(), c_dim(), false));
            } else {
                let n = plural(hits.len(), "registry entry").replace("entrys", "entries");
                lines.push((format!("{n} {} to it and will be removed:", if hits.len() == 1 { "points" } else { "point" }), c_dim(), false));
                lines.extend(hits.iter().take(LIST_MAX).map(|h| (h.describe(), c_faint(), true)));
                if hits.len() > LIST_MAX {
                    lines.push((format!("+ {} more", hits.len() - LIST_MAX), c_faint(), true));
                }
            }
            PView {
                glyph: ("\u{E74D}", DANGER),
                title: ("Delete forever?".into(), DANGER),
                path: path.clone(),
                lines,
                note: if hits.is_empty() { "Files skip the Recycle Bin and cannot be restored.".into() } else { "Files skip the Recycle Bin. Registry entries are saved to a .reg backup first.".into() },
                buttons: btn(&[("Cancel", false), ("Delete forever", true)], *hot),
            }
        }
        Purge::Done { path, title, lines, ok, backup, hot } => PView {
            glyph: if *ok { ("\u{E73E}", c_accent()) } else { ("\u{E7BA}", DANGER) },
            title: (title.clone(), if *ok { c_text() } else { DANGER }),
            path: path.clone(),
            lines: lines.iter().enumerate().map(|(i, l)| (l.clone(), if i == 0 { c_text() } else { c_dim() }, false)).collect(),
            note: backup.as_ref().map_or(String::new(), |b| format!("Backup (double-click to restore): {b}")),
            buttons: if backup.is_some() { btn(&[("Close", false), ("Show backup", false)], *hot) } else { btn(&[("Close", false)], 0) },
        },
    })
}

unsafe fn draw_purge(dc: HDC, w: i32, v: &View, pv: &PView) {
    let s = v.scale;
    let p = |x: i32| (x as f32 * s).round() as i32;
    let top = p(INPUT_H + PAD);
    let mut g: *mut GpGraphics = null_mut();
    GdipCreateFromHDC(dc, &mut g);
    GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
    for ((l, t, r, b), _, danger, hot) in &pv.buttons {
        let (rad, l, t, bw, bh) = (8.0 * s, *l as f32, *t as f32, (*r - *l) as f32, (*b - *t) as f32);
        if *danger {
            // the destructive button stays an outline until it is the chosen one
            round_rect(g, l, t, bw, bh, rad, DANGER);
            if !*hot {
                round_rect(g, l + 1.5 * s, t + 1.5 * s, bw - 3.0 * s, bh - 3.0 * s, rad - 1.5 * s, c_bg());
            }
        } else {
            round_rect(g, l, t, bw, bh, rad, if *hot { c_menu_hot() } else { c_menu_bg() });
        }
    }
    GdipDeleteGraphics(g);
    let left = p(58);
    text(dc, v.fonts[F_ICON], pv.glyph.1, RECT { left: p(22), top: top + p(12), right: p(52), bottom: top + p(40) }, pv.glyph.0, DT_SINGLELINE | DT_VCENTER);
    text(dc, v.fonts[F_NAME], pv.title.1, RECT { left, top: top + p(12), right: w - p(22), bottom: top + p(40) }, &pv.title.0, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
    text(dc, v.fonts[F_PATH], c_dim(), RECT { left, top: top + p(40), right: w - p(22), bottom: top + p(60) }, &pv.path, DT_SINGLELINE | DT_PATH_ELLIPSIS);
    let mut y = top + p(74);
    for (line, c, small) in &pv.lines {
        let hgt = p(if *small { 18 } else { 22 });
        let (font, indent) = if *small { (v.fonts[F_HINT], p(12)) } else { (v.fonts[F_PATH], 0) };
        text(dc, font, *c, RECT { left: left + indent, top: y, right: w - p(22), bottom: y + hgt }, line, DT_SINGLELINE | DT_VCENTER | DT_PATH_ELLIPSIS);
        y += hgt;
    }
    let bottom = top + p(PANEL_H);
    if !pv.note.is_empty() {
        text(dc, v.fonts[F_HINT], c_faint(), RECT { left: p(22), top: bottom - p(20 + 36 + 30), right: w - p(22), bottom: bottom - p(20 + 36 + 8) }, &pv.note, DT_SINGLELINE | DT_VCENTER | DT_PATH_ELLIPSIS);
    }
    for ((l, t, r, b), label, danger, hot) in &pv.buttons {
        let c = if *danger && *hot { (20, 20, 24) } else if *danger { DANGER } else { c_text() };
        text(dc, v.fonts[F_PATH], c, RECT { left: *l, top: *t, right: *r, bottom: *b }, label, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
    }
}


unsafe fn ensure_icons() {
    st(|s| {
        if s.icons.len() > 300 {
            for (_, i) in s.icons.drain() {
                if i != 0 {
                    DestroyIcon(i as _);
                }
            }
        }
    });
    let need: Vec<String> = st(|s| s.results.iter().skip(s.top).take(MAX_ROWS).filter(|p| !s.icons.contains_key(*p)).cloned().collect());
    for p in need {
        let mut fi: SHFILEINFOW = zeroed();
        let path = wide(&p);
        SHGetFileInfoW(path.as_ptr(), 0, &mut fi, size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON);
        if fi.hIcon.is_null() {
            // no icon of its own (unregistered type, vanished file): the generic one for that extension
            SHGetFileInfoW(path.as_ptr(), 0x80, &mut fi, size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES);
        }
        st(|s| {
            s.icons.insert(p, fi.hIcon as isize);
        });
    }
}

unsafe fn paint(h: HWND) {
    ensure_icons();
    let v = st(|s| View {
        scale: s.scale,
        fonts: s.fonts,
        text: s.text.clone(),
        rows: s.results.iter().skip(s.top).take(MAX_ROWS).map(|p| {
            let (a, b) = labels(p);
            (a, b, s.icons.get(p).copied().unwrap_or(0))
        }).collect(),
        total: s.results.len(),
        sel_f: s.sel_f,
        caret: s.caret,
        renaming: s.rename.is_some(),
        menu: s.menu.as_ref().map(|m| {
            let (l, t, ..) = menu_rect(s, m);
            (l, t, m.hot)
        }),
        menu_pinned: s.menu.as_ref().is_some_and(|m| start_pinned(&m.path)),
        purge: purge_view(s),
    });
    let mut ps: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(h, &mut ps);
    let mut rc: RECT = zeroed();
    GetClientRect(h, &mut rc);
    let (w, hgt) = (rc.right, rc.bottom);
    let mem = CreateCompatibleDC(dc);
    let bmp = CreateCompatibleBitmap(dc, w, hgt);
    let old = SelectObject(mem, bmp);
    draw(mem, w, hgt, &v);
    BitBlt(dc, 0, 0, w, hgt, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    DeleteObject(bmp);
    DeleteDC(mem);
    EndPaint(h, &ps);
}

unsafe fn fill(dc: HDC, l: i32, t: i32, r: i32, b: i32, c: Rgb) {
    let br = CreateSolidBrush(rgb(c));
    FillRect(dc, &RECT { left: l, top: t, right: r, bottom: b }, br);
    DeleteObject(br);
}

unsafe fn round_rect(g: *mut GpGraphics, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgb) {
    let r = if style().square() { 0.5 } else { r }; // a hair of a radius: zero-size arcs would drop the whole shape
    let mut path: *mut GpPath = null_mut();
    GdipCreatePath(FillModeAlternate, &mut path);
    let d = r * 2.0;
    GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
    GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
    GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
    GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
    GdipClosePathFigure(path);
    let mut b: *mut GpSolidFill = null_mut();
    GdipCreateSolidFill(argb(c), &mut b);
    GdipFillPath(g, b as *mut GpBrush, path);
    GdipDeleteBrush(b as *mut GpBrush);
    GdipDeletePath(path);
}

unsafe fn text(dc: HDC, font: isize, c: Rgb, mut r: RECT, s: &str, flags: u32) {
    let w: Vec<u16> = s.encode_utf16().collect();
    let old = SelectObject(dc, font as _);
    SetTextColor(dc, rgb(c));
    DrawTextW(dc, w.as_ptr(), w.len() as i32, &mut r, flags | DT_NOPREFIX);
    SelectObject(dc, old);
}

unsafe fn draw(dc: HDC, w: i32, hgt: i32, v: &View) {
    let s = v.scale;
    let p = |x: i32| (x as f32 * s).round() as i32;
    SetBkMode(dc, TRANSPARENT as i32);
    fill(dc, 0, 0, w, hgt, c_bg());

    let (rows_top, row_h) = (p(INPUT_H + PAD), ROW_H as f32 * s);
    let nrows = v.rows.len();

    // anti-aliased shapes first, then text on top
    let mut g: *mut GpGraphics = null_mut();
    GdipCreateFromHDC(dc, &mut g);
    GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
    if nrows > 0 && v.purge.is_none() {
        let y = rows_top as f32 + v.sel_f * row_h;
        round_rect(g, p(PAD) as f32, y + 2.0 * s, (w - 2 * p(PAD)) as f32, row_h - 4.0 * s, 10.0 * s, c_sel());
        round_rect(g, p(PAD + 4) as f32, y + row_h / 2.0 - 10.0 * s, 3.0 * s, 20.0 * s, 1.5 * s, c_accent());
    }
    GdipDeleteGraphics(g);

    // input row
    let mid = p(INPUT_H) / 2;
    text(dc, v.fonts[F_ICON], c_dim(), RECT { left: p(22), top: mid - p(14), right: p(52), bottom: mid + p(14) }, "\u{E721}", DT_SINGLELINE | DT_VCENTER);
    let area = RECT { left: p(58), top: mid - p(18), right: w - p(24), bottom: mid + p(18) };
    let shown = String::from_utf16_lossy(&v.text);
    if v.text.is_empty() {
        text(dc, v.fonts[F_INPUT], c_faint(), area, "Search files and apps", DT_SINGLELINE | DT_VCENTER);
    } else {
        let mut size: SIZE = zeroed();
        let old = SelectObject(dc, v.fonts[F_INPUT] as _);
        GetTextExtentPoint32W(dc, v.text.as_ptr(), v.text.len() as i32, &mut size);
        SelectObject(dc, old);
        let overflow = (size.cx - (area.right - area.left - p(4))).max(0); // keep the end (caret) in view
        let saved = SaveDC(dc);
        IntersectClipRect(dc, area.left, area.top, area.right, area.bottom);
        text(dc, v.fonts[F_INPUT], c_text(), RECT { left: area.left - overflow, ..area }, &shown, DT_SINGLELINE | DT_VCENTER | DT_NOCLIP);
        if v.caret {
            fill(dc, area.left - overflow + size.cx + p(2), mid - p(13), area.left - overflow + size.cx + p(4), mid + p(13), c_accent());
        }
        RestoreDC(dc, saved);
    }
    if v.text.is_empty() && v.caret {
        fill(dc, p(58), mid - p(13), p(58) + p(2), mid + p(13), c_accent());
    }

    if !v.text.is_empty() || v.purge.is_some() {
        fill(dc, p(PAD + 6), p(INPUT_H) - 1, w - p(PAD + 6), p(INPUT_H), c_border()); // separator
    }
    if let Some(pv) = &v.purge {
        return draw_purge(dc, w, v, pv);
    }
    if nrows == 0 {
        if !v.text.is_empty() {
            text(dc, v.fonts[F_NAME], c_dim(), RECT { left: 0, top: rows_top, right: w, bottom: rows_top + row_h as i32 }, "No results", DT_SINGLELINE | DT_VCENTER | DT_CENTER);
        }
        return;
    }

    for (i, (name, sub, icon)) in v.rows.iter().enumerate() {
        let y0 = rows_top + (i as f32 * row_h) as i32;
        let isz = p(28);
        if *icon != 0 {
            DrawIconEx(dc, p(PAD + 16), y0 + (row_h as i32 - isz) / 2, *icon as _, isz, isz, 0, null_mut(), DI_NORMAL);
        }
        let (left, right) = (p(PAD + 16 + 28 + 14), w - p(PAD + 14));
        text(dc, v.fonts[F_NAME], c_text(), RECT { left, top: y0 + p(7), right, bottom: y0 + p(29) }, name, DT_SINGLELINE | DT_END_ELLIPSIS);
        text(dc, v.fonts[F_PATH], c_dim(), RECT { left, top: y0 + p(29), right, bottom: y0 + p(47) }, sub, DT_SINGLELINE | DT_END_ELLIPSIS);
    }
    let foot_y = rows_top + (nrows as f32 * row_h) as i32;
    let more = if v.total > nrows { format!("   \u{2022}   {} results", v.total) } else { String::new() };
    text(
        dc,
        v.fonts[F_HINT],
        c_faint(),
        RECT { left: p(PAD + 16), top: foot_y + p(4), right: w - p(PAD + 14), bottom: foot_y + p(FOOT_H) },
        &if v.renaming {
            "Type the new name     \u{21B5} rename     Esc cancel".to_string()
        } else {
            format!("\u{2191}\u{2193} select     \u{21B5} open     Ctrl+\u{21B5} folder     Right-click menu     Esc close{more}")
        },
        DT_SINGLELINE | DT_VCENTER,
    );

    if let Some((l, t, hot)) = v.menu {
        let (mw, pad, ih) = (p(MENU_W), p(MENU_PAD), p(MENU_ITEM_H));
        let mh = pad * 2 + MENU.len() as i32 * ih;
        let mut g: *mut GpGraphics = null_mut();
        GdipCreateFromHDC(dc, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        round_rect(g, l as f32, t as f32, mw as f32, mh as f32, 10.0 * s, c_border());
        round_rect(g, (l + 1) as f32, (t + 1) as f32, (mw - 2) as f32, (mh - 2) as f32, 9.0 * s, c_menu_bg());
        round_rect(g, (l + pad) as f32, (t + pad + hot as i32 * ih) as f32, (mw - 2 * pad) as f32, ih as f32, 7.0 * s, c_menu_hot());
        GdipDeleteGraphics(g);
        for (i, label) in MENU.iter().enumerate() {
            let label = if i == 3 && v.menu_pinned { "Remove from Start" } else { label };
            let y0 = t + pad + i as i32 * ih;
            let c = if i == MENU.len() - 1 { DANGER } else { c_text() };
            text(dc, v.fonts[F_PATH], c, RECT { left: l + pad + p(12), top: y0, right: l + mw - pad, bottom: y0 + ih }, label, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        }
    }
}

fn main() {
    use std::os::windows::process::CommandExt;
    if std::env::args().nth(1).as_deref() == Some("--autostart") {
        let exe = std::env::current_exe().unwrap();
        let ok = std::process::Command::new("reg")
            .args(winrust_common::reg::user_args(&["add", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "rs-launcher", "/t", "REG_SZ", "/d", &format!("\"{}\"", exe.display()), "/f"]))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        std::process::exit(if ok { 0 } else { 1 });
    }
    unsafe {
        SetProcessDPIAware();
        let dc = GetDC(null_mut());
        let scale = GetDeviceCaps(dc, LOGPIXELSX as i32) as f32 / 96.0;
        ReleaseDC(null_mut(), dc);

        let (mut token, mut input): (usize, GdiplusStartupInput) = (0, zeroed());
        input.GdiplusVersion = 1;
        GdiplusStartup(&mut token, &input, null_mut());

        let hi = GetModuleHandleW(null());
        let cls = wide("rs_launcher");
        RegisterClassW(&WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hi,
            hIcon: null_mut(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: cls.as_ptr(),
        });
        let main = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, cls.as_ptr(), wide("rs launcher").as_ptr(), WS_POPUP, 0, 0, 10, 10, null_mut(), null_mut(), hi, null());
        if main.is_null() {
            let msg = wide(&format!("cannot create window (error {})", GetLastError()));
            MessageBoxW(null_mut(), msg.as_ptr(), wide("rs launcher").as_ptr(), MB_ICONERROR);
            return;
        }

        st(|s| s.scale = scale);
        apply_style(main);

        if RegisterHotKey(main, 1, MOD_ALT | MOD_NOREPEAT, VK_SPACE as u32) == 0 {
            MessageBoxW(null_mut(), wide("Alt+Space is taken (is the launcher already running?)").as_ptr(), wide("rs launcher").as_ptr(), MB_ICONERROR);
            return;
        }
        // SearchHost is started by svchost as a packaged app, which ignores the IFEO block the installer sets: end it whenever it appears.
        // Only when the installer's block is present (an uninstalled / unblocked setup must get its search back).
        let blocked = std::process::Command::new("reg").args(["query", r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\SearchHost.exe", "/v", "Debugger"]).creation_flags(0x0800_0000).output().is_ok_and(|o| o.status.success());
        if blocked {
            std::thread::spawn(|| {
                let Ok(mut sys) = sysinfo::System::new() else { return };
                loop {
                    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                    sys.processes_by_exact_name("SearchHost.exe".as_ref()).for_each(|p| {
                        p.kill();
                    });
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            });
        }
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;

    #[test]
    fn wipe_unlocks_read_only_and_never_follows_junctions() {
        let base = std::env::temp_dir().join("rs-wipe-test-7c1d");
        let _ = fs::remove_dir_all(&base);
        let (doomed, outside) = (base.join("doomed"), base.join("outside"));
        fs::create_dir_all(doomed.join("sub")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let ro = doomed.join("sub").join("ro.txt");
        fs::write(&ro, "x").unwrap();
        let mut perm = fs::metadata(&ro).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&ro, perm).unwrap();
        fs::write(outside.join("keep.txt"), "keep").unwrap();
        let link = doomed.join("link");
        assert!(std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&link).arg(&outside).creation_flags(0x0800_0000).output().unwrap().status.success());

        assert_eq!(measure(&doomed), (2, 1), "ro.txt + the junction itself, target not counted");
        let mut failed = Vec::new();
        wipe(&doomed, &mut failed);
        assert!(failed.is_empty(), "{failed:?}");
        assert!(!doomed.exists());
        assert!(outside.join("keep.txt").exists(), "junction target must survive");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn sizes() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(5 * 1_048_576), "5.0 MB");
    }
}
