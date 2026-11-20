//! The look of the file manager window, in the same palette as topbar: the top part (tabs, back / forward / up / reload,
//! address pill) and the status line are painted by hand (double buffered, eased hover, click flash); the file list is the
//! native list themed dark with taller rows. Everything is scaled by the system DPI.
#![allow(unsafe_op_in_unsafe_fn)]
use super::*;
use std::{cell::Cell, collections::HashMap, path::Path};
use windows_sys::Win32::{Graphics::{Dwm::DwmSetWindowAttribute, Gdi::*}, System::{LibraryLoader::{GetProcAddress, LoadLibraryW}, SystemInformation::GetTickCount64}, UI::Input::KeyboardAndMouse::{GetFocus, TRACKMOUSEEVENT, TME_LEAVE, TrackMouseEvent}};

// The colours come from the WinRust style the topbar gear picks (normal, terminal, dock, tiling): the same palette as the bar.
pub use winrust_common::style::{pal, ui_face};
// sunk surfaces: darker than the window on the dark styles, white on the light one
pub fn listbg() -> u32 { if winrust_common::style::style().light() { winrust_common::style::card() } else { mix(pal().bg, 0, 0.22) } }
pub fn pill() -> u32 { if winrust_common::style::style().light() { mix(pal().bg, pal().text, 0.06) } else { mix(pal().bg, 0, 0.35) } }
pub fn off() -> u32 { mix(pal().dim, pal().bg, 0.45) }

pub const CHROME_H: i32 = 88;
pub const STATUS_H: i32 = 26;
pub const WM_UI: u32 = WM_APP + 4; // chrome -> window: wp = action (see Hit), lp = tab index
const WM_MOUSELEAVE: u32 = 0x02A3; // not exported under the enabled features

// what the pointer is over / what a click does
const NONE: i32 = -1;
const BACK: i32 = 1;
const FWD: i32 = 2;
const UP: i32 = 3;
const RELOAD: i32 = 4;
const SEARCH: i32 = 5;
const VIEW: i32 = 6;
const PLUS: i32 = 7;
const TAB: i32 = 100; // + index
const CLOSE: i32 = 200; // + index
const CRUMB: i32 = 400; // + index: a part of the path in the address pill

thread_local! {
    static CRUMBS: RefCell<Vec<(RECT, PathBuf)>> = const { RefCell::new(Vec::new()) }; // where each path part was painted
    static SEARCHING: RefCell<Option<String>> = const { RefCell::new(None) }; // the list shows hits for this query
}

/// The list shows search hits for `q` (None: a folder again); the pill says so instead of showing the path.
pub fn set_search(q: Option<String>) { SEARCHING.set(q); }

pub fn searching() -> bool { SEARCHING.with_borrow(|s| s.is_some()) }

/// Folder of the path part `i` of the address pill.
pub fn crumb_path(i: usize) -> Option<PathBuf> { CRUMBS.with_borrow(|c| c.get(i).map(|x| x.1.clone())) }

fn crumb_at(x: i32, y: i32) -> Option<usize> { CRUMBS.with_borrow(|c| c.iter().position(|(r, _)| inside(r, x, y))) }

/// Path parts with the folder each one opens: C:\Users\me -> [("C:", C:\), ("Users", C:\Users), ("me", C:\Users\me)].
fn crumbs_of(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut acc = PathBuf::new();
    dir.components().filter_map(|c| {
        acc.push(c.as_os_str());
        match c {
            std::path::Component::RootDir => None, // folded into the drive part
            std::path::Component::Prefix(p) => { acc.push("\\"); Some((p.as_os_str().to_string_lossy().into_owned(), acc.clone())) }
            _ => Some((c.as_os_str().to_string_lossy().into_owned(), acc.clone())),
        }
    }).collect()
}

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    static HV: RefCell<HashMap<i32, (f32, u64)>> = RefCell::new(HashMap::new()); // eased hover per element: (0..1, last update)
    static BUSY: Cell<bool> = const { Cell::new(false) }; // something is still easing: paint another frame
    static HOT: Cell<i32> = const { Cell::new(NONE) };
    static PRESS: Cell<(i32, u64)> = const { Cell::new((i32::MIN, 0)) };
    static BRUSH: Cell<isize> = const { Cell::new(0) }; // address box background
}

pub fn init_scale() {
    unsafe {
        let dc = GetDC(ptr::null_mut());
        SCALE.set(GetDeviceCaps(dc, LOGPIXELSX as i32) as f32 / 96.0);
        ReleaseDC(ptr::null_mut(), dc);
    }
}

pub fn sc(v: i32) -> i32 { (v as f32 * SCALE.get()).round() as i32 }

fn now() -> u64 { unsafe { GetTickCount64() } }

fn ease(t: f32) -> f32 { t * t * (3.0 - 2.0 * t) }

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 255) as f32, ((b >> s) & 255) as f32);
        (x + (y - x) * t) as u32
    };
    ch(0) | ch(8) << 8 | ch(16) << 16
}

/// Eased 0..1 hover value of one element; keeps the painter running while it moves.
fn hv(key: i32, on: bool) -> f32 {
    let t = now();
    let target = if on { 1.0 } else { 0.0 };
    let v = HV.with_borrow_mut(|m| {
        let e = m.entry(key).or_insert((0.0, t));
        let dt = t.saturating_sub(e.1).min(50) as f32;
        e.1 = t;
        e.0 = if e.0 < target { (e.0 + dt / 120.0).min(target) } else { (e.0 - dt / 200.0).max(target) };
        e.0
    });
    if v != target { BUSY.set(true); }
    ease(v)
}

/// 1.0 right after a click, easing to 0 over 320 ms.
fn press_t(key: i32) -> f32 {
    let (k, t0) = PRESS.get();
    let t = now().saturating_sub(t0) as f32 / 320.0;
    if k != key || t >= 1.0 { return 0.0; }
    BUSY.set(true);
    (1.0 - t) * (1.0 - t)
}

pub fn font(px: i32, weight: i32, face: &str) -> isize {
    unsafe { CreateFontW(-sc(px), 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, w(face).as_ptr()) as isize }
}

// ---------- window frame ----------

/// Dark title bar in the bar's colours, rounded corners.
pub fn dark_title(h: HWND) {
    unsafe {
        let set = |attr: u32, v: i32| { DwmSetWindowAttribute(h, attr, &v as *const _ as _, 4); };
        set(20, !winrust_common::style::style().light() as i32); // immersive dark mode, except on the light style
        set(33, if winrust_common::style::style().square() { 1 } else { 2 }); // round corners, square in the terminal and tiling styles
        set(35, pal().bg as i32); // caption
        set(36, pal().text as i32); // caption text
        set(34, pal().line as i32); // border
    }
}

/// Dark context menus / scrollbars for the whole process (uxtheme's unnamed exports 135 and 136). Missing = stays light.
pub fn dark_mode() {
    unsafe {
        let m = LoadLibraryW(w("uxtheme.dll").as_ptr());
        if m.is_null() { return; }
        if let Some(f) = GetProcAddress(m, 135usize as *const u8) { std::mem::transmute::<_, extern "system" fn(i32) -> i32>(f)(2); } // SetPreferredAppMode(ForceDark)
        if let Some(f) = GetProcAddress(m, 136usize as *const u8) { std::mem::transmute::<_, extern "system" fn()>(f)(); } // FlushMenuThemes
    }
}

