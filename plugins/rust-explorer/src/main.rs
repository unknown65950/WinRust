// rust-explorer [path]  — file manager window (Explorer replacement).
// Virtual list (LVS_OWNERDATA, only visible rows are asked for text/icon), system icons, tabs,
// IFileOperation (recycle bin / undo / elevation prompts), real shell context menu with submenus, OLE drag and drop.
// Keys: Enter open, Backspace up, F2 rename, Del recycle, Ctrl+C/X/V copy/cut/paste, Ctrl+N new folder,
// Ctrl+T/W/Tab tabs, Ctrl+M thumbnails, Ctrl+L/F4 address bar, Ctrl+F or "? terms" in the bar = instant search through the rs service,
// Ctrl+Shift+F = cover-flow strip above the list (opens by itself in an image folder); right click / Menu key = shell context menu.
// Look: topbar's palette, painted tabs / toolbar / address pill and status line (src/ui.rs), dark title bar and list.
// Clipboard is the system one (Ctrl+C/X/V interoperate with Explorer). Also: Alt+Left/Right back/forward, Alt+Up up,
// Alt+Enter properties, F5 refresh, Ctrl+H hidden files, click a column header to sort, right click on empty space = folder menu.
// shortcuts: std::fs listing (not FindFirstFileEx large fetch), icons are extension-based (exe/lnk/ico/url/scr
// per file) and synchronous (thumbnails: one background thread, 96px, no disk cache), internal copy/cut clipboard, silent rename errors,
// search blocks the UI while it waits for the service (no async), the service pipe is admin-only so run as admin,
// drops always land in the current folder (not on a folder row), tabs remember only the folder (no selection/scroll).
// upgrade: each of those one at a time; then register as the folder-open handler.
// rust-explorer --desktop  - the desktop for rs-shell: borderless bottom-most color-keyed window with the Desktop folders
//   (user + public) as big icons over the wallpaper it paints itself (without explorer nothing else does): reads the BMP
//   that rs-shell converts into %LOCALAPPDATA%\rs-shell\wallpaper.bmp, "fill" style, scaled once at start.
//   shortcuts: wallpaper is loaded once (changing it needs a restart), icon labels are always white, no background menu /
//   drag-to-position / auto-arrange, items from the public desktop can't be renamed.
#![windows_subsystem = "windows"]
use std::{cell::RefCell, collections::HashMap, env, fs, os::windows::fs::MetadataExt, path::{Path, PathBuf}, ptr, sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed}};
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::{BLACK_BRUSH, GetStockObject, InvalidateRect}, System::LibraryLoader::GetModuleHandleW, UI::{Controls::*, Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL, VK_SHIFT}, Shell::*, WindowsAndMessaging::*}};

mod jobs; // our own copier, ZIP packer, ZIP/7z unpacker (background, progress in the status line)
mod ui; // the look: palette, painted tabs / toolbar / address pill / status line, dark theme
mod cover; // cover-flow strip above the list

static DESKTOP: AtomicBool = AtomicBool::new(false);
fn desk() -> bool { DESKTOP.load(Relaxed) }
const CLR_NONE: isize = 0xFFFF_FFFF; // list background / text background: let the parent's wallpaper show through
thread_local! { static WALL: std::cell::Cell<(isize, i32, i32)> = const { std::cell::Cell::new((0, 0, 0)) }; } // (HBITMAP, w, h)
/// What the bar reserves at the top and bottom of the screen: the work area topbar sets when there is no explorer.
fn work_strips() -> (i32, i32) {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    if unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut _ as *mut _, 0) } == 0 { return (0, 0); }
    (r.top.max(0), (unsafe { GetSystemMetrics(SM_CYSCREEN) } - r.bottom).max(0))
}
/// Margins of the desktop's icon list, from the bar's real size: its reserved strip (any height, top or bottom).
/// The dock style reserves nothing but floats over the bottom edge, so it gets a fixed gap while topbar runs.
fn desk_top() -> i32 { work_strips().0 + ui::sc(8) }
fn desk_bottom() -> i32 {
    let reserved = work_strips().1;
    if reserved > 0 { return reserved + ui::sc(4); }
    let bar_up = unsafe { !FindWindowW(w("topbar").as_ptr(), ptr::null()).is_null() };
    if bar_up && winrust_common::style::style() == winrust_common::style::Style::Dock { ui::sc(52) } else { ui::sc(8) }
}
const THUMB: i32 = 96;
const WM_THUMB: u32 = WM_APP + 2; // worker -> UI: wp = Box<(era, row, HBITMAP)>
static GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0); // bumped on every new listing; stale thumbnails are dropped
static TX: std::sync::OnceLock<std::sync::mpsc::Sender<(u32, usize, PathBuf)>> = std::sync::OnceLock::new();
const WM_GO: u32 = WM_APP + 1; // address bar: Enter (wp 13) / Esc (wp 27)
static EDIT_OLD: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

#[derive(Clone)]
struct Row { name: String, title: String, dir: bool, size: u64, time: u64, icon: i32, thumb: i32 } // time: FILETIME ticks; thumb: -2 not asked, -1 asked, >=0 image-list index

/// Back / forward trail of visited folders (one for the whole window, not per tab).
#[derive(Default)]
struct Hist { v: Vec<PathBuf>, pos: usize }
impl Hist {
    fn visit(&mut self, p: PathBuf) {
        if self.v.get(self.pos) == Some(&p) { return; }
        self.v.truncate(self.pos + usize::from(!self.v.is_empty())); // a new visit drops the forward trail
        self.v.push(p);
        self.pos = self.v.len() - 1;
    }
    fn back(&mut self) -> Option<PathBuf> { if self.pos > 0 { self.pos -= 1; Some(self.v[self.pos].clone()) } else { None } }
    fn forward(&mut self) -> Option<PathBuf> { if self.pos + 1 < self.v.len() { self.pos += 1; Some(self.v[self.pos].clone()) } else { None } }
}

struct State { list: HWND, tab: HWND, edit: HWND, dir: PathBuf, rows: Vec<Row>, thumbs: bool, himl: isize, tabs: Vec<PathBuf>, active: usize, hist: Hist }
thread_local! {
    static ST: RefCell<State> = RefCell::new(State { list: 0 as _, tab: 0 as _, edit: 0 as _, dir: PathBuf::new(), rows: vec![], thumbs: false, himl: 0, tabs: vec![], active: 0, hist: Hist { v: vec![], pos: 0 } });
    static WATCH: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; // last seen row names
    static SHELL_TITLES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}
static SORT_COL: AtomicUsize = AtomicUsize::new(0); // 0 name, 1 size, 2 modified
static SORT_ASC: AtomicBool = AtomicBool::new(true);
static SHOW_HIDDEN: AtomicBool = AtomicBool::new(false); // Ctrl+H
static NAV: AtomicBool = AtomicBool::new(false); // loading from the back / forward trail: do not record it again
static BUSY: AtomicBool = AtomicBool::new(false); // a blocking shell drag is in progress
const WM_NAV: u32 = WM_APP + 3; // wp: 1 back, 2 forward, 3 properties, 4 up (sent from the message loop for Alt+key)
const WM_JOB: u32 = WM_APP + 5; // jobs.rs worker finished: wp = Box<Option<String>> (the error, if any)
const WM_COVER: u32 = WM_APP + 6; // worker -> UI: wp = Box<(era, cover index, HBITMAP)>
const WM_DEVICE_CHANGE: u32 = 0x0219;
const DBT_DEVICEARRIVAL: u32 = 0x8000;
const DBT_DEVICEREMOVECOMPLETE: u32 = 0x8004;
const DBT_DEVTYP_VOLUME: u32 = 0x0002;
const DBT_DEVTYP_DEVICEINTERFACE: u32 = 0x0005;
const DBTF_NET: u16 = 0x0002;

#[repr(C)]
struct DevBroadcastHdr { dbch_size: u32, dbch_devicetype: u32, dbch_reserved: u32 }

#[repr(C)]
struct DevBroadcastVolume { dbcv_size: u32, dbcv_devicetype: u32, dbcv_reserved: u32, dbcv_unitmask: u32, dbcv_flags: u16 }

fn volume_mask(m: u32, wp: WPARAM, lp: LPARAM) -> Option<u32> {
    if m != WM_DEVICE_CHANGE || !matches!(wp as u32, DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE) { return None; }
    unsafe {
        let hdr = lp as *const DevBroadcastHdr;
        if hdr.is_null() || (*hdr).dbch_size < size_of::<DevBroadcastVolume>() as u32 || (*hdr).dbch_devicetype != DBT_DEVTYP_VOLUME { return None; }
        let v = &*(hdr as *const DevBroadcastVolume);
        if v.dbcv_flags & DBTF_NET != 0 || v.dbcv_unitmask == 0 { return None; }
        Some(v.dbcv_unitmask)
    }
}

fn device_interface_changed(m: u32, wp: WPARAM, lp: LPARAM) -> bool {
    if m != WM_DEVICE_CHANGE || !matches!(wp as u32, DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE) { return false; }
    unsafe {
        let hdr = lp as *const DevBroadcastHdr;
        !hdr.is_null() && (*hdr).dbch_devicetype == DBT_DEVTYP_DEVICEINTERFACE
    }
}

fn path_on_mask(p: &Path, mask: u32) -> bool {
    let s = p.to_string_lossy().to_lowercase();
    (0..26u32).any(|i| mask & (1 << i) != 0 && s.starts_with(&format!("{}:\\", (b'a' + i as u8) as char)))
}

fn row(name: String, m: Option<&fs::Metadata>) -> Row {
    let title = name.clone();
    Row { name, title, dir: m.is_some_and(|m| m.is_dir()), size: m.map_or(0, |m| m.len()), time: m.map_or(0, |m| m.last_write_time()), icon: -1, thumb: -2 }
}

fn shell_key(p: &Path) -> String { p.to_string_lossy().into_owned() }

fn remember_shell_title(p: &PathBuf, title: &str) {
    SHELL_TITLES.with_borrow_mut(|m| { m.insert(shell_key(p), title.to_string()); });
}

fn shell_title(p: &PathBuf) -> String {
    SHELL_TITLES.with_borrow(|m| m.get(&shell_key(p)).cloned()).unwrap_or_else(|| p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned()))
}

fn shell_devices() -> Vec<(String, PathBuf, bool)> { sh::devices() }

fn row_path(dir: &Path, name: &str) -> PathBuf {
    let p = Path::new(name);
    if p.is_absolute() || sh::is_shell(p) { p.to_path_buf() } else { dir.join(p) }
}

fn up_from(dir: &Path) -> Option<PathBuf> {
    if sh::is_shell(dir) { sh::parent(dir) } else { dir.parent().map(Path::to_path_buf) }
}

/// Folders always first; inside each group by the column (0 name, 1 size, 2 modified), ties by name.
fn sort_rows(rows: &mut [Row], col: usize, asc: bool) {
    let key = |r: &Row| r.name.rsplit('\\').next().unwrap_or("").to_lowercase();
    rows.sort_by(|a, b| {
        let o = match col { 1 => a.time.cmp(&b.time), 2 => type_text(&a.name, a.dir).cmp(&type_text(&b.name, b.dir)), 3 => a.size.cmp(&b.size), _ => key(a).cmp(&key(b)) };
        (!a.dir).cmp(&!b.dir).then(if asc { o } else { o.reverse() }).then_with(|| key(a).cmp(&key(b)))
    });
}

/// Explorer's "Type" column in short form: "File folder", "TXT file", "File".
fn type_text(name: &str, dir: bool) -> String {
    if dir { return "File folder".into(); }
    let file = name.rsplit('\\').next().unwrap_or(name);
    match file.rsplit_once('.') { Some((stem, e)) if !stem.is_empty() && !e.is_empty() => format!("{} file", e.to_uppercase()), _ => "File".into() }
}

/// Explorer shows file sizes in KB; the bigger ones read better in MB / GB.
fn size_text(b: u64) -> String {
    const MB: u64 = 1 << 20;
    if b < MB { format!("{} KB", b.div_ceil(1024)) } else if b < 1 << 30 { format!("{:.1} MB", b as f64 / MB as f64) } else { format!("{:.2} GB", b as f64 / (1u64 << 30) as f64) }
}

fn date_text(ft: u64) -> String {
    use windows_sys::Win32::{Foundation::{FILETIME, SYSTEMTIME}, System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime}};
    if ft == 0 { return String::new(); }
    unsafe {
        let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
        let (mut u, mut l): (SYSTEMTIME, SYSTEMTIME) = (std::mem::zeroed(), std::mem::zeroed());
        if FileTimeToSystemTime(&f, &mut u) == 0 || SystemTimeToTzSpecificLocalTime(ptr::null(), &u, &mut l) == 0 { return String::new(); }
        format!("{:04}-{:02}-{:02} {:02}:{:02}", l.wYear, l.wMonth, l.wDay, l.wHour, l.wMinute)
    }
}

/// Our own tools installed by the launcher (photoview, wiztree): the exe in Program Files, None when not installed.
fn tool(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env::var_os("ProgramFiles")?).join(name).join(format!("{name}.exe"));
    p.is_file().then_some(p)
}

/// Same list as photoview's EXTS.
fn is_pic(p: &std::path::Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["jpg", "jpeg", "jfif", "png", "bmp", "gif", "tif", "tiff", "webp", "heic", "avif"].contains(&e.to_ascii_lowercase().as_str()))
}

