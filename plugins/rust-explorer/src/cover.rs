//! Cover-flow полоса над обычным списком файлов: листание стрелками, клик по краям, Enter — открыть в photoview.
#![allow(unsafe_op_in_unsafe_fn)]
use super::*;
use std::{cell::Cell, collections::{HashMap, HashSet}, path::PathBuf, sync::{atomic::{AtomicU32, Ordering::Relaxed}, mpsc::Sender, OnceLock}};
use windows_sys::Win32::Graphics::Gdi::*;

pub const CLASS: &str = "RxCover";
const H: i32 = 210;

thread_local! {
    static WND: Cell<isize> = const { Cell::new(0) };
    static OWNER: Cell<isize> = const { Cell::new(0) };
    static OPEN: Cell<bool> = const { Cell::new(false) };
    static PATHS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    static IDX: Cell<usize> = const { Cell::new(0) };
    static CACHE: RefCell<HashMap<usize, isize>> = RefCell::new(HashMap::new());
    static PENDING: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

static TX: OnceLock<Sender<(u32, usize, PathBuf)>> = OnceLock::new();
static GEN: AtomicU32 = AtomicU32::new(0);

pub fn generation() -> u32 { GEN.load(Relaxed) }
pub fn is_open() -> bool { OPEN.get() }
pub fn reserve() -> i32 { if OPEN.get() { ui::sc(H) } else { 0 } }

pub unsafe fn create(parent: HWND, inst: HINSTANCE) -> HWND {
    let h = CreateWindowExW(0, w(CLASS).as_ptr(), ptr::null(), WS_CHILD | WS_CLIPSIBLINGS, 0, 0, 0, 0, parent, 0 as _, inst, ptr::null());
    WND.set(h as isize);
    OWNER.set(parent as isize);
    let _ = TX.set(super::sh::start_cover_thumbs(parent as isize));
    h
}

fn clear_cache() {
    let old: Vec<isize> = CACHE.with_borrow_mut(|c| std::mem::take(c).into_values().collect());
    for b in old { if b != 0 { unsafe { DeleteObject(b as _) }; } }
    PENDING.with_borrow_mut(|p| p.clear());
}

fn client(parent: HWND) -> (i32, i32) {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(parent, &mut r) };
    (r.right, r.bottom)
}

fn relayout() {
    let parent = OWNER.get() as HWND;
    if parent.is_null() { return; }
    let (cw, ch) = client(parent);
    let lp = (((ch as isize) & 0xFFFF) << 16) | ((cw as isize) & 0xFFFF);
    unsafe { PostMessageW(parent, WM_SIZE, 0, lp) };
}

pub unsafe fn layout(cw: i32, ch: i32) {
    let h = WND.get() as HWND;
    if h.is_null() { return; }
    let nw = if cw > ui::sc(760) { ui::sc(ui::NAV_W) } else { 0 };
    let top = ui::sc(ui::CHROME_H);
    let hh = reserve();
    MoveWindow(h, nw, top, (cw - nw).max(0), hh, 1);
    ShowWindow(h, if OPEN.get() { SW_SHOW } else { SW_HIDE });
    let _ = ch;
}

fn request(i: usize) {
    let n = PATHS.with_borrow(|p| p.len());
    if i >= n || CACHE.with_borrow(|c| c.contains_key(&i)) || PENDING.with_borrow(|p| p.contains(&i)) { return; }
    let path = PATHS.with_borrow(|p| p[i].clone());
    PENDING.with_borrow_mut(|p| { p.insert(i); });
    if let Some(tx) = TX.get() { let _ = tx.send((generation(), i, path)); }
}

fn request_window() {
    let n = PATHS.with_borrow(|p| p.len());
    if n == 0 { return; }
    let i = IDX.get();
    for d in -2..=2 {
        let j = i as isize + d;
        if j >= 0 && (j as usize) < n { request(j as usize); }
    }
}

pub fn thumb(era: u32, i: usize, bmp: isize) {
    if era != generation() { unsafe { DeleteObject(bmp as _) }; return; }
    let old = CACHE.with_borrow_mut(|c| c.insert(i, bmp).unwrap_or(0));
    if old != 0 { unsafe { DeleteObject(old as _) }; }
    PENDING.with_borrow_mut(|p| { p.remove(&i); });
    invalidate();
}

pub fn select(path: &PathBuf) {
    if !OPEN.get() { return; }
    let Some(i) = PATHS.with_borrow(|p| p.iter().position(|x| x == path)) else { return };
    if i == IDX.get() { return; }
    IDX.set(i);
    request_window();
    invalidate();
}

fn invalidate() {
    let h = WND.get() as HWND;
    if !h.is_null() { unsafe { InvalidateRect(h, ptr::null(), 0) }; }
}