/// The WinRust style changed (the topbar's broadcast): the lists take the new colours and font, everything repaints.
pub unsafe fn restyle(h: HWND) {
    unsafe extern "system" fn each(c: HWND, _: LPARAM) -> i32 {
        let mut cls = [0u16; 32];
        let n = GetClassNameW(c, cls.as_mut_ptr(), 32).max(0) as usize;
        if String::from_utf16_lossy(&cls[..n]) == "SysListView32" {
            style_list(c);
        }
        1
    }
    EnumChildWindows(h, Some(each), 0);
    // the list rows' cached fonts and the title bar follow too
    let (a, b) = FONTS.replace((0, 0));
    for f in [a, b].into_iter().filter(|&f| f != 0) {
        DeleteObject(f as HGDIOBJ);
    }
    dark_title(h);
    // the window class was registered with the old background brush (shows while a pane resizes)
    let old = SetClassLongPtrW(h, GCLP_HBRBACKGROUND, CreateSolidBrush(pal().bg) as isize);
    DeleteObject(old as HGDIOBJ);
    RedrawWindow(h, ptr::null(), ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME);
}

/// Dark list: themed scrollbars and header, own colours and font, taller rows.
pub fn style_list(list: HWND) {
    unsafe {
        SetWindowTheme(list, w("DarkMode_Explorer").as_ptr(), ptr::null());
        let hdr = SendMessageW(list, LVM_GETHEADER, 0, 0) as HWND;
        SetWindowTheme(hdr, w("DarkMode_ItemsView").as_ptr(), ptr::null());
        for (m, c) in [(LVM_SETBKCOLOR, listbg()), (LVM_SETTEXTBKCOLOR, listbg()), (LVM_SETTEXTCOLOR, pal().text)] { SendMessageW(list, m, 0, c as isize); }
        SendMessageW(list, WM_SETFONT, font(14, 400, ui_face()) as usize, 1);
        row_height(list, true);
        SetWindowSubclass(list, Some(list_sub), 1, 0); // paints the column header (the theme alone leaves it dim)
    }
}

pub const ROW_H: i32 = 36;

/// Details rows get ROW_H tall through an empty 1 px wide state image list (a row is as tall as its tallest image).
/// Off for the thumbnail view, where a state image would take room next to every thumbnail.
pub fn row_height(list: HWND, on: bool) {
    unsafe {
        let il = if on { ImageList_Create(1, sc(ROW_H), ILC_COLOR32, 1, 1) as isize } else { 0 };
        let old = SendMessageW(list, LVM_SETIMAGELIST, LVSIL_STATE as usize, il);
        if old != 0 { ImageList_Destroy(old as _); }
    }
}

thread_local! {
    static ROWHOT: Cell<isize> = const { Cell::new(-1) }; // list row under the pointer
    static DROPHOT: Cell<isize> = const { Cell::new(-1) }; // folder row under a dragged file
    static FONTS: Cell<(isize, isize)> = const { Cell::new((0, 0)) }; // (name, details) fonts of the list rows
}

/// "Today, 09:48" / "Yesterday, 21:50" for recent files, the plain date otherwise.
fn friendly_date(ft: u64) -> String {
    let s = date_text(ft);
    let mut now: windows_sys::Win32::Foundation::FILETIME = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut now) };
    let now = (now.dwHighDateTime as u64) << 32 | now.dwLowDateTime as u64;
    let (today, yest) = (date_text(now), date_text(now.saturating_sub(864_000_000_000))); // 24 h in 100 ns ticks
    match s.get(..10) {
        Some(d) if s.len() >= 16 && today.get(..10) == Some(d) => format!("Today, {}", &s[11..]),
        Some(d) if s.len() >= 16 && yest.get(..10) == Some(d) => format!("Yesterday, {}", &s[11..]),
        _ => s,
    }
}

/// The details rows, painted by hand (NM_CUSTOMDRAW of the list): a rounded accent pill with a marker for the selection,
/// a soft one under the pointer, the name bright and the rest dimmed. The thumbnail view keeps the list's own drawing.
pub unsafe fn list_draw(lp: LPARAM) -> LRESULT {
    let cd = &*(lp as *const NMLVCUSTOMDRAW);
    let list = cd.nmcd.hdr.hwndFrom;
    if ST.with(|c| c.try_borrow().map_or(true, |s| s.thumbs)) { return CDRF_DODEFAULT as isize; }
    let dc = cd.nmcd.hdc;
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(list, &mut cr);
    match cd.nmcd.dwDrawStage {
        CDDS_PREPAINT => (CDRF_NOTIFYITEMDRAW | CDRF_NOTIFYPOSTPAINT) as isize,
        CDDS_POSTPAINT => {
            if SendMessageW(list, LVM_GETITEMCOUNT, 0, 0) == 0 {
                // an empty folder says so instead of showing a blank list
                let (f_big, f_txt) = (font(40, 400, winrust_common::style::icon_face()) as HFONT, font(14, 400, ui_face()) as HFONT);
                SetBkMode(dc, TRANSPARENT as i32);
                let mid = cr.bottom / 3;
                let of = SelectObject(dc, f_big);
                text(dc, "\u{E8B7}", rc(0, mid - sc(40), cr.right, mid + sc(10)), mix(listbg(), pal().dim, 0.5), DT_CENTER);
                SelectObject(dc, f_txt);
                let msg = if SEARCHING.with_borrow(|s| s.is_some()) { "Nothing found" } else { "This folder is empty" };
                text(dc, msg, rc(0, mid + sc(14), cr.right, mid + sc(40)), pal().dim, DT_CENTER);
                SelectObject(dc, of);
                DeleteObject(f_big);
                DeleteObject(f_txt);
            }
            CDRF_DODEFAULT as isize
        }
        CDDS_ITEMPREPAINT => {
            let i = cd.nmcd.dwItemSpec;
            let row = ST.with(|c| {
                let mut st = c.try_borrow_mut().ok()?;
                let State { dir, rows, .. } = &mut *st;
                let r = rows.get_mut(i)?;
                if r.icon < 0 { r.icon = icon_of(&row_path(dir, &r.name), r.dir); }
                Some((r.title.clone(), r.dir, r.time, r.size, r.icon))
            });
            let Some((name, is_dir, time, size, icon)) = row else { return CDRF_DODEFAULT as isize };
            let mut r = RECT { left: LVIR_BOUNDS as i32, ..std::mem::zeroed() };
            SendMessageW(list, LVM_GETITEMRECT, i, &mut r as *mut _ as isize);
            let row_r = rc(0, r.top, cr.right, r.bottom);
            let b = CreateSolidBrush(listbg());
            FillRect(dc, &row_r, b);
            DeleteObject(b);
            let sel = SendMessageW(list, LVM_GETITEMSTATE, i, LVIS_SELECTED as isize) as u32 & LVIS_SELECTED != 0;
            let hot = ROWHOT.get() == i as isize || DROPHOT.get() == i as isize;
            let focus = GetFocus() == list;
            let pill = rc(sc(6), r.top + sc(2), cr.right - sc(6), r.bottom - sc(2));
            let bg = if DROPHOT.get() == i as isize {
                mix(listbg(), pal().accent, 0.32)
            } else {
                match (sel, focus) {
                    (true, true) => mix(listbg(), pal().accent, if hot { 0.26 } else { 0.20 }),
                    (true, false) => mix(listbg(), pal().text, 0.10),
                    _ if hot => mix(listbg(), pal().text, 0.055),
                    _ => listbg(),
                }
            };
            if bg != listbg() { fill(dc, pill, bg, sc(10)); }
            if sel { fill(dc, rc(pill.left, pill.top + sc(8), pill.left + sc(3), pill.bottom - sc(8)), if focus { pal().accent } else { pal().dim }, sc(3)); }
            // columns from the header widths (left of the row = minus the horizontal scroll)
            let cw: Vec<i32> = (0..4).map(|k| SendMessageW(list, LVM_GETCOLUMNWIDTH, k, 0) as i32).collect();
            let x: Vec<i32> = (0..4).map(|k| r.left + cw[..k].iter().sum::<i32>()).collect();
            let himl = SendMessageW(list, LVM_GETIMAGELIST, LVSIL_SMALL as usize, 0);
            let (mut iw, mut ih) = (0, 0);
            ImageList_GetIconSize(himl as _, &mut iw, &mut ih);
            if icon >= 0 { ImageList_Draw(himl as _, icon, dc, x[0] + sc(16), (r.top + r.bottom - ih) / 2, ILD_TRANSPARENT); }
            if FONTS.get().0 == 0 { FONTS.set((font(14, 400, ui_face()), font(13, 400, ui_face()))); }
            let (f_name, f_det) = FONTS.get();
            SetBkMode(dc, TRANSPARENT as i32);
            let of = SelectObject(dc, f_name as HFONT);
            let p = Path::new(&name);
            let hit_of_search = p.is_absolute(); // search hits are absolute paths: show the name, and where it lies in the type column
            let shown = if hit_of_search { p.file_name().map_or(name.clone(), |n| n.to_string_lossy().into_owned()) } else { name.clone() };
            text(dc, &shown, rc(x[0] + sc(16) + iw + sc(12), r.top, x[1] - sc(8), r.bottom), pal().text, DT_LEFT);
            SelectObject(dc, f_det as HFONT);
            let dim = if sel { mix(pal().dim, pal().text, 0.45) } else { pal().dim };
            text(dc, &friendly_date(time), rc(x[1] + sc(10), r.top, x[2] - sc(8), r.bottom), dim, DT_LEFT);
            let kind = if hit_of_search { p.parent().map_or(String::new(), |d| d.to_string_lossy().into_owned()) } else { type_text(&name, is_dir) };
            text(dc, &kind, rc(x[2] + sc(10), r.top, x[3] - sc(8), r.bottom), dim, DT_LEFT | DT_PATH_ELLIPSIS);
            if !is_dir { text(dc, &size_text(size), rc(x[3], r.top, x[3] + cw[3] - sc(14), r.bottom), dim, DT_RIGHT); }
            SelectObject(dc, of);
            CDRF_SKIPDEFAULT as isize
        }
        _ => CDRF_DODEFAULT as isize,
    }
}