fn properties(path: &PathBuf) {
    if sh::is_shell(path) { let _ = sh::properties(path); return; }
    let (f, v) = (w(&path.to_string_lossy()), w("properties"));
    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = 0x0C; // SEE_MASK_INVOKEIDLIST
    sei.lpVerb = v.as_ptr(); sei.lpFile = f.as_ptr(); sei.nShow = SW_SHOW;
    unsafe { ShellExecuteExW(&mut sei) };
}

/// New folder / new empty file with a free name ("New folder", "New folder (2)", ...).
fn new_entry(dir: &PathBuf, base: &str, folder: bool) {
    let (stem, ext) = match base.rsplit_once('.') { Some((s, e)) if !folder => (s.to_string(), format!(".{e}")), _ => (base.to_string(), String::new()) };
    if sh::is_shell(dir) {
        let used = rows_of(dir).map(|v| v.into_iter().map(|r| r.title).collect::<Vec<_>>()).unwrap_or_default();
        let name = (1..).map(|n| if n == 1 { base.to_string() } else { format!("{stem} ({n}){ext}") }).find(|n| !used.iter().any(|u| u.eq_ignore_ascii_case(n))).unwrap_or_else(|| base.to_string());
        let _ = sh::new_item(dir, &name, folder);
        return;
    }
    let path = (1..).map(|n| dir.join(if n == 1 { base.to_string() } else { format!("{stem} ({n}){ext}") })).find(|p| !p.exists());
    if let Some(p) = path { let _ = if folder { fs::create_dir(p) } else { fs::File::create(p).map(drop) }; }
}

/// Pastes what is on the system clipboard (files copied or cut here or in any other file manager).
/// Copies and moves to another drive run in our own copier (jobs.rs: parallel on SSDs, unbuffered for big files); moves on the
/// same drive (a rename), shell items, and folders that need elevation go through the shell.
fn paste(h: HWND, dir: &PathBuf) {
    let Some((paths, cut)) = sh::clip_get() else { return };
    let shell = sh::is_shell(dir) || paths.iter().any(|p| sh::is_shell(p));
    let fast = !shell && jobs::can_fast_copy(&paths, dir) && !(cut && paths.iter().all(|p| jobs::same_volume(p, dir))) && jobs::writable(dir);
    if fast && !jobs::busy() {
        jobs::copy(h as isize, paths, dir.clone(), cut);
    } else {
        let _ = sh::fileop(h, if cut { sh::Op::Move } else { sh::Op::Copy }, &paths, dir);
    }
    if cut { sh::clip_clear(); }
}

const MENU_ZIP: u32 = 0x9001;
const MENU_EXTRACT: u32 = 0x9002;
const MENU_PIN: u32 = 0x9003;

/// The shell menu for the selection, with our "Compress to ZIP" / "Extract here" on top.
fn item_menu(h: HWND, dir: &Path, paths: &[PathBuf]) {
    let shell = sh::is_shell(dir) || paths.iter().any(|p| sh::is_shell(p));
    let mut extra = if shell { vec![] } else { vec![(MENU_ZIP, "Compress to ZIP")] };
    if !shell && paths.iter().all(|p| p.is_file() && jobs::is_archive(p)) {
        extra.insert(0, (MENU_EXTRACT, "Extract here"));
    }
    if !shell {
        if let [p] = paths {
            if p.is_dir() {
                extra.push((MENU_PIN, if ui::pins().contains(p) { "Unpin from Quick access" } else { "Pin to Quick access" }));
            }
        }
    }
    let picked = sh::show(h, paths, if jobs::busy() { &[] } else { &extra }).ok().flatten();
    match picked {
        Some(MENU_PIN) => ui::toggle_pin(&paths[0]),
        Some(MENU_ZIP) => {
            let base = if paths.len() == 1 { paths[0].file_stem().unwrap_or_default().to_string_lossy().into_owned() } else { label(&dir.to_path_buf()) };
            let dest = jobs::free_name(dir, &format!("{base}.zip"), false);
            jobs::zip(h as isize, paths.to_vec(), dest);
        }
        Some(MENU_EXTRACT) => _ = jobs::extract(h as isize, paths.to_vec(), dir.to_path_buf()),
        _ => reload(h),
    }
}

/// Right click on empty space: the folder's own menu.
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn bg_menu(h: HWND) {
    let dir = cur_dir();
    let m = CreatePopupMenu();
    for (id, t) in [(1usize, "New folder"), (2, "New text file"), (3, "Paste"), (4, "Refresh"), (5, "Properties")] { AppendMenuW(m, MF_STRING, id, w(t).as_ptr()); }
    let disk = tool("wiztree").filter(|_| !sh::is_shell(&dir));
    if disk.is_some() { AppendMenuW(m, MF_STRING, 6, w("Disk usage (WinRust Disk)").as_ptr()); }
    let mut pt: POINT = std::mem::zeroed();
    GetCursorPos(&mut pt);
    let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, h, ptr::null());
    DestroyMenu(m);
    match id {
        6 => { // wiztree takes the drive letter and asks for admin rights itself
            let drive: String = dir.to_string_lossy().chars().take(2).collect();
            if let Some(exe) = disk { let _ = std::process::Command::new(exe).arg(drive).spawn(); }
        }
        1 => new_entry(&dir, "New folder", true),
        2 => new_entry(&dir, "New text document.txt", false),
        3 => paste(h, &dir),
        5 => properties(&dir),
        _ => {}
    }
    if id != 0 { reload(h); }
}

/// Back / forward along the visited-folder trail.
fn nav(h: HWND, back: bool) {
    let to = ST.with_borrow_mut(|st| if back { st.hist.back() } else { st.hist.forward() });
    if let Some(p) = to {
        NAV.store(true, Relaxed);
        load(h, p);
        NAV.store(false, Relaxed);
    }
}

fn w(s: &str) -> Vec<u16> { s.encode_utf16().chain([0]).collect() }

fn put(h: HWND, msg: u32, wp: usize, lp: isize) -> isize { unsafe { SendMessageW(h, msg, wp, lp) } }

/// F2 on an item: start the list's label edit in place (no reload) and select the name, not the extension, like Explorer.
fn label_edit(list: HWND, i: i32, name: &str) {
    if i < 0 { return; }
    let edit = put(list, LVM_EDITLABELW, i as usize, 0) as HWND;
    if edit.is_null() { return; }
    let n = name.rfind('.').filter(|&d| d > 0 && d as usize + 1 < name.len()).map(|d| d as isize).unwrap_or(-1);
    put(edit, EM_SETSEL, 0, n);
}