pub unsafe fn open(parent: HWND, paths: Vec<PathBuf>, i: usize) {
    if paths.is_empty() { return; }
    clear_cache();
    GEN.fetch_add(1, Relaxed);
    OWNER.set(parent as isize);
    IDX.set(i.min(paths.len() - 1));
    PATHS.with_borrow_mut(|p| *p = paths);
    OPEN.set(true);
    let (cw, ch) = client(parent);
    layout(cw, ch);
    relayout();
    request_window();
    invalidate();
    let h = WND.get() as HWND;
    if !h.is_null() { SetFocus(h); }
}

pub unsafe fn close() {
    OPEN.set(false);
    let h = WND.get() as HWND;
    if !h.is_null() { ShowWindow(h, SW_HIDE); }
    clear_cache();
    PATHS.with_borrow_mut(|p| p.clear());
    relayout();
    let list = super::ST.with_borrow(|s| s.list);
    if !list.is_null() { SetFocus(list); }
}

fn shift(delta: isize) {
    let n = PATHS.with_borrow(|p| p.len());
    if n < 2 { return; }
    let i = (IDX.get() as isize + delta).rem_euclid(n as isize) as usize;
    IDX.set(i);
    request_window();
    invalidate();
}

fn jump(last: bool) {
    let n = PATHS.with_borrow(|p| p.len());
    if n == 0 { return; }
    IDX.set(if last { n - 1 } else { 0 });
    request_window();
    invalidate();
}

fn current() -> Option<PathBuf> { PATHS.with_borrow(|p| p.get(IDX.get()).cloned()) }

fn open_in_viewer() {
    if let (Some(p), Some(exe)) = (current(), super::tool("photoview")) {
        let _ = std::process::Command::new(exe).arg(p).spawn();
    }
}

unsafe fn key(vk: u32) {
    match vk {
        0x25 => shift(-1), // Left
        0x27 => shift(1),  // Right
        0x24 => jump(false), // Home
        0x23 => jump(true),  // End
        0x1B | 0x08 => unsafe { close() }, // Esc / Backspace
        0x0D | 0x20 => open_in_viewer(), // Enter / Space
        _ => {}
    }
}

unsafe fn click(x: i32) {
    let h = WND.get() as HWND;
    let mut r: RECT = std::mem::zeroed();
    GetClientRect(h, &mut r);
    let wd = r.right;
    if x < wd / 3 { shift(-1); } else if x > wd * 2 / 3 { shift(1); } else { open_in_viewer(); }
}