/// Name column takes what the other three leave, so the list never needs a horizontal scrollbar.
pub unsafe fn fit_columns(list: HWND) {
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(list, &mut cr);
    let rest: i32 = (1..4).map(|k| SendMessageW(list, LVM_GETCOLUMNWIDTH, k, 0) as i32).sum();
    SendMessageW(list, LVM_SETCOLUMNWIDTH, 0, (cr.right - rest).max(sc(200)) as isize);
}

/// Repaints one row (hover moved on or off it).
unsafe fn redraw_row(list: HWND, i: isize) {
    if i < 0 { return; }
    let mut r = RECT { left: LVIR_BOUNDS as i32, ..std::mem::zeroed() };
    if SendMessageW(list, LVM_GETITEMRECT, i as usize, &mut r as *mut _ as isize) != 0 {
        let mut cr: RECT = std::mem::zeroed();
        GetClientRect(list, &mut cr);
        InvalidateRect(list, &rc(0, r.top, cr.right, r.bottom), 0);
    }
}

pub unsafe fn drop_row(i: Option<isize>) {
    let list = ST.with_borrow(|s| s.list);
    let next = i.unwrap_or(-1);
    let old = DROPHOT.replace(next);
    if old != next {
        redraw_row(list, old);
        redraw_row(list, next);
    }
}

/// Column header, drawn by hand: bar colour, white / dim title, a sort arrow on the sorted column. The header reports its
/// custom draw to its parent, the list, so the hook sits on the list.
unsafe extern "system" fn list_sub(h: HWND, m: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if m == WM_MOUSEMOVE {
        // hover row for list_draw
        let mut ht: LVHITTESTINFO = std::mem::zeroed();
        ht.pt = POINT { x: (lp & 0xFFFF) as i16 as i32, y: ((lp >> 16) & 0xFFFF) as i16 as i32 };
        let i = SendMessageW(h, LVM_HITTEST, 0, &mut ht as *mut _ as isize);
        let old = ROWHOT.replace(i);
        if old != i {
            redraw_row(h, old);
            redraw_row(h, i);
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
            TrackMouseEvent(&mut tme);
        }
    } else if m == WM_MOUSELEAVE {
        redraw_row(h, ROWHOT.replace(-1));
    } else if m == WM_SETFOCUS || m == WM_KILLFOCUS {
        InvalidateRect(h, ptr::null(), 0); // the selection is accent-coloured only while the list has the focus
    }
    if m == WM_NOTIFY {
        let nm = &*(lp as *const NMCUSTOMDRAW);
        let hdr = SendMessageW(h, LVM_GETHEADER, 0, 0) as HWND;
        if nm.hdr.hwndFrom == hdr && nm.hdr.code == NM_CUSTOMDRAW {
            return match nm.dwDrawStage {
                CDDS_PREPAINT => {
                    let b = CreateSolidBrush(pal().bg); // the bar behind the columns too
                    FillRect(nm.hdc, &nm.rc, b);
                    DeleteObject(b);
                    CDRF_NOTIFYITEMDRAW as isize
                }
                CDDS_ITEMPREPAINT => {
                    let (dc, r, i) = (nm.hdc, nm.rc, nm.dwItemSpec);
                    let b = CreateSolidBrush(pal().bg);
                    FillRect(dc, &r, b);
                    DeleteObject(b);
                    fill(dc, rc(r.right - 1, r.top + sc(8), r.right, r.bottom - sc(8)), pal().line, 0);
                    let mut buf = [0u16; 128];
                    let mut it: HDITEMW = std::mem::zeroed();
                    it.mask = HDI_TEXT;
                    it.pszText = buf.as_mut_ptr();
                    it.cchTextMax = 128;
                    SendMessageW(hdr, HDM_GETITEMW, i, &mut it as *mut _ as isize);
                    let sorted = SORT_COL.load(Relaxed) == i;
                    let arrow = if !sorted { "" } else if SORT_ASC.load(Relaxed) { "  \u{2191}" } else { "  \u{2193}" };
                    let f = font(13, if sorted { 600 } else { 400 }, ui_face()) as HFONT;
                    let of = SelectObject(dc, f);
                    SetBkMode(dc, TRANSPARENT as i32);
                    let align = if i == 3 { DT_RIGHT } else { DT_LEFT }; // the size column is right-aligned like its numbers
                    text(dc, &format!("{}{arrow}", wstr(buf.as_ptr())), rc(r.left + sc(10), r.top, r.right - sc(12), r.bottom), if sorted { pal().text } else { pal().dim }, align);
                    SelectObject(dc, of);
                    DeleteObject(f);
                    CDRF_SKIPDEFAULT as isize
                }
                _ => 0,
            };
        }
    }
    DefSubclassProc(h, m, wp, lp)
}