fn wstr(p: *const u16) -> String {
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 { n += 1; }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

pub fn cur_dir() -> PathBuf { ST.with_borrow(|st| st.dir.clone()) }

/// System image-list index. Extension-based (no disk access) except for files that carry their own icon.
fn icon_of(p: &PathBuf, dir: bool) -> i32 {
    if sh::is_shell(p) { return sh::icon_of_shell(p); }
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let own = !dir && matches!(ext.as_str(), "exe" | "lnk" | "ico" | "url" | "scr");
    let mut fi: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let attr = if dir { 0x10 } else { 0x80 }; // DIRECTORY / NORMAL
    let flags = SHGFI_SYSICONINDEX | SHGFI_SMALLICON | if own { 0 } else { SHGFI_USEFILEATTRIBUTES };
    unsafe { SHGetFileInfoW(w(&p.to_string_lossy()).as_ptr(), attr, &mut fi, size_of::<SHFILEINFOW>() as u32, flags) };
    fi.iIcon
}

/// The tabs are painted by the chrome window from State.tabs: "setting a tab's text" is just a repaint.
fn tab_text(tab: HWND, _i: usize, _text: &str, _insert: bool) {
    if !tab.is_null() { unsafe { InvalidateRect(tab, ptr::null(), 0) }; }
}

/// The text of a desktop row. Rows from the public desktop carry absolute paths: only the file name is shown, and shortcuts
/// lose their ".lnk" / ".url" like in Explorer. Other windows show the row as it is.
fn shown_name(name: &str, desktop: bool) -> String {
    if !desktop { return name.to_string(); }
    let file = std::path::Path::new(name).file_name().map_or_else(|| name.to_string(), |n| n.to_string_lossy().into_owned());
    let lower = file.to_lowercase();
    if lower.ends_with(".lnk") || lower.ends_with(".url") { file[..file.len() - 4].to_string() } else { file }
}

fn label(dir: &PathBuf) -> String {
    if sh::is_shell(dir) { shell_title(dir) } else { dir.file_name().map_or_else(|| dir.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned()) }
}

/// Rows of `dir`, folders first. Desktop mode merges the public desktop (absolute names) and hides hidden files.
/// None = `dir` itself is unreadable.
fn rows_of(dir: &PathBuf) -> Option<Vec<Row>> {
    if sh::is_shell(dir) {
        return sh::children(dir).map(|v| v.into_iter().map(|(title, path, folder)| {
            remember_shell_title(&path, &title);
            let mut r = row(path.to_string_lossy().into_owned(), None);
            r.title = title;
            r.dir = folder;
            r
        }).collect());
    }
    let mut dirs = vec![dir.clone()];
    if desk() { dirs.extend(sh::desktop_dirs().into_iter().skip(1)); }
    let mut rows = vec![];
    for (n, d) in dirs.iter().enumerate() {
        let Ok(rd) = fs::read_dir(d) else { if n == 0 { return None } else { continue } };
        for e in rd.flatten() {
            let m = e.metadata().ok();
            if (desk() || !SHOW_HIDDEN.load(Relaxed)) && m.as_ref().is_some_and(|m| m.file_attributes() & 2 != 0) { continue; } // FILE_ATTRIBUTE_HIDDEN
            let name = if n == 0 { e.file_name().to_string_lossy().into_owned() } else { e.path().to_string_lossy().into_owned() };
            rows.push(row(name, m.as_ref()));
        }
    }
    sort_rows(&mut rows, SORT_COL.load(Relaxed), SORT_ASC.load(Relaxed));
    Some(rows)
}

fn watch_signature(rows: &[Row]) -> Vec<String> { rows.iter().map(|r| r.name.clone()).collect() }

/// Loads `dir` into the virtual list and the active tab. Unreadable dir (access denied, gone) leaves the view unchanged.
/// Never call with ST borrowed: the list sends GETDISPINFO back into the window proc.
fn load(h: HWND, dir: PathBuf) {
    let Some(rows) = rows_of(&dir) else { return };
    let old_dir = ST.with_borrow(|st| st.dir.clone());
    let changed = !desk() && old_dir != dir;
    let pics = if changed && !sh::is_shell(&dir) { cover::from_rows(&dir, &rows, None, true) } else { None };
    WATCH.with_borrow_mut(|w| *w = watch_signature(&rows));
    if !desk() { unsafe { SetWindowTextW(h, w(&dir.to_string_lossy()).as_ptr()) }; }
    let (list, tab, edit, active, n) = ST.with_borrow_mut(|st| {
        if let Some(t) = st.tabs.get_mut(st.active) { *t = dir.clone(); }
        st.dir = dir.clone(); st.rows = rows;
        if !NAV.load(Relaxed) { st.hist.visit(dir.clone()); }
        (st.list, st.tab, st.edit, st.active, st.rows.len())
    });
    unsafe { SetWindowTextW(edit, w(&dir.to_string_lossy()).as_ptr()) };
    ui::set_search(None); // a folder again: the pill shows its path
    tab_text(tab, active, &label(&dir), false);
    ui::nav_dirty(); // the pane highlights where we are
    reset_thumbs();
    put(list, LVM_DELETEALLITEMS, 0, 0);
    if desk() { desk::fill(list, n); } else { put(list, LVM_SETITEMCOUNT, n, 0); }
    if changed {
        if let Some((paths, i)) = pics { unsafe { cover::open(h, paths, i) }; }
        else if cover::is_open() { unsafe { cover::close() }; }
    }
}

/// Asks the rust-search service of drive `d` (see plugins/rust-search): terms in, one path per line out.
/// None = no service for that drive or no access (its pipe is admin-only unless started with --allow).
fn ask(d: char, q: &str) -> Option<String> {
    use std::{fs::OpenOptions, io::{Read, Write}};
    let mut p = OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\rs-search-{d}")).ok()?;
    p.write_all(q.as_bytes()).ok()?;
    let (mut out, mut chunk) = (Vec::new(), [0u8; 1 << 16]);
    // the server ends its reply by hanging up, which surfaces as a read error: treat any error as EOF
    while let Ok(n @ 1..) = p.read(&mut chunk) { out.extend_from_slice(&chunk[..n]); }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Fills the list with search hits from every drive's service. Rows hold absolute paths, so joining them
/// onto the current dir still yields the right file. Blocks the UI for the duration of the query (fast).
/// Never call with ST borrowed.
fn search(h: HWND, q: &str) {
    let mut rows = vec![];
    let mut more = String::new();
    let mut indexed = false; // some drive had a rust-search service
    for d in 'A'..='Z' {
        let Some(reply) = ask(d, q) else { continue };
        indexed = true;
        for line in reply.lines() {
            if line.starts_with("... ") { more = format!(", {}", line.trim_start_matches("... ")); continue; }
            let m = fs::metadata(line).ok();
            rows.push(row(line.to_string(), m.as_ref()));
        }
    }
    if !indexed {
        // no rust-search installed: look through the current folder and below instead
        let (local, cut, flipped) = local_search(&cur_dir(), q);
        if flipped { more = format!(" for \u{201C}{}\u{201D}", flip_layout(q)); } // typed in the wrong layout: shown what was meant
        rows = local;
        if cut { more += ", stopped early: narrow the search"; }
    }
    let n = rows.len();
    let where_ = if indexed { "" } else { " in this folder" };
    let title = if n == 0 { format!("search: {q} - nothing{where_}") } else { format!("search: {q} - {n} hits{where_}{more}") };
    ui::set_search(Some(q.to_string()));
    unsafe { SetWindowTextW(h, w(&title).as_ptr()) };
    let list = ST.with_borrow_mut(|st| { st.rows = rows; st.list });
    reset_thumbs();
    put(list, LVM_DELETEALLITEMS, 0, 0);
    put(list, LVM_SETITEMCOUNT, n, 0);
}

/// The same keys typed in the other layout (QWERTY <-> ЙЦУКЕН): "сфкпщ" -> "cargo", "ljrevtyns" -> "документы".
fn flip_layout(q: &str) -> String {
    let en: Vec<char> = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`QWERTYUIOP{}ASDFGHJKL:\"ZXCVBNM<>~".chars().collect();
    let ru: Vec<char> = "йцукенгшщзхъфывапролджэячсмитьбюёЙЦУКЕНГШЩЗХЪФЫВАПРОЛДЖЭЯЧСМИТЬБЮЁ".chars().collect();
    q.chars().map(|c| en.iter().position(|&x| x == c).map(|i| ru[i]).or_else(|| ru.iter().position(|&x| x == c).map(|i| en[i])).unwrap_or(c)).collect()
}

/// Files and folders under `dir` whose name contains `q` (any case), as rows with absolute names. Walks breadth-first and
/// stops after LOCAL_MAX hits or LOCAL_MS so a search from C:\ cannot hang the window. The same walk also matches `q` typed
/// in the other keyboard layout; those hits are used only when `q` itself found nothing.
/// Returns (rows, stopped early, rows are for the other layout).
/// shortcut: blocks the UI while it walks (up to LOCAL_MS); move to a thread if deeper searches are wanted.
fn local_search(dir: &Path, q: &str) -> (Vec<Row>, bool, bool) {
    const LOCAL_MAX: usize = 500;
    const LOCAL_MS: u128 = 1500;
    let (q, t0) = (q.to_lowercase(), std::time::Instant::now());
    let alt = flip_layout(&q);
    let (mut out, mut other, mut queue) = (vec![], vec![], std::collections::VecDeque::from([dir.to_path_buf()]));
    let mut cut = false;
    'walk: while let Some(d) = queue.pop_front() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            if out.len() >= LOCAL_MAX || t0.elapsed().as_millis() > LOCAL_MS { cut = true; break 'walk; }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && !ft.is_symlink() { queue.push_back(e.path()); } // no junction loops
            let name = e.file_name().to_string_lossy().to_lowercase();
            let hit = |v: &mut Vec<Row>| v.push(row(e.path().to_string_lossy().into_owned(), e.metadata().ok().as_ref()));
            if name.contains(&q) { hit(&mut out); } else if alt != q && other.len() < LOCAL_MAX && name.contains(&alt) { hit(&mut other); }
        }
    }
    if out.is_empty() && !other.is_empty() { (other, cut, true) } else { (out, cut, false) }
}

/// Drops all thumbnails and invalidates in-flight ones; call whenever the rows are replaced.
fn reset_thumbs() {
    GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let himl = ST.with_borrow(|st| st.himl);
    unsafe { ImageList_Remove(himl as _, -1) };
}

fn reload(h: HWND) { load(h, cur_dir()); }

/// Polls the visible folder: a new / removed / renamed row triggers the same reload as F5.
/// Skipped during a drag, an in-place rename and search results, where a reload would throw away what the user is doing.
fn auto_reload(h: HWND) {
    if BUSY.load(Relaxed) || ui::searching() || sh::is_shell(&cur_dir()) { return; }
    let list = ST.with_borrow(|st| st.list);
    if list.is_null() || put(list, LVM_GETEDITCONTROL, 0, 0) != 0 { return; }
    let dir = cur_dir();
    let Some(rows) = rows_of(&dir) else { return };
    let names = watch_signature(&rows);
    if WATCH.with_borrow(|w| w == &names) { return; }
    load(h, dir);
}

/// (list, current dir, full paths of the selected rows)
fn selected() -> (HWND, PathBuf, Vec<PathBuf>) {
    ST.with_borrow(|st| {
        let (mut sel, mut i) = (vec![], -1isize);
        loop {
            i = put(st.list, LVM_GETNEXTITEM, i as usize, LVNI_SELECTED as isize);
            let Some(r) = usize::try_from(i).ok().and_then(|i| st.rows.get(i)) else { break };
            sel.push(row_path(&st.dir, &r.name));
        }
        (st.list, st.dir.clone(), sel)
    })
}

fn col(list: HWND, i: i32, text: &str, width: i32, right: bool) {
    let mut t = w(text);
    let mut c: LVCOLUMNW = unsafe { std::mem::zeroed() };
    c.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_FMT; c.pszText = t.as_mut_ptr(); c.cx = width;
    c.fmt = if right { LVCFMT_RIGHT } else { LVCFMT_LEFT };
    put(list, LVM_INSERTCOLUMNW, i as usize, &c as *const _ as isize);
}

/// Opens a new tab on the current folder.
fn tab_new() {
    let tab = ST.with_borrow_mut(|st| { st.tabs.push(st.dir.clone()); st.active = st.tabs.len() - 1; st.tab });
    tab_text(tab, 0, "", true);
}

fn tab_select(h: HWND, i: usize) {
    let dir = ST.with_borrow_mut(|st| { st.active = i; st.tabs[i].clone() });
    load(h, dir);
}

fn tab_close(h: HWND) { let a = ST.with_borrow(|st| st.active); tab_close_at(h, a); }

/// Closes tab `i` (the last one stays). The listing changes only when the active tab was the one closed.
fn tab_close_at(h: HWND, i: usize) {
    let (tab, was_active) = ST.with_borrow_mut(|st| {
        if st.tabs.len() < 2 || i >= st.tabs.len() { return (st.tab, None); }
        st.tabs.remove(i);
        let before = st.active;
        st.active = if i < before { before - 1 } else { before.min(st.tabs.len() - 1) };
        (st.tab, Some(i == before))
    });
    match was_active {
        Some(true) => { let a = ST.with_borrow(|st| st.active); tab_select(h, a); }
        Some(false) => tab_text(tab, 0, "", false),
        None => {}
    }
}

fn toggle_view(h: HWND) {
    let (list, on) = ST.with_borrow_mut(|st| { st.thumbs = !st.thumbs; (st.list, st.thumbs) });
    ui::row_height(list, !on);
    put(list, LVM_SETVIEW, if on { LV_VIEW_ICON } else { LV_VIEW_DETAILS } as usize, 0);
    reload(h);
}

/// Address box in search mode ("? terms").
fn search_focus() {
    let edit = ST.with_borrow(|st| st.edit);
    unsafe {
        SetWindowTextW(edit, w("? ").as_ptr());
        SetFocus(edit);
    }
    put(edit, EM_SETSEL, 2, 2);
}

/// Wallpaper for the desktop window.
mod wall {
    use windows_sys::Win32::{Graphics::Gdi::*, UI::WindowsAndMessaging::{IMAGE_BITMAP, LR_LOADFROMFILE, LoadImageW}};

    /// The BMP scaled ("fill": cropped to the screen's aspect) to cw x ch. 0 = no wallpaper file.
    pub fn load(cw: i32, ch: i32) -> isize {
        let Ok(base) = std::env::var("LOCALAPPDATA") else { return 0 };
        let path: Vec<u16> = format!(r"{base}\rs-shell\wallpaper.bmp").encode_utf16().chain([0]).collect();
        unsafe {
            let src = LoadImageW(std::ptr::null_mut(), path.as_ptr(), IMAGE_BITMAP, 0, 0, LR_LOADFROMFILE);
            if src.is_null() { return 0; }
            let mut bm: BITMAP = std::mem::zeroed();
            GetObjectW(src, size_of::<BITMAP>() as i32, &mut bm as *mut _ as _);
            let screen = GetDC(std::ptr::null_mut());
            let dst = CreateCompatibleBitmap(screen, cw, ch);
            let (sdc, ddc) = (CreateCompatibleDC(screen), CreateCompatibleDC(screen));
            let (o1, o2) = (SelectObject(sdc, src), SelectObject(ddc, dst));
            SetStretchBltMode(ddc, HALFTONE);
            let (sw, sh) = if bm.bmWidth * ch > bm.bmHeight * cw { (bm.bmHeight * cw / ch, bm.bmHeight) } else { (bm.bmWidth, bm.bmWidth * ch / cw) };
            StretchBlt(ddc, 0, 0, cw, ch, sdc, (bm.bmWidth - sw) / 2, (bm.bmHeight - sh) / 2, sw, sh, SRCCOPY);
            SelectObject(sdc, o1); SelectObject(ddc, o2);
            DeleteDC(sdc); DeleteDC(ddc); ReleaseDC(std::ptr::null_mut(), screen); DeleteObject(src);
            dst as isize
        }
    }

    pub fn paint(dc: HDC, bmp: isize, cw: i32, ch: i32) {
        unsafe {
            let m = CreateCompatibleDC(dc);
            let old = SelectObject(m, bmp as _);
            BitBlt(dc, 0, 0, cw, ch, m, 0, 0, SRCCOPY);
            SelectObject(m, old);
            DeleteDC(m);
        }
    }
}

/// Shell integration through COM (windows crate): file operations, context menu, drag and drop.
mod sh {
    use std::{cell::{Cell, RefCell}, ffi::c_void, os::windows::ffi::OsStrExt, path::{Path, PathBuf}};
    use windows::{core::*, Win32::{Foundation::*, System::{Com::*, DataExchange::RegisterClipboardFormatW, Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock}, Ole::*, SystemServices::{MODIFIERKEYS_FLAGS, SFGAO_FOLDER}}, UI::{Shell::{Common::ITEMIDLIST, *}, WindowsAndMessaging::*}}};
    use windows_sys::Win32::{Foundation::POINT as SysPoint, Graphics::Gdi::ScreenToClient, UI::Controls::{LVHITTESTINFO, LVM_HITTEST}};

    #[derive(Clone, Copy)]
    pub enum Op { Copy, Move, Delete }

    /// [user Desktop, public Desktop] (the user one may be redirected, e.g. to OneDrive).
    pub fn desktop_dirs() -> Vec<PathBuf> {
        [&FOLDERID_Desktop, &FOLDERID_PublicDesktop].iter().filter_map(|id| unsafe {
            let p = SHGetKnownFolderPath(*id, KF_FLAG_DEFAULT, None).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const c_void));
            s.map(PathBuf::from)
        }).collect()
    }

    /// A known folder where Windows says it is (it may be moved to another drive).
    pub fn known_dir(id: &GUID) -> Option<PathBuf> {
        unsafe {
            let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const c_void));
            s.map(PathBuf::from)
        }
    }

    pub fn init() { unsafe { let _ = OleInitialize(None); } }

    pub const PORTABLE_DEVICES: &str = "shell:::{35786D3C-B075-49B9-88DD-029876E11C01}";
    thread_local! { static DEVNOTIFY: Cell<isize> = const { Cell::new(0) }; }

    pub fn is_shell(p: &Path) -> bool {
        let s = p.to_string_lossy().to_lowercase();
        s.starts_with("::{") || s.starts_with("shell:")
    }

    pub fn watch_devices(h: *mut c_void) {
        unsafe {
            let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
                dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
                dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
                dbcc_reserved: 0,
                dbcc_classguid: GUID::from_u128(0x6ac27878_a6fa_4155_ba85_f98f491d4f33),
                dbcc_name: [0],
            };
            if let Ok(hn) = RegisterDeviceNotificationW(HANDLE(h), &filter as *const _ as *const c_void, DEVICE_NOTIFY_WINDOW_HANDLE) {
                DEVNOTIFY.set(hn.0 as isize);
            }
        }
    }

    pub fn unwatch_devices() {
        let p = DEVNOTIFY.replace(0);
        if p != 0 { unsafe { let _ = UnregisterDeviceNotification(HDEVNOTIFY(p as *mut c_void)); } }
    }

    fn name(it: &IShellItem, sigdn: SIGDN) -> Option<String> {
        unsafe {
            let p = it.GetDisplayName(sigdn).ok()?;
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const c_void));
            Some(s)
        }
    }

    pub fn children(dir: &Path) -> Option<Vec<(String, PathBuf, bool)>> {
        unsafe {
            let wide = wide(&dir.to_path_buf());
            let folder: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
            let en = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems).ok()?;
            let mut out = vec![];
            loop {
                let mut items = [None];
                let mut got = 0u32;
                if en.Next(&mut items, Some(&mut got)).is_err() || got == 0 { break; }
                let Some(it) = items[0].take() else { break };
                let (Some(title), Some(path)) = (name(&it, SIGDN_NORMALDISPLAY), name(&it, SIGDN_DESKTOPABSOLUTEPARSING)) else { continue };
                if title.is_empty() || path.is_empty() { continue; }
                let is_dir = it.GetAttributes(SFGAO_FOLDER).map(|a| a.0 & SFGAO_FOLDER.0 != 0).unwrap_or(false);
                out.push((title, PathBuf::from(path), is_dir));
            }
            Some(out)
        }
    }

    pub fn devices() -> Vec<(String, PathBuf, bool)> { children(Path::new(PORTABLE_DEVICES)).unwrap_or_default() }

    pub fn parent(p: &Path) -> Option<PathBuf> {
        unsafe {
            let it: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide(&p.to_path_buf()).as_ptr()), None).ok()?;
            let up = it.GetParent().ok()?;
            name(&up, SIGDN_DESKTOPABSOLUTEPARSING).map(PathBuf::from)
        }
    }

    fn invoke(p: &PathBuf, verb: PCWSTR) -> bool {
        unsafe {
            let mut pidl = std::ptr::null_mut();
            if SHParseDisplayName(PCWSTR(wide(p).as_ptr()), None, &mut pidl, 0, None).is_err() { return false; }
            let _owned = Pidls(vec![pidl]);
            let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
            sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
            sei.fMask = SEE_MASK_INVOKEIDLIST;
            sei.lpVerb = verb;
            sei.lpIDList = pidl as *mut c_void;
            sei.nShow = SW_SHOW.0;
            ShellExecuteExW(&mut sei).is_ok()
        }
    }

    pub fn open(p: &PathBuf) -> bool { invoke(p, w!("open")) }

    pub fn properties(p: &PathBuf) -> bool { invoke(p, w!("properties")) }

    pub fn rename(p: &PathBuf, new: &str) -> Result<()> {
        unsafe {
            let fo: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            let it = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide(p).as_ptr()), None)?;
            let n: Vec<u16> = new.encode_utf16().chain([0]).collect();
            fo.RenameItem(&it, PCWSTR(n.as_ptr()), None::<&IFileOperationProgressSink>)?;
            fo.PerformOperations()
        }
    }

    pub fn new_item(dir: &PathBuf, name: &str, folder: bool) -> Result<()> {
        unsafe {
            let fo: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            let dest = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide(dir).as_ptr()), None)?;
            let n: Vec<u16> = name.encode_utf16().chain([0]).collect();
            let attr = if folder { 0x10 } else { 0x80 };
            fo.NewItem(&dest, attr, PCWSTR(n.as_ptr()), PCWSTR::null(), None::<&IFileOperationProgressSink>)?;
            fo.PerformOperations()
        }
    }

    fn wide(p: &PathBuf) -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() }

    /// Copy / move into `to`, or recycle. Must not be called while ST is borrowed (the shell pumps messages).
    pub fn fileop(h: *mut c_void, op: Op, from: &[PathBuf], to: &PathBuf) -> Result<()> {
        unsafe {
            let fo: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            fo.SetOwnerWindow(HWND(h))?;
            fo.SetOperationFlags(if matches!(op, Op::Delete) { FOF_ALLOWUNDO } else { FOF_RENAMEONCOLLISION })?;
            let item = |p: &PathBuf| SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide(p).as_ptr()), None);
            let dest = if matches!(op, Op::Delete) { None } else { Some(item(to)?) };
            for p in from {
                let it = item(p)?;
                match (op, &dest) {
                    (Op::Copy, Some(d)) => fo.CopyItem(&it, d, PCWSTR::null(), None)?,
                    (Op::Move, Some(d)) => fo.MoveItem(&it, d, PCWSTR::null(), None)?,
                    _ => fo.DeleteItem(&it, None)?,
                }
            }
            fo.PerformOperations()
        }
    }

    /// Background thumbnail loader: GetImage for each (era, row, path) asked for, result posted to the main window.
    pub fn start_thumbs(main: isize) -> std::sync::mpsc::Sender<(u32, usize, PathBuf)> {
        start_images(main, super::THUMB, super::WM_THUMB, false)
    }

    /// The same loader for the cover-flow strip: larger images, posted to cover::thumb().
    pub fn start_cover_thumbs(main: isize) -> std::sync::mpsc::Sender<(u32, usize, PathBuf)> {
        start_images(main, 320, super::WM_COVER, true)
    }

    fn start_images(main: isize, size: i32, msg: u32, cover: bool) -> std::sync::mpsc::Sender<(u32, usize, PathBuf)> {
        let (tx, rx) = std::sync::mpsc::channel::<(u32, usize, PathBuf)>();
        std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            for (era, row, path) in rx {
                let stale = if cover { era != super::cover::generation() } else { era != super::GEN.load(std::sync::atomic::Ordering::Relaxed) };
                if stale { continue; }
                let Ok(f) = SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(PCWSTR(wide(&path).as_ptr()), None) else { continue };
                let Ok(bmp) = f.GetImage(SIZE { cx: size, cy: size }, SIIGBF_BIGGERSIZEOK) else { continue };
                let padded = pad_to(bmp.0, size);
                windows_sys::Win32::Graphics::Gdi::DeleteObject(bmp.0 as _);
                if padded.is_null() { continue; }
                let b = Box::new((era, row, padded as isize));
                let _ = PostMessageW(Some(HWND(main as *mut c_void)), msg, WPARAM(Box::into_raw(b) as usize), LPARAM(0));
            }
        });
        tx
    }

    /// Centers `src` on a transparent `t` x `t` bitmap: ImageList_Add needs exact sizes.
    unsafe fn pad_to(src: *mut c_void, t: i32) -> *mut c_void {
        use windows_sys::Win32::Graphics::Gdi::*;
        unsafe {
            let mut bm: BITMAP = std::mem::zeroed();
            GetObjectW(src as _, size_of::<BITMAP>() as i32, &mut bm as *mut _ as _);
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: t, biHeight: -t, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
            let mut bits = std::ptr::null_mut();
            let dst = CreateDIBSection(std::ptr::null_mut(), &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if dst.is_null() { return std::ptr::null_mut(); }
            let (dc, sdc) = (CreateCompatibleDC(std::ptr::null_mut()), CreateCompatibleDC(std::ptr::null_mut()));
            let (o1, o2) = (SelectObject(dc, dst), SelectObject(sdc, src as _));
            BitBlt(dc, (t - bm.bmWidth) / 2, (t - bm.bmHeight) / 2, bm.bmWidth, bm.bmHeight, sdc, 0, 0, SRCCOPY);
            SelectObject(dc, o1); SelectObject(sdc, o2);
            DeleteDC(dc); DeleteDC(sdc);
            dst
        }
    }

    /// Puts `paths` on the system clipboard like Explorer: the shell's own data object plus the "Preferred DropEffect"
    /// marker (copy or cut), so Ctrl+V works in a real Explorer window and in other file managers.
    pub fn clip_set(h: *mut c_void, paths: &[PathBuf], cut: bool) -> Result<()> {
        unsafe {
            let data: IDataObject = ui(HWND(h), paths)?;
            let fmt = FORMATETC { cfFormat: RegisterClipboardFormatW(w!("Preferred DropEffect")) as u16, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0 as u32, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
            let hg = GlobalAlloc(GMEM_MOVEABLE, 4)?;
            *(GlobalLock(hg) as *mut u32) = if cut { DROPEFFECT_MOVE.0 } else { DROPEFFECT_COPY.0 };
            let _ = GlobalUnlock(hg);
            let med = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: hg }, pUnkForRelease: std::mem::ManuallyDrop::new(None) };
            data.SetData(&fmt, &med, true)?; // the data object owns the memory from here
            OleSetClipboard(&data)
        }
    }

    /// What is on the system clipboard as files: (paths, cut?). None = no files there.
    pub fn clip_get() -> Option<(Vec<PathBuf>, bool)> {
        unsafe {
            let o = OleGetClipboard().ok()?;
            let paths = hdrop_paths(&o).ok().filter(|p| !p.is_empty())?;
            let fmt = FORMATETC { cfFormat: RegisterClipboardFormatW(w!("Preferred DropEffect")) as u16, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0 as u32, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
            let cut = o.GetData(&fmt).ok().is_some_and(|mut m| {
                let p = GlobalLock(m.u.hGlobal) as *const u32;
                let v = if p.is_null() { 0 } else { *p };
                let _ = GlobalUnlock(m.u.hGlobal);
                ReleaseStgMedium(&mut m);
                v & DROPEFFECT_MOVE.0 != 0
            });
            Some((paths, cut))
        }
    }

    pub fn clip_clear() { unsafe { let _ = OleSetClipboard(None); } }

    struct Pidls(Vec<*mut ITEMIDLIST>);
    impl Drop for Pidls { fn drop(&mut self) { for p in &self.0 { unsafe { CoTaskMemFree(Some(*p as *const c_void)) }; } } }

    pub fn icon_of_shell(p: &PathBuf) -> i32 {
        unsafe {
            let mut pidl = std::ptr::null_mut();
            if SHParseDisplayName(PCWSTR(wide(p).as_ptr()), None, &mut pidl, 0, None).is_err() { return 0; }
            let _owned = Pidls(vec![pidl]);
            let mut fi: SHFILEINFOW = std::mem::zeroed();
            SHGetFileInfoW(PCWSTR(pidl as *const ITEMIDLIST as *const u16), windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut fi), size_of::<SHFILEINFOW>() as u32, SHGFI_PIDL | SHGFI_SYSICONINDEX | SHGFI_SMALLICON);
            fi.iIcon
        }
    }

    /// A shell UI object (IContextMenu, IDataObject...) for items that all live in one folder.
    fn ui<T: Interface>(h: HWND, paths: &[PathBuf]) -> Result<T> {
        unsafe {
            let mut abs = Pidls(vec![]);
            for p in paths {
                let mut pidl = std::ptr::null_mut();
                SHParseDisplayName(PCWSTR(wide(p).as_ptr()), None, &mut pidl, 0, None)?;
                abs.0.push(pidl);
            }
            let parent: IShellFolder = SHBindToParent(abs.0[0], None)?;
            let kids: Vec<*const ITEMIDLIST> = abs.0.iter().map(|&p| ILFindLastID(p) as *const _).collect();
            parent.GetUIObjectOf(h, &kids, None)
        }
    }

    thread_local! { static CM2: RefCell<Option<IContextMenu2>> = const { RefCell::new(None) }; }

    /// Forwards owner-draw / init-popup menu messages so submenus like "Send to" get filled in.
    pub fn menu_msg(m: u32, wp: usize, lp: isize) -> bool {
        CM2.with_borrow(|c| c.as_ref().is_some_and(|c| unsafe { c.HandleMenuMsg(m, WPARAM(wp), LPARAM(lp)).is_ok() }))
    }

    /// The real shell context menu for `paths`, so 7-Zip/Git/etc. entries show up. `extra` = our own items on top
    /// (ids above the shell's range); returns the one picked, None when the shell handled the pick or nothing was picked.
    pub fn show(h: *mut c_void, paths: &[PathBuf], extra: &[(u32, &str)]) -> Result<Option<u32>> {
        unsafe {
            let hwnd = HWND(h);
            let cm: IContextMenu = ui(hwnd, paths)?;
            let menu = CreatePopupMenu()?;
            cm.QueryContextMenu(menu, 0, 1, 0x7FFF, CMF_NORMAL).ok()?;
            if !extra.is_empty() {
                let _ = InsertMenuW(menu, 0, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null());
                for (id, text) in extra.iter().rev() {
                    let t: Vec<u16> = text.encode_utf16().chain([0]).collect();
                    let _ = InsertMenuW(menu, 0, MF_BYPOSITION | MF_STRING, *id as usize, PCWSTR(t.as_ptr()));
                }
            }
            CM2.set(cm.cast::<IContextMenu2>().ok());
            let mut pt = POINT::default();
            GetCursorPos(&mut pt)?;
            let id = TrackPopupMenuEx(menu, TPM_RETURNCMD.0 | TPM_RIGHTBUTTON.0, pt.x, pt.y, hwnd, None).0;
            CM2.set(None);
            if extra.iter().any(|e| e.0 as i32 == id) {
                DestroyMenu(menu)?;
                return Ok(Some(id as u32));
            }
            if id > 0 {
                // ptInvoke: without it dialogs the shell opens ("Open with" > choose another app) land off screen
                let ci = CMINVOKECOMMANDINFOEX {
                    cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                    fMask: CMIC_MASK_PTINVOKE,
                    hwnd,
                    lpVerb: PCSTR((id - 1) as usize as *const u8),
                    nShow: SW_SHOWNORMAL.0,
                    ptInvoke: pt,
                    ..Default::default()
                };
                cm.InvokeCommand(&ci as *const _ as *const CMINVOKECOMMANDINFO)?;
            }
            DestroyMenu(menu)?;
            Ok(None)
        }
    }

    /// Starts an OLE drag of `paths` (blocks until dropped).
    pub fn drag(h: *mut c_void, paths: &[PathBuf]) -> Result<()> {
        unsafe {
            let hwnd = HWND(h);
            let data: IDataObject = ui(hwnd, paths)?;
            SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK)?;
            Ok(())
        }
    }

    fn hdrop_paths(o: &IDataObject) -> Result<Vec<PathBuf>> {
        unsafe {
            let fmt = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0 as u32, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
            let mut med = o.GetData(&fmt)?;
            let hd = HDROP(med.u.hGlobal.0);
            let mut out = vec![];
            for i in 0..DragQueryFileW(hd, u32::MAX, None) {
                let mut buf = vec![0u16; DragQueryFileW(hd, i, None) as usize + 1];
                let n = DragQueryFileW(hd, i, Some(&mut buf)) as usize;
                out.push(PathBuf::from(String::from_utf16_lossy(&buf[..n])));
            }
            ReleaseStgMedium(&mut med);
            Ok(out)
        }
    }

    #[implement(IDropTarget)]
    struct DropT { main: isize }

    fn effect(k: MODIFIERKEYS_FLAGS) -> DROPEFFECT { if k.0 & 8 != 0 { DROPEFFECT_COPY } else { DROPEFFECT_MOVE } } // 8 = MK_CONTROL

    pub fn folder_row(rows: &[super::Row], dir: &std::path::Path, i: isize) -> Option<(usize, PathBuf)> {
        let i = usize::try_from(i).ok()?;
        rows.get(i).filter(|r| r.dir).map(|r| (i, super::row_path(dir, &r.name)))
    }

    fn drop_target(_main: isize, pt: &POINTL) -> Option<(usize, PathBuf)> {
        let list = super::ST.with_borrow(|s| s.list);
        if list.is_null() { return None; }
        let mut p = SysPoint { x: pt.x, y: pt.y };
        unsafe { ScreenToClient(list, &mut p) };
        let mut ht: LVHITTESTINFO = unsafe { std::mem::zeroed() };
        ht.pt = p;
        let i = super::put(list, LVM_HITTEST, 0, &mut ht as *mut _ as isize);
        super::ST.with_borrow(|s| folder_row(&s.rows, &s.dir, i))
    }

    fn drop_highlight(main: isize, pt: &POINTL) {
        unsafe { super::ui::drop_row(drop_target(main, pt).map(|(i, _)| i as isize)) };
    }

    impl IDropTarget_Impl for DropT_Impl {
        fn DragEnter(&self, _o: Ref<'_, IDataObject>, k: MODIFIERKEYS_FLAGS, pt: &POINTL, e: *mut DROPEFFECT) -> Result<()> {
            drop_highlight(self.main, pt);
            unsafe { *e = effect(k) };
            Ok(())
        }
        fn DragOver(&self, k: MODIFIERKEYS_FLAGS, pt: &POINTL, e: *mut DROPEFFECT) -> Result<()> {
            drop_highlight(self.main, pt);
            unsafe { *e = effect(k) };
            Ok(())
        }
        fn DragLeave(&self) -> Result<()> { unsafe { super::ui::drop_row(None) }; Ok(()) }
        fn Drop(&self, o: Ref<'_, IDataObject>, k: MODIFIERKEYS_FLAGS, pt: &POINTL, e: *mut DROPEFFECT) -> Result<()> {
            unsafe { super::ui::drop_row(None) };
            if super::desk::dragging() {
                // desktop icons dropped back on the desktop: a move of the icons, not of the files (the dragger finishes it)
                super::desk::dropped_here((pt.x, pt.y));
                unsafe { *e = DROPEFFECT_NONE };
                return Ok(());
            }
            let paths = hdrop_paths(o.ok()?)?;
            let target = drop_target(self.main, pt);
            if target.as_ref().is_some_and(|(_, p)| paths.contains(p)) {
                unsafe { *e = DROPEFFECT_NONE };
                return Ok(());
            }
            let to = target.map(|(_, p)| p).unwrap_or_else(super::cur_dir);
            // like Explorer: same drive moves, other drive copies, Ctrl forces copy
            let copy = k.0 & 8 != 0 || paths.first().and_then(|p| p.components().next()) != to.components().next();
            unsafe { *e = if copy { DROPEFFECT_COPY } else { DROPEFFECT_MOVE } };
            let _ = fileop(self.main as *mut c_void, if copy { Op::Copy } else { Op::Move }, &paths, &to);
            unsafe { let _ = PostMessageW(Some(HWND(self.main as *mut c_void)), WM_APP, WPARAM(0), LPARAM(0)); }
            Ok(())
        }
    }

    pub fn register_drop(list: *mut c_void, main: isize) -> Result<()> {
        let t: IDropTarget = DropT { main }.into();
        unsafe { RegisterDragDrop(HWND(list), &t) }
    }

    #[implement(IDropTarget)]
    struct NavDropT { main: isize }

    impl IDropTarget_Impl for NavDropT_Impl {
        fn DragEnter(&self, _o: Ref<'_, IDataObject>, _k: MODIFIERKEYS_FLAGS, pt: &POINTL, e: *mut DROPEFFECT) -> Result<()> {
            let ok = super::ui::nav_drop_at(pt.y);
            if ok { super::ui::nav_drop_begin(); } else { super::ui::nav_drop_end(); }
            unsafe { *e = if ok { DROPEFFECT_LINK } else { DROPEFFECT_NONE } };
            Ok(())
        }
        fn DragOver(&self, _k: MODIFIERKEYS_FLAGS, pt: &POINTL, e: *mut DROPEFFECT) -> Result<()> {
            let ok = super::ui::nav_drop_at(pt.y);
            if ok { super::ui::nav_drop_begin(); } else { super::ui::nav_drop_end(); }
            unsafe { *e = if ok { DROPEFFECT_LINK } else { DROPEFFECT_NONE } };
            Ok(())
        }
        fn DragLeave(&self) -> Result<()> { super::ui::nav_drop_end(); Ok(()) }
        fn Drop(&self, o: Ref<'_, IDataObject>, _k: MODIFIERKEYS_FLAGS, _pt: &POINTL, e: *mut DROPEFFECT) -> Result<()> {
            super::ui::nav_drop_end();
            let paths = hdrop_paths(o.ok()?)?;
            let n = super::ui::nav_add(&paths);
            unsafe { *e = if n > 0 { DROPEFFECT_LINK } else { DROPEFFECT_NONE } };
            unsafe { let _ = PostMessageW(Some(HWND(self.main as *mut c_void)), WM_APP, WPARAM(0), LPARAM(0)); }
            Ok(())
        }
    }

    pub fn register_nav_drop(nav: *mut c_void, main: isize) -> Result<()> {
        let t: IDropTarget = NavDropT { main }.into();
        unsafe { RegisterDragDrop(HWND(nav), &t) }
    }
}


/// The desktop's own logic: icons stay where the user puts them (positions saved per name), dropping one onto a folder
/// icon moves it into the folder, and a host window for Wallpaper Engine (it draws behind the icons).
mod desk {
    #![allow(unsafe_op_in_unsafe_fn)]
    use super::*;
    use std::{cell::Cell, collections::{HashMap, HashSet}};
    use windows_sys::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, HDC, ScreenToClient};

    pub const SPAWN_WORKERW: u32 = 0x052C; // what wallpaper apps send to "Progman" to get a window behind the icons
    const KEY: u32 = 0x0001_0001; // rgb(1, 0, 1): the list's background colour, made see-through while a live wallpaper runs

    thread_local! {
        static DRAG_FROM: Cell<Option<(i32, i32)>> = const { Cell::new(None) }; // our own icon drag: where it started (list coords)
        static DROP_AT: Cell<Option<(i32, i32)>> = const { Cell::new(None) }; // ...and where it was dropped (screen coords)
        static WORKERW: Cell<isize> = const { Cell::new(0) };
        static LIVE: Cell<bool> = const { Cell::new(false) }; // a live wallpaper is running in WORKERW
    }

    fn pos_file() -> PathBuf {
        PathBuf::from(env::var_os("APPDATA").unwrap_or_default()).join(r"rust-explorer\desktop-icons.txt")
    }

    /// Saved icon positions: name -> (x, y) in list pixels.
    fn saved() -> HashMap<String, (i32, i32)> {
        fs::read_to_string(pos_file()).unwrap_or_default().lines().filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.to_string()))
        }).map(|(x, y, n): (i32, i32, String)| (n, (x, y))).collect()
    }

    fn names() -> Vec<String> { ST.with_borrow(|st| st.rows.iter().map(|r| r.name.clone()).collect()) }

    fn pos(list: HWND, i: usize) -> (i32, i32) {
        let mut p = POINT { x: 0, y: 0 };
        put(list, LVM_GETITEMPOSITION, i, &mut p as *mut _ as isize);
        (p.x, p.y)
    }

    fn set_pos(list: HWND, i: usize, (x, y): (i32, i32)) {
        let p = POINT { x, y };
        put(list, LVM_SETITEMPOSITION32, i, &p as *const _ as isize);
    }

    /// Writes every icon's position (after a move or a fresh layout).
    pub fn save(list: HWND) {
        let lines: String = names().iter().enumerate().map(|(i, n)| { let (x, y) = pos(list, i); format!("{x}\t{y}\t{n}\n") }).collect();
        let f = pos_file();
        let _ = fs::create_dir_all(f.parent().unwrap_or(&f));
        let _ = fs::write(f, lines);
    }

    /// The icon grid: cell size, and how many columns / rows fit.
    struct Grid { cx: i32, cy: i32, cols: i32, rows: i32 }

    impl Grid {
        fn of(list: HWND) -> Grid {
            let s = put(list, LVM_GETITEMSPACING, 0, 0) as u32;
            let (cx, cy) = (((s & 0xFFFF) as i32).max(1), ((s >> 16) as i32).max(1));
            let mut r: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetClientRect(list, &mut r) };
            if r.bottom <= 0 {
                // the first fill runs in WM_CREATE, before WM_SIZE gives the list its size: the size it is about to get
                r = unsafe { RECT { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) - desk_top() - desk_bottom() } };
            }
            Grid { cx, cy, cols: (r.right / cx).max(1), rows: (r.bottom / cy).max(1) }
        }
        /// Nearest cell to a position; None if it is off the grid (screen got smaller).
        fn cell(&self, (x, y): (i32, i32)) -> Option<(i32, i32)> {
            let c = ((x + self.cx / 2).div_euclid(self.cx), (y + self.cy / 2).div_euclid(self.cy));
            (c.0 >= 0 && c.1 >= 0 && c.0 < self.cols && c.1 < self.rows).then_some(c)
        }
        fn at(&self, (c, r): (i32, i32)) -> (i32, i32) { (c * self.cx, r * self.cy) }
        /// Free cell nearest to `want`; with no wish the first free one going down the columns, like Windows.
        fn free(&self, taken: &HashSet<(i32, i32)>, want: Option<(i32, i32)>) -> (i32, i32) {
            let all = (0..self.cols).flat_map(|c| (0..self.rows).map(move |r| (c, r))).filter(|c| !taken.contains(c));
            match want {
                Some((wc, wr)) => all.min_by_key(|&(c, r)| (c - wc).pow(2) + (r - wr).pow(2)),
                None => all.min(),
            }
            .unwrap_or((0, 0))
        }
    }

    /// Fills the (non-virtual) desktop list with `n` rows and puts every icon on its saved cell, new ones on free cells.
    pub fn fill(list: HWND, n: usize) {
        for i in 0..n {
            let mut it: LVITEMW = unsafe { std::mem::zeroed() };
            it.mask = LVIF_TEXT | LVIF_IMAGE;
            it.iItem = i as i32;
            it.pszText = usize::MAX as *mut u16; // LPSTR_TEXTCALLBACKW: text and icon come from LVN_GETDISPINFO
            it.iImage = -1; // I_IMAGECALLBACK
            put(list, LVM_INSERTITEMW, 0, &it as *const _ as isize);
        }
        let (g, old, names) = (Grid::of(list), saved(), names());
        let mut taken = HashSet::new();
        let mut later = vec![];
        for (i, name) in names.iter().enumerate() {
            match old.get(name).and_then(|&p| g.cell(p)).filter(|c| !taken.contains(c)) {
                Some(c) => { taken.insert(c); set_pos(list, i, g.at(c)); }
                None => later.push(i),
            }
        }
        for i in later {
            let c = g.free(&taken, None);
            taken.insert(c);
            set_pos(list, i, g.at(c));
        }
        save(list);
    }

    // ---------- moving icons ----------

    /// Our own drag of desktop icons starts (`from` in list coordinates).
    pub fn drag_start(from: (i32, i32)) { DRAG_FROM.set(Some(from)); DROP_AT.set(None); }
    pub fn dragging() -> bool { DRAG_FROM.get().is_some() }
    /// Called by the drop target: the drag ended on the desktop itself at screen point `at`.
    pub fn dropped_here(at: (i32, i32)) { DROP_AT.set(Some(at)); }

    /// After the drag: on a folder icon (not one of the dragged) the selection moves into that folder; anywhere else the
    /// icons move by the drag distance, snapped to the grid, each to the nearest free cell. true = handled here.
    pub fn drag_end(h: HWND, list: HWND) -> bool {
        let Some(from) = DRAG_FROM.take() else { return false };
        // normally our drop target recorded where it ended; over a live wallpaper OLE never reaches it (the wallpaper window of
        // another process is under the pointer), so then: the pointer is over our desktop window = the drop was on the desktop
        let at = DROP_AT.take().or_else(|| unsafe {
            let mut c = POINT { x: 0, y: 0 };
            GetCursorPos(&mut c);
            (GetAncestor(WindowFromPoint(c), GA_ROOT) == h).then_some((c.x, c.y))
        });
        let Some(at) = at else { return false };
        let mut pt = POINT { x: at.0, y: at.1 };
        unsafe { ScreenToClient(list, &mut pt) };
        let (_, dir, paths) = selected();
        let mut sel = vec![];
        let mut i = -1isize;
        loop {
            i = put(list, LVM_GETNEXTITEM, i as usize, LVNI_SELECTED as isize);
            if i < 0 { break; }
            sel.push(i as usize);
        }
        // onto a folder icon
        let mut ht: LVHITTESTINFO = unsafe { std::mem::zeroed() };
        ht.pt = pt;
        let tgt = put(list, LVM_HITTEST, 0, &mut ht as *mut _ as isize);
        if tgt >= 0 && !sel.contains(&(tgt as usize)) {
            let target = ST.with_borrow(|st| st.rows.get(tgt as usize).filter(|r| r.dir).map(|r| row_path(&st.dir, &r.name)));
            if let Some(t) = target {
                let _ = sh::fileop(h as _, sh::Op::Move, &paths, &t);
                let _ = dir;
                unsafe { PostMessageW(h, WM_APP, 0, 0) }; // reload: the moved icons are gone from here
                return true;
            }
        }
        let g = Grid::of(list);
        let n = put(list, LVM_GETITEMCOUNT, 0, 0).max(0) as usize;
        let mut taken: HashSet<(i32, i32)> = (0..n).filter(|i| !sel.contains(i)).filter_map(|i| g.cell(pos(list, i))).collect();
        let (dx, dy) = (pt.x - from.0, pt.y - from.1);
        for &i in &sel {
            let (x, y) = pos(list, i);
            let want = g.cell((x + dx, y + dy)).unwrap_or_else(|| {
                // dragged off the grid: the nearest edge cell
                (((x + dx) / g.cx).clamp(0, g.cols - 1), ((y + dy) / g.cy).clamp(0, g.rows - 1))
            });
            let c = if taken.contains(&want) { g.free(&taken, Some(want)) } else { want };
            taken.insert(c);
            set_pos(list, i, g.at(c));
        }
        save(list);
        true
    }

    // ---------- Wallpaper Engine (and other live wallpapers) ----------
    //
    // They look for the "Progman" window (that is our desktop's class), send it 0x052C and put their window into the
    // "WorkerW" that appears: Windows 11 24H2 has it as a child of Progman, under the icons. Ours is the same, and while it
    // holds a wallpaper the icon list turns see-through (colour key) so the wallpaper shows between the icons.

    pub unsafe fn spawn_workerw(h: HWND) {
        if WORKERW.get() != 0 && IsWindow(WORKERW.get() as HWND) != 0 { return; }
        let mut r: RECT = std::mem::zeroed();
        GetClientRect(h, &mut r);
        let ww = CreateWindowExW(0, w("WorkerW").as_ptr(), ptr::null(), WS_CHILD | WS_VISIBLE, 0, 0, r.right, r.bottom, h, 0 as _, GetModuleHandleW(ptr::null()), ptr::null());
        SetWindowPos(ww, HWND_BOTTOM, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE); // under the icon list
        WORKERW.set(ww as isize);
    }

    /// Every couple of seconds: a wallpaper window inside WorkerW = live mode on, none = back to the picture.
    pub unsafe fn check_live(list: HWND) {
        let ww = WORKERW.get() as HWND;
        let live = !ww.is_null() && IsWindow(ww) != 0 && !GetWindow(ww, GW_CHILD).is_null();
        if live == LIVE.replace(live) { return; }
        // the list stays "transparent" (its background is what the desktop paints: the key colour while live, see erase()),
        // so it keeps drawing the label shadows; the colour key then cuts the background out
        let ex = GetWindowLongPtrW(list, GWL_EXSTYLE);
        if live {
            SetWindowLongPtrW(list, GWL_EXSTYLE, ex | WS_EX_LAYERED as isize);
            SetLayeredWindowAttributes(list, KEY, 0, LWA_COLORKEY);
        } else {
            SetWindowLongPtrW(list, GWL_EXSTYLE, ex & !(WS_EX_LAYERED as isize));
        }
        InvalidateRect(list, ptr::null(), 1);
    }

    /// The desktop's background: the key colour while a live wallpaper shows through, else the picture. false = none to paint.
    pub unsafe fn erase(dc: HDC, h: HWND) -> bool {
        let (bmp, cw, ch) = WALL.get();
        if LIVE.get() {
            let mut r: RECT = std::mem::zeroed();
            GetClientRect(h, &mut r);
            let b = CreateSolidBrush(KEY);
            FillRect(dc, &r, b);
            DeleteObject(b);
        } else if bmp != 0 {
            wall::paint(dc, bmp, cw, ch);
        } else {
            return false;
        }
        true
    }

    /// WorkerW without a wallpaper in it shows the picture wallpaper, like the desktop behind it.
    pub unsafe extern "system" fn workerw_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if m == WM_ERASEBKGND {
            let (bmp, cw, ch) = WALL.get();
            if bmp != 0 { wall::paint(wp as _, bmp, cw, ch); } else { let mut r: RECT = std::mem::zeroed(); GetClientRect(h, &mut r); FillRect(wp as _, &r, GetStockObject(BLACK_BRUSH) as _); }
            return 1;
        }
        DefWindowProcW(h, m, wp, lp)
    }

    #[cfg(test)]
    mod tests {
        use super::Grid;
        use std::collections::HashSet;

        #[test]
        fn grid_cells() {
            let g = Grid { cx: 100, cy: 90, cols: 5, rows: 4 };
            assert_eq!(g.cell((0, 0)), Some((0, 0)));
            assert_eq!(g.cell((149, 44)), Some((1, 0))); // rounds to the nearest cell
            assert_eq!(g.cell((900, 0)), None); // off the grid
            let mut taken: HashSet<_> = [(0, 0), (0, 1)].into();
            assert_eq!(g.free(&taken, None), (0, 2)); // down the first column first
            taken.insert((2, 2));
            assert_eq!(g.free(&taken, Some((2, 2))), (1, 2)); // nearest free to the wish (ties: lowest)
        }
    }
}

/// Trace of the icon drags (%TEMP%\rx-drag.log), to find out where one stops working. Cheap: a few lines per drag.
fn drag_log(s: &str) {
    use std::io::Write;
    let p = std::env::temp_dir().join("rx-drag.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(p) { let _ = writeln!(f, "{s}"); }
}

/// Address bar subclass: Enter / Esc go to the main window, everything else is the normal edit control.
unsafe extern "system" fn edit_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if matches!(wp, 13 | 27) && matches!(m, WM_KEYDOWN | WM_CHAR) {
            if m == WM_KEYDOWN { PostMessageW(GetAncestor(h, GA_ROOT), WM_GO, wp, 0); }
            return 0; // WM_CHAR swallowed so there is no beep
        }
        CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(EDIT_OLD.load(std::sync::atomic::Ordering::Relaxed)), h, m, wp, lp)
    }
}

unsafe extern "system" fn proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if m == winrust_common::style::msg() {
            // the topbar switched the WinRust style: take it now (the desktop's list keeps its see-through colours)
            winrust_common::style::reload();
            if desk() { InvalidateRect(h, ptr::null(), 1); } else { ui::restyle(h); }
            return 0;
        }
        match m {
            WM_CREATE => {
                let inst = GetModuleHandleW(ptr::null());
                // desktop mode has no tab bar / address bar: the null handles are harmless to every SendMessage below
                let (mut tab, mut edit) = (0 as HWND, 0 as HWND);
                if !desk() {
                    sh::watch_devices(h);
                    ui::dark_title(h);
                    // the chrome (tabs, toolbar, address pill) is one painted window; the address box is a real edit inside its pill
                    tab = CreateWindowExW(0, w("RxChrome").as_ptr(), ptr::null(), WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN, 0, 0, 0, 0, h, 0 as _, inst, ptr::null());
                    let nav = CreateWindowExW(0, w("RxNav").as_ptr(), ptr::null(), WS_CHILD | WS_VISIBLE, 0, 0, 0, 0, h, 0 as _, inst, ptr::null());
                    ui::nav_init(nav);
                    let _ = sh::register_nav_drop(nav as _, h as isize);
                    edit = CreateWindowExW(0, w("EDIT").as_ptr(), ptr::null(), WS_CHILD | WS_VISIBLE | ES_AUTOHSCROLL as u32, 0, 0, 0, 0, tab, 0 as _, inst, ptr::null());
                    put(edit, WM_SETFONT, ui::font(14, 400, ui::ui_face()) as usize, 1);
                    EDIT_OLD.store(SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize), std::sync::atomic::Ordering::Relaxed);
                }
                let view = if desk() { LVS_ICON } else { LVS_REPORT };
                let list = CreateWindowExW(0, w("SysListView32").as_ptr(), ptr::null(), WS_CHILD | WS_VISIBLE | view | if desk() { 0 } else { LVS_OWNERDATA } | LVS_EDITLABELS | LVS_SHOWSELALWAYS | LVS_SHAREIMAGELISTS, 0, 0, 0, 0, h, 0 as _, inst, ptr::null());
                put(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize);
                let mut fi: SHFILEINFOW = std::mem::zeroed();
                let il = SHGetFileInfoW(w("C:\\").as_ptr(), 0, &mut fi, size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX | SHGFI_SMALLICON);
                put(list, LVM_SETIMAGELIST, LVSIL_SMALL as usize, il as isize);
                if desk() {
                    // system large icons; indexes are shared with the small list, so icon_of() works unchanged
                    let big = SHGetFileInfoW(w("C:\\").as_ptr(), 0, &mut fi, size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX | SHGFI_LARGEICON);
                    put(list, LVM_SETIMAGELIST, LVSIL_NORMAL as usize, big as isize);
                    put(list, LVM_SETICONSPACING, 0, ((ui::sc(96) as isize) << 16) | ui::sc(88) as isize);
                    // transparent background (the desktop paints it) and shadowed labels, readable on light wallpapers too
                    let ex = LVS_EX_TRANSPARENTBKGND | 0x0080_0000; // LVS_EX_TRANSPARENTSHADOWTEXT
                    put(list, LVM_SETEXTENDEDLISTVIEWSTYLE, ex as usize, ex as isize);
                    for msg in [LVM_SETBKCOLOR, LVM_SETTEXTBKCOLOR] { put(list, msg, 0, CLR_NONE); }
                    put(list, LVM_SETTEXTCOLOR, 0, 0x00FF_FFFF);
                    let (cw, ch) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
                    WALL.set((wall::load(cw, ch), cw, ch));
                    // grey (not ClearType) smoothing: on a live wallpaper the labels sit on a colour key, where ClearType leaves coloured fringes
                    put(list, WM_SETFONT, windows_sys::Win32::Graphics::Gdi::CreateFontW(-ui::sc(13), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 4, 0, w(ui::ui_face()).as_ptr()) as usize, 1);
                    // like explorer's desktop: wallpaper apps look for a SHELLDLL_DefView under Progman
                    CreateWindowExW(0, w("SHELLDLL_DefView").as_ptr(), ptr::null(), WS_CHILD, 0, 0, 0, 0, h, 0 as _, inst, ptr::null());
                    SetTimer(h, 7, 2000, None); // live wallpaper on / off (desk::check_live)
                } else {
                    let himl = ImageList_Create(THUMB, THUMB, ILC_COLOR32, 64, 64);
                    put(list, LVM_SETIMAGELIST, LVSIL_NORMAL as usize, himl as isize);
                    put(list, LVM_SETICONSPACING, 0, ((THUMB as isize + 34) << 16) | (THUMB as isize + 24));
                    let _ = TX.set(sh::start_thumbs(h as isize));
                    ST.with_borrow_mut(|st| st.himl = himl as isize);
                    // the same columns, in the same order, as Explorer's details view
                    col(list, 0, "Name", ui::sc(360), false); col(list, 1, "Date modified", ui::sc(160), false); col(list, 2, "Type", ui::sc(140), false); col(list, 3, "Size", ui::sc(96), true);
                    ui::style_list(list);
                }
                ST.with_borrow_mut(|st| { st.list = list; st.tab = tab; st.edit = edit; st.tabs = vec![PathBuf::new()]; });
                if !desk() { ui::place_edit(tab); cover::create(h, inst); }
                let _ = sh::register_drop(list as _, h as isize);
                // live wallpaper: the list is see-through (colour key), so over empty desktop OLE finds the wallpaper window under it,
                // and walks up from there to the desktop window: it must accept the drop too
                if desk() { let _ = sh::register_drop(h as _, h as isize); }
                SetTimer(h, 8, 900, None); // watch the visible folder for new files
                let start = if desk() { sh::desktop_dirs().into_iter().next().unwrap_or_default() } else { env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\")) };
                load(h, start);
                0
            }
            // the bar came, went or changed size (it sets the work area), or the screen changed: the icon list follows
            WM_SETTINGCHANGE | WM_DISPLAYCHANGE if desk() => {
                if m == WM_DISPLAYCHANGE {
                    SetWindowPos(h, HWND_BOTTOM, 0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN), SWP_NOACTIVATE); // WM_SIZE lays out
                } else if wp == SPI_SETWORKAREA as usize {
                    let (list, mut r) = (ST.with_borrow(|st| st.list), std::mem::zeroed::<RECT>());
                    GetClientRect(h, &mut r);
                    MoveWindow(list, 0, desk_top(), r.right, (r.bottom - desk_top() - desk_bottom()).max(0), 1);
                }
                0
            }
            WM_SIZE => {
                let list = ST.with_borrow(|st| st.list);
                let (cw, ch) = ((lp & 0xFFFF) as i32, ((lp >> 16) & 0xFFFF) as i32);
                if desk() {
                    MoveWindow(list, 0, desk_top(), cw, (ch - desk_top() - desk_bottom()).max(0), 1);
                } else {
                    cover::layout(cw, ch);
                    ui::layout_children(cw, ch);
                    InvalidateRect(h, ptr::null(), 0);
                }
                0
            }
            WM_PAINT if !desk() => { ui::paint_status(h); 0 }
            ui::WM_UI => {
                match wp {
                    1 => { let n = ST.with_borrow(|st| st.tabs.len()); if (lp as usize) < n { tab_select(h, lp as usize); } }
                    2 => tab_close_at(h, lp as usize),
                    3 => tab_new(),
                    4 => nav(h, true),
                    5 => nav(h, false),
                    6 => { if let Some(p) = up_from(&cur_dir()) { load(h, p) } }
                    7 => reload(h),
                    8 => toggle_view(h),
                    9 => search_focus(),
                    20 => { if let Some(p) = ui::nav_path(lp as usize) { load(h, p); } } // a folder in the left pane
                    21 => { if let Some(p) = ui::crumb_path(lp as usize) { load(h, p); } } // a part of the path in the address pill
                    _ => {}
                }
                0
            }
            WM_ERASEBKGND if desk() && desk::erase(wp as _, h) => 1,
            WM_WINDOWPOSCHANGING if desk() => { // the desktop never rises above other windows
                let wp = &mut *(lp as *mut WINDOWPOS);
                wp.hwndInsertAfter = HWND_BOTTOM;
                wp.flags |= SWP_NOACTIVATE;
                0
            }
            WM_INITMENUPOPUP if sh::menu_msg(m, wp, lp) => 0,
            WM_DRAWITEM | WM_MEASUREITEM if sh::menu_msg(m, wp, lp) => 1,
            WM_NOTIFY => {
                let nm = &*(lp as *const NMHDR);
                let list = ST.with_borrow(|st| st.list);
                match nm.code {
                    NM_CUSTOMDRAW if nm.hwndFrom == list && !desk() => return ui::list_draw(lp), // the rows are painted by hand
                    LVN_ITEMCHANGED | LVN_ODSTATECHANGED => {
                        ui::status_dirty(h); // selection count in the status line
                        if cover::is_open() {
                            let (_, _, sel) = selected();
                            if let Some(p) = sel.first() { cover::select(p); }
                        }
                    }
                    LVN_GETDISPINFOW => {
                        let it = &mut (*(lp as *mut NMLVDISPINFOW)).item;
                        // a request that arrives while ST is borrowed is skipped; the list repaints later
                        ST.with(|c| {
                            let Ok(mut st) = c.try_borrow_mut() else { return };
                            let State { dir, rows, thumbs, .. } = &mut *st;
                            let Some(i) = usize::try_from(it.iItem).ok() else { return };
                            let Some(r) = rows.get_mut(i) else { return };
                            if it.mask & LVIF_TEXT != 0 && it.cchTextMax > 0 {
                                let s = match it.iSubItem { 0 => shown_name(&r.title, desk()), 1 => date_text(r.time), 2 => type_text(&r.name, r.dir), _ if r.dir => String::new(), _ => size_text(r.size) };
                                let u: Vec<u16> = s.encode_utf16().take(it.cchTextMax as usize - 1).collect();
                                ptr::copy_nonoverlapping(u.as_ptr(), it.pszText, u.len());
                                *it.pszText.add(u.len()) = 0;
                            }
                            if it.mask & LVIF_IMAGE != 0 && *thumbs {
                                if r.thumb == -2 {
                                    r.thumb = -1;
                                    if let Some(tx) = TX.get() { let _ = tx.send((GEN.load(std::sync::atomic::Ordering::Relaxed), i, row_path(dir, &r.name))); }
                                }
                                it.iImage = if r.thumb >= 0 { r.thumb } else { I_IMAGENONE };
                            } else if it.mask & LVIF_IMAGE != 0 {
                                if r.icon < 0 { r.icon = icon_of(&row_path(dir, &r.name), r.dir); }
                                it.iImage = r.icon;
                            }
                        });
                    }
                    LVN_ITEMACTIVATE => {
                        let i = (*(lp as *const NMITEMACTIVATE)).iItem;
                        let hit = ST.with_borrow(|st| usize::try_from(i).ok().and_then(|i| st.rows.get(i)).map(|r| (row_path(&st.dir, &r.name), r.dir)));
                        if let Some((p, is_dir)) = hit {
                            if is_dir && desk() { let _ = std::process::Command::new(env::current_exe().unwrap_or_default()).arg(p).spawn(); }
                            else if is_dir { load(h, p) }
                            else if sh::is_shell(&p) { let _ = sh::open(&p); }
                            else if let Some(exe) = tool("photoview").filter(|_| is_pic(&p)) { let _ = std::process::Command::new(exe).arg(&p).spawn(); }
                            else { ShellExecuteW(0 as _, ptr::null(), w(&p.to_string_lossy()).as_ptr(), ptr::null(), ptr::null(), SW_SHOWNORMAL); }
                        }
                    }
                    NM_RCLICK if nm.hwndFrom == list => {
                        let (_, dir, paths) = selected();
                        if !paths.is_empty() { item_menu(h, &dir, &paths); } else { bg_menu(h); }
                    }
                    LVN_COLUMNCLICK => {
                        let c = (*(lp as *const NMLISTVIEW)).iSubItem as usize;
                        let asc = if SORT_COL.load(Relaxed) == c { !SORT_ASC.load(Relaxed) } else { true };
                        SORT_COL.store(c, Relaxed);
                        SORT_ASC.store(asc, Relaxed);
                        let n = ST.with_borrow_mut(|st| {
                            sort_rows(&mut st.rows, c, asc);
                            st.rows.iter_mut().for_each(|r| r.thumb = -2); // rows moved: thumbnails are asked for again
                            st.rows.len()
                        });
                        reset_thumbs();
                        put(list, LVM_REDRAWITEMS, 0, n as isize - 1);
                    }
                    LVN_BEGINDRAG => {
                        let (_, _, paths) = selected();
                        drag_log(&format!("BEGINDRAG desk={} selected={}", desk(), paths.len()));
                        if !paths.is_empty() {
                            if desk() { let p = (*(lp as *const NMLISTVIEW)).ptAction; desk::drag_start((p.x, p.y)); }
                            BUSY.store(true, Relaxed);
                            let r = sh::drag(h, &paths);
                            BUSY.store(false, Relaxed);
                            drag_log(&format!("drag finished: {r:?}"));
                            // dropped on the desktop itself: the icons were moved (or went into a folder icon), no reload needed
                            let handled = desk() && desk::drag_end(h, list);
                            drag_log(&format!("moved on the desktop: {handled}"));
                            if !handled { reload(h); }
                        }
                    }
                    LVN_KEYDOWN => {
                        let vk = (*(lp as *const NMLVKEYDOWN)).wVKey as u32;
                        let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
                        let shift = GetKeyState(VK_SHIFT as i32) < 0;
                        if desk() && matches!(vk, 0x08 | 0x46 | 0x4D | 0x54 | 0x57 | 0x09 | 0x4C | 0x73) { return 0; } // browsing keys make no sense on the desktop
                        let (list, dir, paths) = selected();
                        match vk {
                            0x08 => if let Some(p) = up_from(&dir) { load(h, p) },
                            0x2E if !paths.is_empty() => { let _ = sh::fileop(h, sh::Op::Delete, &paths, &dir); }
                            // Space on a picture: a quick look in photoview (pictures only: videos and text are not previewed yet)
                            0x20 if !desk() => if let (Some(p), Some(exe)) = (paths.first().filter(|p| is_pic(p)), tool("photoview")) { let _ = std::process::Command::new(exe).arg(p).spawn(); },
                            0x71 => {
                                // F2: edit the label in place; reloading here would cancel the edit box
                                let i = put(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
                                let name = usize::try_from(i).ok().and_then(|i| ST.with_borrow(|st| st.rows.get(i).map(|r| r.title.clone()))).unwrap_or_default();
                                label_edit(list, i as i32, &name);
                                return 0;
                            }
                            0x46 if ctrl && shift => {
                                let (_, dir, sel) = selected();
                                let rows = ST.with_borrow(|st| st.rows.clone());
                                 if let Some((paths, i)) = cover::from_rows(&dir, &rows, sel.first(), false) { cover::open(h, paths, i); }
                                return 0;
                            }
                            0x46 if ctrl => { search_focus(); return 0; } // Ctrl+F: address bar in search mode
                            0x73 | 0x4C if vk == 0x73 || ctrl => { // F4 / Ctrl+L: address bar
                                let edit = ST.with_borrow(|st| st.edit);
                                SetFocus(edit);
                                put(edit, EM_SETSEL, 0, -1);
                                return 0;
                            }
                            0x5D if !paths.is_empty() => item_menu(h, &dir, &paths),
                            0x43 | 0x58 if ctrl && !paths.is_empty() => { let _ = sh::clip_set(h, &paths, vk == 0x58); return 0; }
                            0x41 if ctrl => { // Ctrl+A: select all (state mask LVIS_SELECTED, item -1 = every row)
                                let mut it: LVITEMW = std::mem::zeroed();
                                it.stateMask = LVIS_SELECTED;
                                it.state = LVIS_SELECTED;
                                put(list, LVM_SETITEMSTATE, usize::MAX, &it as *const _ as isize);
                                return 0;
                            }
                            0x56 if ctrl => paste(h, &dir),
                            0x4E if ctrl => new_entry(&dir, "New folder", true),
                            0x48 if ctrl => { SHOW_HIDDEN.fetch_xor(true, Relaxed); } // Ctrl+H: hidden files
                            0x74 => {} // F5: just the reload below
                            0x4D if ctrl => { toggle_view(h); return 0; } // Ctrl+M: details <-> thumbnails
                            0x54 if ctrl => tab_new(),
                            0x57 if ctrl => { tab_close(h); return 0; }
                            0x09 if ctrl => {
                                let next = ST.with_borrow(|st| (st.active + 1) % st.tabs.len());
                                tab_select(h, next);
                                return 0;
                            }
                            _ => return 0,
                        }
                        reload(h);
                    }
                    LVN_ENDLABELEDITW => {
                        let it = &(*(lp as *const NMLVDISPINFOW)).item;
                        if !it.pszText.is_null() {
                            let new = wstr(it.pszText);
                            // reject separators so a rename can never move the item
                            if !new.is_empty() && !new.contains(['\\', '/', ':']) {
                                ST.with_borrow(|st| if let Some(r) = usize::try_from(it.iItem).ok().and_then(|i| st.rows.get(i)) {
                                    let p = row_path(&st.dir, &r.name);
                                    if sh::is_shell(&p) { let _ = sh::rename(&p, &new); } else { let _ = fs::rename(p, st.dir.join(&new)); }
                                });
                                PostMessageW(h, WM_APP, 0, 0); // reload after the edit control is gone
                            }
                        }
                    }
                    _ => {}
                }
                0
            }
            WM_SETFOCUS => { SetFocus(ST.with_borrow(|st| st.list)); 0 }
            WM_DEVICE_CHANGE => {
                if let Some(mask) = volume_mask(m, wp, lp) {
                    if !desk() { ui::nav_refresh(); }
                    if path_on_mask(&cur_dir(), mask) { reload(h); }
                } else if device_interface_changed(m, wp, lp) && !desk() {
                    ui::nav_refresh();
                    if sh::is_shell(&cur_dir()) { reload(h); }
                }
                DefWindowProcW(h, m, wp, lp)
            }
            WM_APP => { reload(h); 0 }
            WM_JOB => {
                let err = *Box::from_raw(wp as *mut Option<String>);
                reload(h);
                ui::status_dirty(h);
                if let Some(e) = err { MessageBoxW(h, w(&e).as_ptr(), w("WinRust Explorer").as_ptr(), MB_ICONWARNING); }
                0
            }
            WM_NAV => {
                match wp {
                    1 => nav(h, true),
                    2 => nav(h, false),
                    3 => { let (_, dir, sel) = selected(); properties(&sel.first().cloned().unwrap_or(dir)); }
                    _ => if let Some(p) = up_from(&cur_dir()) { load(h, p) },
                }
                0
            }
            WM_THUMB => {
                let (era, row, bmp) = *Box::from_raw(wp as *mut (u32, usize, isize));
                let (list, himl) = ST.with_borrow(|st| (st.list, st.himl));
                if era == GEN.load(std::sync::atomic::Ordering::Relaxed) {
                    let idx = ImageList_Add(himl as _, bmp as _, ptr::null_mut());
                    let ok = idx >= 0 && ST.with_borrow_mut(|st| st.rows.get_mut(row).map(|r| r.thumb = idx).is_some());
                    if ok { put(list, LVM_REDRAWITEMS, row, row as isize); }
                }
                windows_sys::Win32::Graphics::Gdi::DeleteObject(bmp as _);
                0
            }
            WM_COVER => {
                let (era, i, bmp) = *Box::from_raw(wp as *mut (u32, usize, isize));
                cover::thumb(era, i, bmp);
                0
            }
            WM_GO => {
                let (edit, list) = ST.with_borrow(|st| (st.edit, st.list));
                if wp == 13 {
                    let mut buf = vec![0u16; GetWindowTextLengthW(edit) as usize + 1];
                    let n = GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32) as usize;
                    let p = PathBuf::from(String::from_utf16_lossy(&buf[..n]).trim().trim_matches('"'));
                    let text = String::from_utf16_lossy(&buf[..n]);
                    if let Some(q) = text.trim().strip_prefix('?') {
                        if !q.trim().is_empty() { search(h, q.trim()); SetFocus(list); return 0; } // keep the query in the bar
                    } else if p.is_dir() { load(h, p); }
                }
                SetWindowTextW(edit, w(&cur_dir().to_string_lossy()).as_ptr()); // also undoes a bad path / Esc
                SetFocus(list);
                0
            }
            desk::SPAWN_WORKERW if desk() => { desk::spawn_workerw(h); 0 }
            WM_TIMER if wp == 8 => { auto_reload(h); 0 }
            WM_TIMER if desk() && wp == 7 => { desk::check_live(ST.with_borrow(|st| st.list)); 0 }
            WM_DESTROY => { sh::unwatch_devices(); PostQuitMessage(0); 0 }
            _ => DefWindowProcW(h, m, wp, lp),
        }
    }
}

fn main() {
    sh::init();
    unsafe {
        SetProcessDPIAware(); // sharp text at any scaling; every size below goes through ui::sc
        ui::init_scale();
        ui::gdiplus_start();
        ui::dark_mode();
        InitCommonControlsEx(&INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_LISTVIEW_CLASSES });
        let inst = GetModuleHandleW(ptr::null());
        let cls = w("RustExplorer");
        let desktop = env::args().nth(1).as_deref() == Some("--desktop");
        // desktop without a wallpaper file shows black, not window white
        let bg = if desktop { GetStockObject(BLACK_BRUSH) as _ } else { windows_sys::Win32::Graphics::Gdi::CreateSolidBrush(ui::pal().bg) };
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(ui::proc), hInstance: inst, lpszClassName: w("RxChrome").as_ptr(), hCursor: LoadCursorW(0 as _, IDC_ARROW), ..std::mem::zeroed() });
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(ui::nav_proc), hInstance: inst, lpszClassName: w("RxNav").as_ptr(), hCursor: LoadCursorW(0 as _, IDC_ARROW), ..std::mem::zeroed() });
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(cover::proc), hInstance: inst, lpszClassName: w(cover::CLASS).as_ptr(), hCursor: LoadCursorW(0 as _, IDC_ARROW), ..std::mem::zeroed() });
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: cls.as_ptr(), hCursor: LoadCursorW(0 as _, IDC_ARROW), hIcon: LoadIconW(inst, 1 as _), hbrBackground: bg, ..std::mem::zeroed() });
        if desktop {
            DESKTOP.store(true, Relaxed);
            // the whole screen: it paints the wallpaper too; the icon list sits above the bar (desk_top / desk_bottom)
            // class "Progman" like explorer's desktop, so Wallpaper Engine & co. find it; title "rs-desktop" tells it apart
            // from explorer's ("Program Manager") for rs-shell
            let pm = w("Progman");
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: pm.as_ptr(), hCursor: LoadCursorW(0 as _, IDC_ARROW), hIcon: LoadIconW(inst, 1 as _), hbrBackground: bg, ..std::mem::zeroed() });
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(desk::workerw_proc), hInstance: inst, lpszClassName: w("WorkerW").as_ptr(), ..std::mem::zeroed() });
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(DefWindowProcW), hInstance: inst, lpszClassName: w("SHELLDLL_DefView").as_ptr(), ..std::mem::zeroed() });
            let d = CreateWindowExW(WS_EX_TOOLWINDOW, pm.as_ptr(), w("rs-desktop").as_ptr(), WS_POPUP, 0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN), 0 as _, 0 as _, inst, ptr::null());
            SetWindowPos(d, HWND_BOTTOM, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            CreateWindowExW(0, cls.as_ptr(), ptr::null(), WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, ui::sc(1000), ui::sc(660), 0 as _, 0 as _, inst, ptr::null());
        }
        let mut msg = std::mem::zeroed();
        while GetMessageW(&mut msg, 0 as _, 0, 0) > 0 {
            // Alt+Left / Right / Up / Enter arrive as WM_SYSKEYDOWN at the list, which never reports them: handle them here
            if msg.message == WM_SYSKEYDOWN && !desk() {
                let act = match msg.wParam { 0x25 => 1, 0x27 => 2, 0x0D => 3, 0x26 => 4, _ => 0 };
                if act != 0 { PostMessageW(GetAncestor(msg.hwnd, GA_ROOT), WM_NAV, act, 0); continue; }
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, dir: bool, size: u64, time: u64) -> Row { Row { name: name.into(), title: name.into(), dir, size, time, icon: -1, thumb: -2 } }
    fn names(v: &[Row]) -> Vec<&str> { v.iter().map(|r| r.name.as_str()).collect() }

    #[test]
    fn desktop_rows_show_file_names_without_shortcut_extensions() {
        assert_eq!(shown_name(r"C:\Users\Public\Desktop\Steam.lnk", true), "Steam");
        assert_eq!(shown_name("Notes.txt", true), "Notes.txt");
        assert_eq!(shown_name(r"C:\x\Game.URL", true), "Game");
        assert_eq!(shown_name(r"C:\x\Steam.lnk", false), r"C:\x\Steam.lnk");
    }

    #[test]
    fn watch_signature_changes_when_a_file_appears() {
        let a = [r("a.txt", false, 1, 1)];
        let b = [r("a.txt", false, 1, 1), r("b.png", false, 2, 2)];
        assert_ne!(watch_signature(&a), watch_signature(&b));
        assert_eq!(watch_signature(&b), watch_signature(&b));
    }

    #[test]
    fn drop_target_uses_only_folder_rows() {
        let dir = PathBuf::from("C:\\base");
        let rows = [r("a.txt", false, 1, 1), r("sub", true, 0, 2)];
        assert_eq!(sh::folder_row(&rows, &dir, 1), Some((1, dir.join("sub"))));
        assert_eq!(sh::folder_row(&rows, &dir, 0), None);
        assert_eq!(sh::folder_row(&rows, &dir, -1), None);
    }

    #[test]
    fn folders_first_then_by_the_chosen_column() {
        let mut v = vec![r("b.txt", false, 5, 1), r("A", true, 0, 2), r("a.txt", false, 9, 3)];
        sort_rows(&mut v, 0, true);
        assert_eq!(names(&v), ["A", "a.txt", "b.txt"]);
        sort_rows(&mut v, 3, false); // biggest first, the folder still on top
        assert_eq!(names(&v), ["A", "a.txt", "b.txt"]);
        sort_rows(&mut v, 1, true); // oldest first
        assert_eq!(names(&v), ["A", "b.txt", "a.txt"]);
    }

    #[test]
    fn search_hit_paths_sort_by_file_name() {
        let mut v = vec![r("C:\\z\\b.txt", false, 1, 1), r("D:\\a\\a.txt", false, 1, 1)];
        sort_rows(&mut v, 0, true);
        assert_eq!(names(&v), ["D:\\a\\a.txt", "C:\\z\\b.txt"]);
    }

    #[test]
    fn history_goes_back_and_forward_and_a_new_visit_drops_the_forward_trail() {
        let mut h = Hist::default();
        for p in ["a", "b", "c"] { h.visit(p.into()); }
        assert_eq!(h.back(), Some("b".into()));
        assert_eq!(h.back(), Some("a".into()));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some("b".into()));
        h.visit("x".into()); // from b: c is no longer ahead
        assert_eq!(h.forward(), None);
        assert_eq!(h.back(), Some("b".into()));
        h.visit("x".into()); h.visit("x".into()); // same folder twice is one step
        assert_eq!(h.v.len(), 3);
    }

    #[test]
    fn type_and_size_read_like_explorers() {
        assert_eq!(type_text("x", true), "File folder");
        assert_eq!(type_text("a.txt", false), "TXT file");
        assert_eq!(type_text("C:\\d\\b.tar.gz", false), "GZ file");
        assert_eq!(type_text(".gitignore", false), "File"); // a dot file has no extension
        assert_eq!(type_text("Makefile", false), "File");
        assert_eq!(size_text(1), "1 KB");
        assert_eq!(size_text(2048), "2 KB");
        assert_eq!(size_text(5 << 20), "5.0 MB");
        assert_eq!(size_text(3 << 30), "3.00 GB");
    }

    #[test]
    fn dates_are_formatted_and_zero_is_blank() {
        assert_eq!(date_text(0), "");
        let t = date_text(132_000_000_000_000_000); // some day in 2019, local time zone
        assert!(t.len() == 16 && t.starts_with("20"), "{t}");
    }

    fn volume(devicetype: u32, unitmask: u32, flags: u16) -> DevBroadcastVolume {
        DevBroadcastVolume { dbcv_size: size_of::<DevBroadcastVolume>() as u32, dbcv_devicetype: devicetype, dbcv_reserved: 0, dbcv_unitmask: unitmask, dbcv_flags: flags }
    }

    #[test]
    fn volume_arrival_and_removal_are_detected() {
        let v = volume(DBT_DEVTYP_VOLUME, 1 << 4, 0);
        let lp = &v as *const _ as LPARAM;
        assert_eq!(volume_mask(WM_DEVICE_CHANGE, DBT_DEVICEARRIVAL as WPARAM, lp), Some(1 << 4));
        assert_eq!(volume_mask(WM_DEVICE_CHANGE, DBT_DEVICEREMOVECOMPLETE as WPARAM, lp), Some(1 << 4));
        assert_eq!(volume_mask(WM_APP, DBT_DEVICEARRIVAL as WPARAM, lp), None);
    }

    #[test]
    fn network_volumes_and_other_device_types_are_ignored() {
        let net = volume(DBT_DEVTYP_VOLUME, 1 << 5, DBTF_NET);
        assert_eq!(volume_mask(WM_DEVICE_CHANGE, DBT_DEVICEARRIVAL as WPARAM, &net as *const _ as LPARAM), None);
        let other = volume(5, 1 << 5, 0);
        assert_eq!(volume_mask(WM_DEVICE_CHANGE, DBT_DEVICEARRIVAL as WPARAM, &other as *const _ as LPARAM), None);
    }

    #[test]
    fn path_on_changed_volume_matches_only_that_drive() {
        let mask = 1 << 4;
        assert!(path_on_mask(Path::new(r"E:\photos"), mask));
        assert!(path_on_mask(Path::new(r"e:\"), mask));
        assert!(!path_on_mask(Path::new(r"C:\photos"), mask));
        assert!(!path_on_mask(Path::new(r"F:\photos"), mask));
    }

    #[test]
    fn shell_paths_are_recognized_and_kept() {
        let dev = PathBuf::from(r"::{35786D3C-B075-49B9-88DD-029876E11C01}\phone\storage");
        assert!(sh::is_shell(&dev));
        assert!(!sh::is_shell(Path::new(r"C:\photos")));
        assert_eq!(row_path(Path::new(r"C:\photos"), dev.to_str().unwrap()), dev);
        assert_eq!(row_path(Path::new(r"C:\photos"), "a.txt"), PathBuf::from(r"C:\photos\a.txt"));
        assert_eq!(up_from(Path::new(r"C:\photos\sub")), Some(PathBuf::from(r"C:\photos")));
    }

    #[test]
    fn device_interface_notifications_are_recognized() {
        let hdr = DevBroadcastHdr { dbch_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32, dbch_devicetype: DBT_DEVTYP_DEVICEINTERFACE, dbch_reserved: 0 };
        let lp = &hdr as *const _ as LPARAM;
        assert!(device_interface_changed(WM_DEVICE_CHANGE, DBT_DEVICEARRIVAL as WPARAM, lp));
        assert!(device_interface_changed(WM_DEVICE_CHANGE, DBT_DEVICEREMOVECOMPLETE as WPARAM, lp));
        assert!(!device_interface_changed(WM_APP, DBT_DEVICEARRIVAL as WPARAM, lp));
        assert!(!device_interface_changed(WM_DEVICE_CHANGE, DBT_DEVICEARRIVAL as WPARAM, &volume(DBT_DEVTYP_VOLUME, 1, 0) as *const _ as LPARAM));
    }

    #[test]
    #[ignore = "requires a connected MTP device"]
    fn mtp_devices_are_enumerated() {
        sh::init();
        let root = sh::children(Path::new(sh::PORTABLE_DEVICES)).expect("cannot enumerate Portable Devices");
        println!("root items={}", root.len());
        let devices = sh::devices();
        if devices.is_empty() { eprintln!("no portable devices present"); return; }
        for (title, path, folder) in &devices { println!("{title}\t{path:?}\t{folder}"); }
        let storage = devices.iter().find(|(_, p, folder)| *folder && sh::is_shell(p)).expect("no storage folder");
        let children = sh::children(&storage.1).expect("cannot enumerate storage");
        assert!(!children.is_empty(), "storage is empty");
        for (title, path, folder) in children.iter().take(10) { println!("  {title}\t{path:?}\t{folder}"); }
    }
}

#[cfg(test)]
mod search_tests {
    #[test]
    fn local_search_finds_names_below_the_folder() {
        let d = std::env::temp_dir().join(format!("rx-search-test-{}", std::process::id()));
        std::fs::create_dir_all(d.join(r"sub\deep")).unwrap();
        for f in [r"Report.txt", r"sub\notes.md", r"sub\deep\REPORT-2.pdf"] { std::fs::write(d.join(f), "x").unwrap(); }
        let (rows, cut, flipped) = super::local_search(&d, "report");
        let (wrong, _, was_flipped) = super::local_search(&d, "кузщке"); // "report" typed in the Russian layout
        let mut names: Vec<String> = rows.iter().map(|r| std::path::Path::new(&r.name).file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(names, ["REPORT-2.pdf", "Report.txt"]); // any case, any depth, absolute paths
        assert!(!cut && !flipped);
        assert!(was_flipped && wrong.len() == 2);
    }

    #[test]
    fn layout_flip_both_ways() {
        assert_eq!(super::flip_layout("сфкпщ"), "cargo");
        assert_eq!(super::flip_layout("ljrevtyns"), "документы");
        assert_eq!(super::flip_layout("Ыфму"), "Save");
    }
}