unsafe fn paint(h: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(h, &mut ps);
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(h, &mut cr);
    if cr.right > 0 && cr.bottom > 0 {
        let mem = CreateCompatibleDC(dc);
        let bmp = CreateCompatibleBitmap(dc, cr.right, cr.bottom);
        let old = SelectObject(mem, bmp as *mut _);
        let brush = CreateSolidBrush(ui::pal().bg);
        FillRect(mem, &cr, brush);
        DeleteObject(brush as _);
        SetStretchBltMode(mem, HALFTONE);
        let n = PATHS.with_borrow(|p| p.len());
        if n > 0 {
            let avail = cr.bottom - ui::sc(30);
            let cw = cr.right / 3;
            let h1 = ((avail as f32 * 0.88) as i32).min(avail);
            let (w0, w1, w2) = (cw, (cw as f32 * 0.68) as i32, (cw as f32 * 0.46) as i32);
            let gap = ui::sc(10);
            let cx = cr.right / 2;
            let centers = [
                (cx - (w0 / 2 + gap + w1 + gap + w2 / 2), w2, (h1 as f32 * 0.46) as i32, -2),
                (cx + (w0 / 2 + gap + w1 + gap + w2 / 2), w2, (h1 as f32 * 0.46) as i32, 2),
                (cx - (w0 / 2 + gap + w1 / 2), w1, (h1 as f32 * 0.68) as i32, -1),
                (cx + (w0 / 2 + gap + w1 / 2), w1, (h1 as f32 * 0.68) as i32, 1),
                (cx, w0, h1, 0),
            ];
                for (x, box_w, box_h, off) in centers {
                    let i = IDX.get() as isize + off;
                    if i < 0 || i as usize >= n { continue; }
                    let Some(b) = CACHE.with_borrow(|c| c.get(&(i as usize)).copied()) else { continue };
                    let mut bm: BITMAP = std::mem::zeroed();
                    GetObjectW(b as _, size_of::<BITMAP>() as i32, &mut bm as *mut _ as _);
                    if bm.bmWidth <= 0 || bm.bmHeight == 0 { continue; }
                    let ih = bm.bmHeight.unsigned_abs() as i32;
                    let scale = (box_w as f32 / bm.bmWidth as f32).min(box_h as f32 / ih as f32);
                    let (dw, dh) = ((bm.bmWidth as f32 * scale) as i32, (ih as f32 * scale) as i32);
                    let (dx, dy) = (x - dw / 2, (avail - dh) / 2);
                    let sdc = CreateCompatibleDC(mem);
                    let prev = SelectObject(sdc, b as *mut _);
                    StretchBlt(mem, dx, dy, dw, dh, sdc, 0, 0, bm.bmWidth, ih, SRCCOPY);
                    SelectObject(sdc, prev);
                    DeleteDC(sdc);
                    if off == 0 {
                        let pen = CreatePen(PS_SOLID as i32, 1, ui::pal().accent);
                        let op = SelectObject(mem, pen as *mut _);
                        let ob = SelectObject(mem, GetStockObject(NULL_BRUSH) as *mut _);
                        Rectangle(mem, dx - 1, dy - 1, dx + dw + 1, dy + dh + 1);
                        SelectObject(mem, ob);
                        SelectObject(mem, op);
                        DeleteObject(pen as _);
                    }
                }
            let name = current().and_then(|p| p.file_name().map(|x| x.to_string_lossy().into_owned())).unwrap_or_default();
            let label = format!("{name}    {}/{}", IDX.get() + 1, n);
            let font = ui::font(13, 400, ui::ui_face());
            let of = SelectObject(mem, font as *mut _);
            SetTextColor(mem, ui::pal().text);
            SetBkMode(mem, TRANSPARENT as i32);
            let mut lr = RECT { left: 0, top: cr.bottom - ui::sc(26), right: cr.right, bottom: cr.bottom - ui::sc(4) };
            DrawTextW(mem, w(&label).as_ptr(), -1, &mut lr, DT_CENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            SelectObject(mem, of);
            DeleteObject(font as _);
        }
        BitBlt(dc, 0, 0, cr.right, cr.bottom, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        DeleteObject(bmp as _);
        DeleteDC(mem);
    }
    EndPaint(h, &ps);
}

pub unsafe extern "system" fn proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => 1,
        WM_PAINT => { paint(h); 0 }
        WM_KEYDOWN => { key(wp as u32); 0 }
        WM_GETDLGCODE => (DLGC_WANTARROWS | DLGC_WANTCHARS) as isize,
        WM_LBUTTONDOWN => { click((lp & 0xFFFF) as i32); 0 }
        _ => DefWindowProcW(h, m, wp, lp),
    }
}

/// Pictures of `rows`; `strict` requires them to be the majority (automatic opening in an image folder).
pub fn from_rows(dir: &PathBuf, rows: &[Row], selected: Option<&PathBuf>, strict: bool) -> Option<(Vec<PathBuf>, usize)> {
    let files: Vec<&Row> = rows.iter().filter(|r| !r.dir).collect();
    let pics: Vec<PathBuf> = files.iter().filter(|r| super::is_pic(&PathBuf::from(&r.name))).map(|r| dir.join(&r.name)).collect();
    if pics.is_empty() || (strict && pics.len() * 2 < files.len()) { return None; }
    let i = selected.and_then(|p| pics.iter().position(|x| x == p)).unwrap_or(0);
    Some((pics, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, dir: bool) -> Row {
        Row { name: name.into(), title: name.into(), dir, size: 0, time: 0, icon: 0, thumb: 0 }
    }

    #[test]
    fn from_rows_strict_needs_picture_majority() {
        let dir = PathBuf::from("C:\\pics");
        let rows = [row("a.png", false), row("b.jpg", false), row("notes.txt", false), row("sub", true)];
        assert!(from_rows(&dir, &rows, None, true).is_some());
        let rows = [row("a.png", false), row("notes.txt", false), row("todo.txt", false)];
        assert!(from_rows(&dir, &rows, None, true).is_none());
    }

    #[test]
    fn from_rows_non_strict_keeps_selected_picture() {
        let dir = PathBuf::from("C:\\mixed");
        let rows = [row("a.png", false), row("b.jpg", false), row("notes.txt", false)];
        let (pics, i) = from_rows(&dir, &rows, Some(&dir.join("b.jpg")), false).unwrap();
        assert_eq!(pics, [dir.join("a.png"), dir.join("b.jpg")]);
        assert_eq!(i, 1);
    }

    #[test]
    fn select_follows_list_picture() {
        let dir = PathBuf::from("C:\\pics");
        PATHS.with_borrow_mut(|p| *p = vec![dir.join("a.png"), dir.join("b.jpg")]);
        IDX.set(0);
        OPEN.set(true);
        select(&dir.join("b.jpg"));
        assert_eq!(IDX.get(), 1);
        select(&dir.join("notes.txt"));
        assert_eq!(IDX.get(), 1);
        OPEN.set(false);
    }
}