// ---------- layout and hit testing ----------

struct Layout { tabs: Vec<RECT>, close: Vec<RECT>, plus: RECT, back: RECT, fwd: RECT, up: RECT, reload: RECT, pill: RECT, search: RECT, view: RECT }

fn rc(l: i32, t: i32, r: i32, b: i32) -> RECT { RECT { left: l, top: t, right: r, bottom: b } }

fn layout(cw: i32, n: usize) -> Layout {
    let (ty0, ty1) = (sc(8), sc(36));
    let tw = ((cw - sc(12) * 2 - sc(36)) / n.max(1) as i32 - sc(4)).clamp(sc(90), sc(220));
    let tabs: Vec<RECT> = (0..n as i32).map(|i| { let l = sc(12) + i * (tw + sc(4)); rc(l, ty0, l + tw, ty1) }).collect();
    let close = tabs.iter().map(|t| rc(t.right - sc(26), (t.top + t.bottom) / 2 - sc(10), t.right - sc(6), (t.top + t.bottom) / 2 + sc(10))).collect();
    let px = tabs.last().map_or(sc(12), |t| t.right + sc(4));
    let plus = rc(px, ty0 + sc(2), px + sc(26), ty1 - sc(2));
    let (by0, by1, bs) = (sc(44), sc(80), sc(36));
    let btn = |i: i32| rc(sc(12) + i * (bs + sc(4)), by0, sc(12) + i * (bs + sc(4)) + bs, by1);
    let view = rc(cw - sc(12) - bs, by0, cw - sc(12), by1);
    let search = rc(view.left - sc(4) - bs, by0, view.left - sc(4), by1);
    let reload = btn(3);
    let pill = rc(reload.right + sc(8), by0, search.left - sc(8), by1);
    Layout { tabs, close, plus, back: btn(0), fwd: btn(1), up: btn(2), reload, pill, search, view }
}

fn inside(r: &RECT, x: i32, y: i32) -> bool { x >= r.left && x < r.right && y >= r.top && y < r.bottom }

fn hit(l: &Layout, x: i32, y: i32) -> i32 {
    for (i, c) in l.close.iter().enumerate() { if inside(c, x, y) && l.tabs.len() > 1 { return CLOSE + i as i32; } }
    for (i, t) in l.tabs.iter().enumerate() { if inside(t, x, y) { return TAB + i as i32; } }
    for (k, r) in [(BACK, &l.back), (FWD, &l.fwd), (UP, &l.up), (RELOAD, &l.reload), (SEARCH, &l.search), (VIEW, &l.view), (PLUS, &l.plus)] {
        if inside(r, x, y) { return k; }
    }
    NONE
}

/// Puts the address box inside the pill.
pub fn place_edit(chrome: HWND) {
    unsafe {
        let edit = ST.with(|c| c.try_borrow().map_or(ptr::null_mut(), |s| s.edit));
        if edit.is_null() { return; }
        let mut r: RECT = std::mem::zeroed();
        GetClientRect(chrome, &mut r);
        let p = layout(r.right, 1).pill;
        let eh = sc(20);
        MoveWindow(edit, p.left + sc(40), (p.top + p.bottom - eh) / 2, (p.right - p.left - sc(40) - sc(16)).max(0), eh, 1);
        if GetFocus() != edit { ShowWindow(edit, SW_HIDE); } // the path parts are painted instead until the pill is clicked
    }
}

// ---------- painting ----------

/// GDI+ for anti-aliased rounded shapes (started once in main); fill() falls back to plain GDI without it.
pub fn gdiplus_start() {
    use windows_sys::Win32::Graphics::GdiPlus::{GdiplusStartup, GdiplusStartupInput};
    unsafe {
        let (mut tok, mut inp): (usize, GdiplusStartupInput) = (0, std::mem::zeroed());
        inp.GdiplusVersion = 1;
        GdiplusStartup(&mut tok, &inp, ptr::null_mut());
    }
}

/// Fills `r` (right / bottom exclusive) in `c`; `rad` > 0 rounds the corners (rad = corner diameter), anti-aliased.
unsafe fn fill(dc: HDC, r: RECT, c: u32, rad: i32) {
    let rad = if winrust_common::style::style().square() { 0 } else { rad };
    use windows_sys::Win32::Graphics::GdiPlus::*;
    let (wd, ht) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
    if wd <= 0.0 || ht <= 0.0 { return; }
    let mut g: *mut GpGraphics = ptr::null_mut();
    if rad <= 0 || GdipCreateFromHDC(dc, &mut g) != 0 || g.is_null() {
        let b = CreateSolidBrush(c);
        if rad <= 0 {
            FillRect(dc, &r, b);
        } else {
            let (ob, op) = (SelectObject(dc, b), SelectObject(dc, GetStockObject(NULL_PEN)));
            RoundRect(dc, r.left, r.top, r.right + 1, r.bottom + 1, rad, rad);
            SelectObject(dc, ob);
            SelectObject(dc, op);
        }
        DeleteObject(b);
        return;
    }
    GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
    GdipSetPixelOffsetMode(g, PixelOffsetModeHalf); // pixel i spans [i, i+1): straight edges stay crisp
    let (x, y, d) = (r.left as f32, r.top as f32, (rad as f32).min(wd).min(ht));
    let mut path: *mut GpPath = ptr::null_mut();
    GdipCreatePath(FillModeAlternate, &mut path);
    GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
    GdipAddPathArc(path, x + wd - d, y, d, d, 270.0, 90.0);
    GdipAddPathArc(path, x + wd - d, y + ht - d, d, d, 0.0, 90.0);
    GdipAddPathArc(path, x, y + ht - d, d, d, 90.0, 90.0);
    GdipClosePathFigure(path);
    let mut br: *mut GpSolidFill = ptr::null_mut();
    GdipCreateSolidFill(0xFF00_0000 | (c & 255) << 16 | (c & 0xFF00) | (c >> 16 & 255), &mut br); // COLORREF -> ARGB
    GdipFillPath(g, br as *mut GpBrush, path);
    GdipDeleteBrush(br as *mut GpBrush);
    GdipDeletePath(path);
    GdipDeleteGraphics(g);
}

unsafe fn text(dc: HDC, s: &str, mut r: RECT, c: u32, flags: u32) {
    SetTextColor(dc, c);
    DrawTextW(dc, w(s).as_ptr(), -1, &mut r, flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS);
}

/// A rounded highlight that eases in on hover and flashes in the accent colour on click; `base` is what it sits on.
unsafe fn glow(dc: HDC, key: i32, r: RECT, base: u32, to: u32, rad: i32, on: bool) {
    let (t, p) = (hv(key, on), press_t(key));
    if t > 0.01 || p > 0.0 { fill(dc, r, mix(mix(base, to, t), pal().accent, 0.45 * p), rad); }
}

unsafe fn paint_chrome(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let wdc = BeginPaint(hwnd, &mut ps);
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut cr);
    // everything painting needs from the window state, copied out so nothing is borrowed while drawing
    let snap = ST.with(|c| c.try_borrow().ok().map(|s| (s.tabs.iter().map(label).collect::<Vec<_>>(), s.active, s.hist.pos > 0, s.hist.pos + 1 < s.hist.v.len(), s.thumbs, s.edit, s.dir.clone())));
    let Some((labels, active, can_back, can_fwd, thumbs, edit, dir)) = snap else { EndPaint(hwnd, &ps); return };
    BUSY.set(false);
    let (dc, bmp) = (CreateCompatibleDC(wdc), CreateCompatibleBitmap(wdc, cr.right, cr.bottom));
    let old = SelectObject(dc, bmp);
    let bg = CreateSolidBrush(pal().bg);
    FillRect(dc, &cr, bg);
    DeleteObject(bg);
    SetBkMode(dc, TRANSPARENT as i32);
    let l = layout(cr.right, labels.len());
    let hot = HOT.get();
    let (f_txt, f_glyph, f_small) = (font(13, 400, ui_face()) as HFONT, font(16, 400, winrust_common::style::icon_face()) as HFONT, font(12, 400, winrust_common::style::icon_face()) as HFONT);
    let of = SelectObject(dc, f_txt);

    // tabs
    for (i, t) in l.tabs.iter().enumerate() {
        let on = i == active;
        let (th, ta) = (hv(TAB + i as i32, hot == TAB + i as i32 || hot == CLOSE + i as i32), hv(1000 + i as i32, on));
        let p = press_t(TAB + i as i32);
        fill(dc, *t, mix(mix(mix(pal().bg, pal().hover, th), pal().active, ta), pal().accent, 0.35 * p), sc(8));
        if ta > 0.01 { fill(dc, rc(t.left + (t.right - t.left) / 2 - sc(12), t.bottom - sc(3), t.left + (t.right - t.left) / 2 + sc(12), t.bottom), mix(pal().active, pal().accent, ta), sc(3)); }
        SelectObject(dc, f_txt);
        text(dc, &labels[i], rc(t.left + sc(14), t.top, t.right - sc(if labels.len() > 1 { 30 } else { 10 }), t.bottom), mix(pal().dim, pal().text, ta.max(th)), DT_LEFT);
        if labels.len() > 1 && (on || th > 0.05) {
            let c = l.close[i];
            glow(dc, CLOSE + i as i32, c, mix(pal().bg, pal().active, ta), pal().hover, sc(6), hot == CLOSE + i as i32);
            SelectObject(dc, f_small);
            text(dc, "\u{E711}", c, mix(pal().dim, pal().text, if hot == CLOSE + i as i32 { 1.0 } else { 0.4 }), DT_CENTER);
        }
    }
    glow(dc, PLUS, l.plus, pal().bg, pal().hover, sc(6), hot == PLUS);
    SelectObject(dc, f_small);
    text(dc, "\u{E710}", l.plus, pal().dim, DT_CENTER);

    // toolbar buttons
    SelectObject(dc, f_glyph);
    for (k, r, g, enabled) in [(BACK, l.back, "\u{E72B}", can_back), (FWD, l.fwd, "\u{E72A}", can_fwd), (UP, l.up, "\u{E74A}", true), (RELOAD, l.reload, "\u{E72C}", true), (SEARCH, l.search, "\u{E721}", true), (VIEW, l.view, if thumbs { "\u{E8FD}" } else { "\u{E80A}" }, true)] {
        if enabled { glow(dc, k, r, pal().bg, pal().hover, sc(8), hot == k); }
        text(dc, g, r, if enabled { pal().text } else { off() }, DT_CENTER);
    }

    // address pill: accent outline while it is being edited
    let focused = !edit.is_null() && GetFocus() == edit;
    let f = hv(2000, focused);
    fill(dc, l.pill, mix(pal().line, pal().accent, f), sc(18));
    fill(dc, rc(l.pill.left + 1, l.pill.top + 1, l.pill.right - 1, l.pill.bottom - 1), pill(), sc(17));
    SelectObject(dc, f_glyph);
    let searching = SEARCHING.with_borrow(|s| s.clone());
    text(dc, if searching.is_some() { "\u{E721}" } else { "\u{E838}" }, rc(l.pill.left + sc(10), l.pill.top, l.pill.left + sc(38), l.pill.bottom), mix(pal().dim, pal().accent, f), DT_CENTER);
    // not being edited: the path as clickable parts, like Windows 11 Explorer ("C: > Users > me"); the edit box is hidden
    let mut crumbs = vec![];
    if !focused {
        let (x0, x1) = (l.pill.left + sc(40), l.pill.right - sc(12));
        let (t, b) = (l.pill.top + sc(5), l.pill.bottom - sc(5));
        if let Some(q) = &searching {
            SelectObject(dc, f_txt);
            text(dc, &format!("Results for \u{201C}{q}\u{201D}"), rc(x0 + sc(6), l.pill.top, x1, l.pill.bottom), pal().accent, DT_LEFT);
        } else {
            let parts = crumbs_of(&dir);
            let width = |s: &str| { let mut r = rc(0, 0, 0, 0); DrawTextW(dc, w(s).as_ptr(), -1, &mut r, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX); r.right + sc(16) };
            let chev = sc(18);
            SelectObject(dc, f_txt);
            let ws: Vec<i32> = parts.iter().map(|p| width(&p.0)).collect();
            // too long: drop parts from the left; a "..." part then opens the last one dropped
            let mut first = 0;
            while first + 1 < parts.len() && ws[first..].iter().sum::<i32>() + chev * (parts.len() - first) as i32 + width("\u{2026}") > x1 - x0 { first += 1; }
            let mut shown: Vec<(String, PathBuf, i32)> = vec![];
            if first > 0 { shown.push(("\u{2026}".into(), parts[first - 1].1.clone(), width("\u{2026}"))); }
            shown.extend(parts[first..].iter().zip(&ws[first..]).map(|(p, &wd)| (p.0.clone(), p.1.clone(), wd)));
            let mut x = x0;
            let last = shown.len().saturating_sub(1);
            for (i, (label, path, wd)) in shown.into_iter().enumerate() {
                let r = rc(x, t, (x + wd).min(x1), b);
                glow(dc, CRUMB + i as i32, r, pill(), pal().hover, sc(8), hot == CRUMB + i as i32);
                SelectObject(dc, f_txt);
                text(dc, &label, r, if i == last { pal().text } else { mix(pal().dim, pal().text, if hot == CRUMB + i as i32 { 1.0 } else { 0.35 }) }, DT_CENTER);
                crumbs.push((r, path));
                x = r.right;
                if i < last {
                    SelectObject(dc, f_small);
                    text(dc, "\u{E76C}", rc(x, t, x + chev, b), mix(pal().dim, pill(), 0.3), DT_CENTER);
                    x += chev;
                }
            }
        }
    }
    CRUMBS.set(crumbs);

    fill(dc, rc(0, cr.bottom - 1, cr.right, cr.bottom), pal().line, 0); // hairline under the chrome
    BitBlt(wdc, 0, 0, cr.right, cr.bottom, dc, 0, 0, SRCCOPY);
    SelectObject(dc, of);
    for f in [f_txt, f_glyph, f_small] { DeleteObject(f); }
    SelectObject(dc, old);
    DeleteObject(bmp);
    DeleteDC(dc);
    if BUSY.get() { SetTimer(hwnd, 4, 16, None); } else { KillTimer(hwnd, 4); }
    EndPaint(hwnd, &ps);
}

/// Item / selection counts and the sort order, along the bottom edge.
pub unsafe fn paint_status(h: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(h, &mut ps);
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(h, &mut cr);
    let strip = rc(0, cr.bottom - sc(STATUS_H), cr.right, cr.bottom);
    let bg = CreateSolidBrush(pal().bg);
    FillRect(dc, &strip, bg);
    DeleteObject(bg);
    fill(dc, rc(0, strip.top, cr.right, strip.top + 1), pal().line, 0); // hairline above the status line
    let snap = ST.with(|c| {
        c.try_borrow().ok().map(|s| {
            // selected count and the total size of the selected files
            let (mut n, mut bytes, mut i) = (0usize, 0u64, -1isize);
            loop {
                i = SendMessageW(s.list, LVM_GETNEXTITEM, i as usize, LVNI_SELECTED as isize);
                let Some(r) = usize::try_from(i).ok().and_then(|i| s.rows.get(i)) else { break };
                n += 1;
                if !r.dir { bytes += r.size; }
            }
            (s.rows.len(), n, bytes)
        })
    });
    if let Some((n, sel, bytes)) = snap {
        let items = |k: usize| format!("{k} item{}", if k == 1 { "" } else { "s" });
        let left = match (sel, bytes) {
            (0, _) => items(n),
            (s, 0) => format!("{}  \u{00B7}  {s} selected", items(n)),
            (s, b) => format!("{}  \u{00B7}  {s} selected  \u{00B7}  {}", items(n), size_text(b)),
        };
        let left = crate::jobs::status_text().unwrap_or(left); // a copy, packing or unpacking in progress
        let by = ["Name", "Date", "Type", "Size"][SORT_COL.load(Relaxed).min(3)];
        let right = format!("{by} {}", if SORT_ASC.load(Relaxed) { "\u{2191}" } else { "\u{2193}" });
        let f = font(12, 400, ui_face()) as HFONT;
        let of = SelectObject(dc, f);
        SetBkMode(dc, TRANSPARENT as i32);
        text(dc, &left, rc(strip.left + sc(14), strip.top, strip.right / 2, strip.bottom), pal().dim, DT_LEFT);
        text(dc, &right, rc(strip.right / 2, strip.top, strip.right - sc(14), strip.bottom), pal().dim, DT_RIGHT);
        SelectObject(dc, of);
        DeleteObject(f);
    }
    EndPaint(h, &ps);
}

pub unsafe fn status_dirty(h: HWND) {
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(h, &mut cr);
    let strip = rc(0, cr.bottom - sc(STATUS_H), cr.right, cr.bottom);
    InvalidateRect(h, &strip, 0);
}

// ---------- chrome window ----------

pub unsafe extern "system" fn proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
    let layout_now = || {
        let mut r: RECT = std::mem::zeroed();
        GetClientRect(h, &mut r);
        layout(r.right, ST.with(|c| c.try_borrow().map_or(1, |s| s.tabs.len())))
    };
    match m {
        WM_PAINT => paint_chrome(h),
        WM_ERASEBKGND => return 1,
        WM_SIZE => { place_edit(h); InvalidateRect(h, ptr::null(), 0); } // the pill and the tabs re-flow: repaint it all
        WM_TIMER if wp == 4 => { InvalidateRect(h, ptr::null(), 0); }
        WM_MOUSEMOVE => {
            let k = match hit(&layout_now(), x, y) { NONE => crumb_at(x, y).map_or(NONE, |i| CRUMB + i as i32), k => k };
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
            TrackMouseEvent(&mut tme);
            if HOT.replace(k) != k { InvalidateRect(h, ptr::null(), 0); }
        }
        WM_MOUSELEAVE => {
            HOT.set(NONE);
            InvalidateRect(h, ptr::null(), 0);
        }
        WM_LBUTTONDOWN | WM_MBUTTONDOWN => {
            let l = layout_now();
            let k = hit(&l, x, y);
            if k == NONE {
                if let Some(i) = crumb_at(x, y) {
                    PRESS.set((CRUMB + i as i32, now()));
                    PostMessageW(GetParent(h), WM_UI, 21, i as isize); // a path part: go there
                } else if inside(&l.pill, x, y) {
                    // the rest of the pill: type a path
                    let edit = ST.with(|c| c.try_borrow().map_or(ptr::null_mut(), |s| s.edit));
                    SetFocus(edit);
                    SendMessageW(edit, EM_SETSEL, 0, -1);
                }
                InvalidateRect(h, ptr::null(), 0);
                return 0;
            }
            PRESS.set((k, now()));
            InvalidateRect(h, ptr::null(), 0);
            let (act, idx) = match k {
                BACK => (4, 0), FWD => (5, 0), UP => (6, 0), RELOAD => (7, 0), SEARCH => (9, 0), VIEW => (8, 0), PLUS => (3, 0),
                k if k >= CLOSE => (2, (k - CLOSE) as isize),
                k => (if m == WM_MBUTTONDOWN { 2 } else { 1 }, (k - TAB) as isize), // middle click on a tab closes it
            };
            PostMessageW(GetParent(h), WM_UI, act, idx);
        }
        WM_COMMAND if (wp >> 16) == 0x0100 || (wp >> 16) == 0x0200 => {
            // edit got / lost focus: shown only while typing (the path parts otherwise), outline colour
            ShowWindow(lp as HWND, if (wp >> 16) == 0x0100 { SW_SHOW } else { SW_HIDE });
            InvalidateRect(h, ptr::null(), 0);
        }
        WM_CTLCOLOREDIT => {
            // the address box: white text on the pill's colour
            let dc = wp as HDC;
            SetTextColor(dc, pal().text);
            SetBkColor(dc, pill());
            if BRUSH.get() == 0 { BRUSH.set(CreateSolidBrush(pill()) as isize); }
            return BRUSH.get();
        }
        WM_RBUTTONUP => {
            // right click on a pinned folder: unpin it
            if let Some(p) = nav_hit(y).and_then(nav_path).filter(|p| pins().contains(p)) {
                let menu = CreatePopupMenu();
                AppendMenuW(menu, MF_STRING, 1, w("Unpin from Quick access").as_ptr());
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, h, ptr::null());
                DestroyMenu(menu);
                if id == 1 { toggle_pin(&p); }
            }
        }
        _ => return DefWindowProcW(h, m, wp, lp),
    }
    0
}


// ---------- navigation pane (left): quick access folders and the drives, like Explorer's ----------

pub const NAV_W: i32 = 232;
const NAV_ROW: i32 = 34;
const NAV_HEAD: i32 = 32;

struct NavItem { title: String, path: PathBuf, glyph: &'static str, cap: Option<(u64, u64)> } // empty path = section heading; cap = (free, total) bytes of a drive

const NAV_DRIVE: i32 = 52; // a drive row also shows how full it is

fn nav_h(it: &NavItem) -> i32 {
    sc(if it.path.as_os_str().is_empty() { NAV_HEAD } else if it.cap.is_some() { NAV_DRIVE } else { NAV_ROW })
}

/// Bottom of the Quick access section (the point where "This PC" starts).
fn nav_quick_bottom() -> i32 {
    let mut top = sc(8);
    NAV.with_borrow(|n| {
        for it in n {
            if it.path.as_os_str().is_empty() && it.title == "This PC" { return top; }
            top += nav_h(it);
        }
        top
    })
}

thread_local! {
    static NAV: RefCell<Vec<NavItem>> = const { RefCell::new(Vec::new()) };
    static NAVWND: Cell<isize> = const { Cell::new(0) };
    static NAVHOT: Cell<i32> = const { Cell::new(-1) };
    static NAVDROP: Cell<bool> = const { Cell::new(false) };
}

fn nav_quick_file() -> PathBuf { PathBuf::from(env::var_os("APPDATA").unwrap_or_default()).join(r"rust-explorer\quick-access.txt") }

fn nav_same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

fn nav_saved() -> Vec<PathBuf> {
    fs::read_to_string(nav_quick_file()).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect()
}

/// Folders the user pinned to Quick access: %APPDATA%\WinRust\rust-explorer-pins.txt, one path per line.
fn pins_file() -> Option<PathBuf> { env::var_os("APPDATA").map(|a| PathBuf::from(a).join("WinRust").join("rust-explorer-pins.txt")) }

pub fn pins() -> Vec<PathBuf> {
    pins_file().and_then(|f| fs::read_to_string(f).ok()).map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect()).unwrap_or_default()
}

/// Pins `p` (or unpins it when already pinned) and rebuilds the left pane.
pub fn toggle_pin(p: &Path) {
    let mut v = pins();
    if let Some(i) = v.iter().position(|x| x == p) { v.remove(i); } else { v.push(p.to_path_buf()); }
    if let Some(f) = pins_file() {
        if let Some(d) = f.parent() { let _ = fs::create_dir_all(d); }
        let _ = fs::write(f, v.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n"));
    }
    let h = NAVWND.get() as HWND;
    if !h.is_null() { nav_init(h); nav_dirty(); }
}

pub fn nav_init(h: HWND) {
    NAVWND.set(h as isize);
    let home = PathBuf::from(env::var_os("USERPROFILE").unwrap_or_default());
    let head = |t: &str| NavItem { title: t.into(), path: PathBuf::new(), glyph: "", cap: None };
    let mut v = vec![head("Quick access"), NavItem { title: "Home".into(), path: home.clone(), glyph: "\u{E80F}", cap: None }];
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Videos};
    for (t, id, g) in [("Desktop", &FOLDERID_Desktop, "\u{E7F4}"), ("Downloads", &FOLDERID_Downloads, "\u{E896}"), ("Documents", &FOLDERID_Documents, "\u{E8A5}"), ("Pictures", &FOLDERID_Pictures, "\u{E91B}"), ("Music", &FOLDERID_Music, "\u{E8D6}"), ("Videos", &FOLDERID_Videos, "\u{E714}")] {
        let Some(p) = crate::sh::known_dir(id).filter(|p| p.is_dir()) else { continue };
        v.push(NavItem { title: t.into(), path: p, glyph: g, cap: None });
    }
    for p in pins() {
        if p.is_dir() { v.push(NavItem { title: p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned()), path: p, glyph: "\u{E718}", cap: None }); }
    }
    let mut known: Vec<PathBuf> = v.iter().map(|x| x.path.clone()).collect();
    for p in nav_saved() {
        if known.iter().any(|x| nav_same_path(x, &p)) { continue; }
        let title = p.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| p.to_string_lossy().into_owned());
        known.push(p.clone());
        v.push(NavItem { title, path: p, glyph: "\u{E8B7}", cap: None });
    }
    v.push(head("This PC"));
    unsafe {
        use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
        let mask = GetLogicalDrives();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 { continue; }
            let letter = (b'A' + i as u8) as char;
            let root = format!("{letter}:\\");
            // fixed and removable disks only: asking a dead network share for its label would stall the window
            let ty = GetDriveTypeW(w(&root).as_ptr());
            if ty == 4 {
                // a mapped network drive: listed without label and size, they would stall the window while a share is down
                v.push(NavItem { title: format!("Network Drive ({letter}:)"), path: PathBuf::from(root), glyph: "\u{E8CE}", cap: None });
                continue;
            }
            if !matches!(ty, 2 | 3) { continue; }
            let mut label = [0u16; 64];
            let ok = GetVolumeInformationW(w(&root).as_ptr(), label.as_mut_ptr(), 64, ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), 0);
            let name = if ok != 0 { wstr(label.as_ptr()) } else { String::new() };
            let (mut free, mut total) = (0u64, 0u64);
            let cap = (GetDiskFreeSpaceExW(w(&root).as_ptr(), &mut free, &mut total, ptr::null_mut()) != 0 && total > 0).then_some((free, total));
            v.push(NavItem { title: format!("{} ({letter}:)", if name.is_empty() { "Local Disk" } else { &name }), path: PathBuf::from(root), glyph: "\u{EDA2}", cap });
        }
    }
    let devices = shell_devices();
    if !devices.is_empty() {
        v.push(head("Portable Devices"));
        for (title, path, _) in devices {
            remember_shell_title(&path, &title);
            v.push(NavItem { title, path, glyph: "\u{E8BD}", cap: None });
        }
    }
    NAV.with_borrow_mut(|n| *n = v);
}

pub fn nav_path(i: usize) -> Option<PathBuf> {
    NAV.with_borrow(|n| n.get(i).map(|x| x.path.clone()).filter(|p| !p.as_os_str().is_empty()))
}

pub fn nav_refresh() {
    let h = NAVWND.get() as HWND;
    if !h.is_null() {
        nav_init(h);
        nav_dirty();
    }
}

pub fn nav_dirty() {
    let h = NAVWND.get() as HWND;
    if !h.is_null() { unsafe { InvalidateRect(h, ptr::null(), 0) }; }
}

/// Pins folders to Quick access and returns how many were really added.
pub fn nav_add(paths: &[PathBuf]) -> usize {
    let mut saved = nav_saved();
    let mut known: Vec<PathBuf> = NAV.with_borrow(|n| n.iter().map(|x| x.path.clone()).collect());
    let mut added = 0;
    for p in paths.iter().filter(|p| p.is_dir()) {
        if known.iter().any(|x| nav_same_path(x, p)) { continue; }
        known.push(p.clone());
        saved.push(p.clone());
        added += 1;
    }
    if added == 0 { return 0; }
    let f = nav_quick_file();
    let _ = fs::create_dir_all(f.parent().unwrap_or(&f));
    let text: String = saved.iter().map(|p| format!("{}\n", p.to_string_lossy())).collect();
    if fs::write(f, text).is_err() { return 0; }
    let at = NAV.with_borrow(|n| n.iter().position(|x| x.path.as_os_str().is_empty() && x.title == "This PC").unwrap_or(n.len()));
    NAV.with_borrow_mut(|n| {
        for p in saved.iter().rev().take(added) {
            let title = p.file_name().map(|x| x.to_string_lossy().into_owned()).filter(|x| !x.is_empty()).unwrap_or_else(|| p.to_string_lossy().into_owned());
            n.insert(at, NavItem { title, path: p.clone(), glyph: "\u{E8B7}", cap: None });
        }
    });
    nav_dirty();
    added
}

/// True while a drop is over the Quick access section.
pub fn nav_drop_at(y: i32) -> bool { y >= sc(8) && y < nav_quick_bottom() }

pub fn nav_drop_begin() { if !NAVDROP.replace(true) { nav_dirty(); } }

pub fn nav_drop_end() { if NAVDROP.replace(false) { nav_dirty(); } }

/// Chrome on top, the pane on the left (hidden when the window is narrow), the list takes the rest.
pub unsafe fn layout_children(cw: i32, ch: i32) {
    let (chrome, list) = ST.with(|c| c.try_borrow().map_or((ptr::null_mut(), ptr::null_mut()), |s| (s.tab, s.list)));
    let nav = NAVWND.get() as HWND;
    let (top, status) = (sc(CHROME_H), sc(STATUS_H));
    let nw = if cw > sc(760) { sc(NAV_W) } else { 0 };
    let reserve = super::cover::reserve();
    let h = (ch - top - status - reserve).max(0);
    MoveWindow(chrome, 0, 0, cw, top, 1);
    MoveWindow(nav, 0, top, nw, h + reserve, 1);
    MoveWindow(list, nw, top + reserve, cw - nw, h, 1);
    fit_columns(list);
}

/// Item under y (None over headings or empty space).
fn nav_hit(y: i32) -> Option<usize> {
    let mut top = sc(8);
    NAV.with_borrow(|n| {
        for (i, it) in n.iter().enumerate() {
            let hgt = nav_h(it);
            if y >= top && y < top + hgt { return (!it.path.as_os_str().is_empty()).then_some(i); }
            top += hgt;
        }
        None
    })
}

unsafe fn paint_nav(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let wdc = BeginPaint(hwnd, &mut ps);
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut cr);
    BUSY.set(false);
    let (dc, bmp) = (CreateCompatibleDC(wdc), CreateCompatibleBitmap(wdc, cr.right.max(1), cr.bottom.max(1)));
    let old = SelectObject(dc, bmp);
    let bg = CreateSolidBrush(pal().bg);
    FillRect(dc, &cr, bg);
    DeleteObject(bg);
    if NAVDROP.get() { fill(dc, rc(sc(4), sc(8), cr.right - sc(4), nav_quick_bottom()), mix(pal().bg, pal().accent, 0.22), sc(10)); }
    SetBkMode(dc, TRANSPARENT as i32);
    let (f_txt, f_head, f_glyph, f_small) = (font(13, 400, ui_face()) as HFONT, font(12, 600, ui_face()) as HFONT, font(16, 400, winrust_common::style::icon_face()) as HFONT, font(11, 400, ui_face()) as HFONT);
    let of = SelectObject(dc, f_txt);
    // where we are = the deepest item that contains the current folder
    let cur = ST.with(|c| c.try_borrow().ok().map(|s| s.dir.to_string_lossy().to_lowercase())).unwrap_or_default();
    let hot = NAVHOT.get();
    NAV.with_borrow(|n| {
        let here = n.iter().enumerate().filter(|(_, it)| !it.path.as_os_str().is_empty() && cur.starts_with(&it.path.to_string_lossy().to_lowercase())).max_by_key(|(_, it)| it.path.as_os_str().len()).map(|(i, _)| i);
        let mut y = sc(8);
        for (i, it) in n.iter().enumerate() {
            if it.path.as_os_str().is_empty() {
                SelectObject(dc, f_head);
                let head = if NAVDROP.get() && it.title == "Quick access" { "QUICK ACCESS - DROP TO PIN".to_string() } else { it.title.to_uppercase() };
                text(dc, &head, rc(sc(18), y + sc(6), cr.right - sc(8), y + sc(NAV_HEAD)), pal().dim, DT_LEFT);
                y += sc(NAV_HEAD);
                continue;
            }
            let hgt = nav_h(it);
            let r = rc(sc(8), y, cr.right - sc(8), y + hgt);
            let (th, ta) = (hv(5000 + i as i32, hot == i as i32), hv(6000 + i as i32, here == Some(i)));
            let p = press_t(5000 + i as i32);
            if th.max(ta) > 0.01 || p > 0.0 { fill(dc, r, mix(mix(mix(pal().bg, pal().hover, th), pal().active, ta), pal().accent, 0.3 * p), sc(10)); }
            if ta > 0.01 { fill(dc, rc(r.left, y + sc(10), r.left + sc(3), y + hgt - sc(10)), mix(pal().active, pal().accent, ta), sc(3)); }
            let line = rc(r.left, y, r.right, y + sc(NAV_ROW)); // the title line (a drive has its usage bar below it)
            SelectObject(dc, f_glyph);
            text(dc, it.glyph, rc(r.left + sc(10), line.top, r.left + sc(40), line.bottom), mix(pal().dim, pal().accent, ta), DT_CENTER);
            SelectObject(dc, f_txt);
            text(dc, &it.title, rc(r.left + sc(48), line.top, r.right - sc(8), line.bottom), mix(pal().dim, pal().text, ta.max(th)), DT_LEFT);
            if let Some((free, total)) = it.cap {
                // usage bar like Windows' "This PC": accent, red once the drive is more than 90 % full
                let used = 1.0 - free as f32 / total as f32;
                let bar = rc(r.left + sc(48), y + sc(30), r.right - sc(14), y + sc(34));
                fill(dc, bar, mix(pal().bg, pal().text, 0.12), sc(4));
                let fw = ((bar.right - bar.left) as f32 * used) as i32;
                fill(dc, rc(bar.left, bar.top, bar.left + fw.max(sc(4)), bar.bottom), if used > 0.9 { winrust_common::style::BAD } else { pal().accent }, sc(4));
                SelectObject(dc, f_small);
                let gb = |b: u64| b as f64 / 1_073_741_824.0;
                text(dc, &format!("{:.0} GB free of {:.0} GB", gb(free), gb(total)), rc(bar.left, bar.bottom + sc(1), r.right - sc(8), y + hgt - sc(2)), pal().dim, DT_LEFT);
            }
            y += hgt;
        }
    });
    fill(dc, rc(cr.right - 1, 0, cr.right, cr.bottom), pal().line, 0); // hairline between the pane and the list
    BitBlt(wdc, 0, 0, cr.right, cr.bottom, dc, 0, 0, SRCCOPY);
    SelectObject(dc, of);
    for f in [f_txt, f_head, f_glyph, f_small] { DeleteObject(f); }
    SelectObject(dc, old);
    DeleteObject(bmp);
    DeleteDC(dc);
    if BUSY.get() { SetTimer(hwnd, 4, 16, None); } else { KillTimer(hwnd, 4); }
    EndPaint(hwnd, &ps);
}

pub unsafe extern "system" fn nav_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
    match m {
        WM_PAINT => paint_nav(h),
        WM_ERASEBKGND => return 1,
        WM_TIMER if wp == 4 => { InvalidateRect(h, ptr::null(), 0); }
        WM_MOUSEMOVE => {
            let k = nav_hit(y).map_or(-1, |i| i as i32);
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
            TrackMouseEvent(&mut tme);
            if NAVHOT.replace(k) != k { InvalidateRect(h, ptr::null(), 0); }
        }
        WM_MOUSELEAVE => { NAVHOT.set(-1); InvalidateRect(h, ptr::null(), 0); }
        WM_LBUTTONDOWN => {
            if let Some(i) = nav_hit(y) {
                PRESS.set((5000 + i as i32, now()));
                InvalidateRect(h, ptr::null(), 0);
                PostMessageW(GetParent(h), WM_UI, 20, i as isize);
            }
        }
        _ => return DefWindowProcW(h, m, wp, lp),
    }
    0
}
