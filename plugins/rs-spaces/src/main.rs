use std::{
    collections::HashMap,
    env, fs,
    path::PathBuf,
    sync::{
        LazyLock, Mutex, MutexGuard,
        atomic::{AtomicIsize, AtomicU32, Ordering::Relaxed},
    },
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
    System::{
        Console::FreeConsole,
        LibraryLoader::GetModuleHandleW,
        Threading::{CreateMutexW, GetCurrentProcessId},
    },
    UI::{Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON}, WindowsAndMessaging::*},
};

const SPACE_COUNT: usize = 10;
static SPACES: LazyLock<Mutex<Spaces>> = LazyLock::new(|| Mutex::new(Spaces::default()));
static HELD: AtomicU32 = AtomicU32::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy, Default)]
struct Entry {
    space: usize,
    minimized: bool,
    hidden: bool,
}

#[derive(Default)]
struct Spaces {
    active: usize,
    prev: usize,
    windows: HashMap<isize, Entry>,
}

fn lock() -> MutexGuard<'static, Spaces> {
    SPACES.lock().unwrap_or_else(|e| e.into_inner())
}

fn dir() -> PathBuf {
    let root = env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| env::temp_dir());
    root.join("WinRust").join("rs-spaces")
}

fn state_file() -> PathBuf {
    dir().join("state.txt")
}

fn load() -> Spaces {
    let mut s = Spaces::default();
    let Ok(text) = fs::read_to_string(state_file()) else { return s };
    for line in text.lines() {
        let p: Vec<_> = line.split_whitespace().collect();
        match p.as_slice() {
            ["active", n] => s.active = n.parse().unwrap_or(0).min(SPACE_COUNT - 1),
            ["prev", n] => s.prev = n.parse().unwrap_or(0).min(SPACE_COUNT - 1),
            ["w", h, space, min, hidden] => {
                if let (Ok(h), Ok(space)) = (h.parse::<isize>(), space.parse::<usize>()) {
                    if space < SPACE_COUNT {
                        s.windows.insert(h, Entry { space, minimized: *min == "1", hidden: *hidden == "1" });
                    }
                }
            }
            _ => {}
        }
    }
    s
}

fn save(s: &Spaces) {
    let Ok(_) = fs::create_dir_all(dir()) else { return };
    let mut text = format!("active {}\nprev {}\n", s.active, s.prev);
    for (h, e) in &s.windows {
        text.push_str(&format!("w {} {} {} {}\n", h, e.space, e.minimized as u8, e.hidden as u8));
    }
    let _ = fs::write(state_file(), text);
}

fn show(h: isize, minimized: bool) {
    unsafe { ShowWindow(h as HWND, if minimized { SW_MINIMIZE } else { SW_SHOW }); }
}

fn apply(s: &mut Spaces) {
    let active = s.active;
    for (h, e) in s.windows.iter_mut() {
        let hw = *h as HWND;
        if unsafe { IsWindow(hw) } == 0 { continue; }
        if e.space == active {
            if e.hidden {
                show(*h, e.minimized);
                e.hidden = false;
            } else if unsafe { IsIconic(hw) } != 0 {
                e.minimized = true;
            }
        } else if !e.hidden {
            e.minimized = unsafe { IsIconic(hw) != 0 };
            unsafe { ShowWindow(hw, SW_HIDE); }
            e.hidden = true;
        }
    }
}

unsafe extern "system" fn collect(h: HWND, l: LPARAM) -> i32 {
    let out = unsafe { &mut *(l as *mut Vec<isize>) };
    if unsafe { IsWindowVisible(h) } == 0 || !unsafe { GetWindow(h, GW_OWNER) }.is_null() { return TRUE; }
    let mut class = [0u16; 64];
    let n = unsafe { GetClassNameW(h, class.as_mut_ptr(), class.len() as i32) } as usize;
    let class = String::from_utf16_lossy(&class[..n]).to_lowercase();
    if matches!(class.as_str(), "progman" | "workerw" | "shell_traywnd" | "shell_secondarytraywnd" | "topbar" | "windows.ui.core.corewindow") { return TRUE; }
    let mut cloaked = 0i32;
    if unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED as u32, std::ptr::addr_of_mut!(cloaked).cast(), std::mem::size_of::<i32>() as u32) } == 0 && cloaked != 0 { return TRUE; }
    if unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32 & WS_EX_TOOLWINDOW != 0 { return TRUE; }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    if pid == unsafe { GetCurrentProcessId() } { return TRUE; }
    let len = unsafe { GetWindowTextLengthW(h) } as usize;
    if len == 0 { return TRUE; }
    let mut title = vec![0u16; len + 1];
    unsafe { GetWindowTextW(h, title.as_mut_ptr(), title.len() as i32) };
    if title.iter().take(len).all(|c| (*c as u8).is_ascii_whitespace()) { return TRUE; }
    out.push(h as isize);
    TRUE
}

fn visible_windows() -> Vec<isize> {
    let mut out = Vec::new();
    unsafe { EnumWindows(Some(collect), &mut out as *mut _ as LPARAM); }
    out
}

fn scan() {
    let visible = visible_windows();
    let mut s = lock();
    let active = s.active;
    s.windows.retain(|h, _| unsafe { IsWindow(*h as HWND) } != 0);
    for h in visible {
        let minimized = unsafe { IsIconic(h as HWND) != 0 };
        let e = s.windows.entry(h).or_insert(Entry { space: active, minimized, hidden: false });
        e.minimized = minimized;
        e.hidden = false;
    }
    apply(&mut s);
    save(&s);
}

fn switch(target: usize) {
    if target >= SPACE_COUNT { return; }
    let mut s = lock();
    if target == s.active { return; }
    s.prev = s.active;
    s.active = target;
    apply(&mut s);
    save(&s);
}

fn move_window(h: isize, target: usize) -> bool {
    if target >= SPACE_COUNT || unsafe { IsWindow(h as HWND) } == 0 { return false; }
    let mut s = lock();
    let active = s.active;
    let e = s.windows.entry(h).or_insert(Entry::default());
    e.space = target;
    if target == active {
        if e.hidden {
            show(h, e.minimized);
            e.hidden = false;
        }
    } else if !e.hidden {
        e.minimized = unsafe { IsIconic(h as HWND) != 0 };
        unsafe { ShowWindow(h as HWND, SW_HIDE); }
        e.hidden = true;
    }
    save(&s);
    true
}

fn dragged_window() -> Option<isize> {
    let h = unsafe { GetForegroundWindow() };
    if h.is_null() { return None; }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    if pid == unsafe { GetCurrentProcessId() } { return None; }
    let root = unsafe { GetAncestor(h, GA_ROOT) };
    (!root.is_null()).then_some(root as isize)
}

fn digit_index(vk: u32) -> Option<usize> {
    match vk { 0x30 => Some(9), 0x31..=0x39 => Some(vk as usize - 0x31), _ => None }
}

fn alt_digit(k: &KBDLLHOOKSTRUCT) -> Option<usize> {
    if k.flags & LLKHF_ALTDOWN == 0 || k.flags & LLKHF_INJECTED != 0 { None } else { digit_index(k.vkCode) }
}

unsafe extern "system" fn hook(n: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if n >= 0 {
        let k = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
        let up = w == WM_KEYUP as usize || w == WM_SYSKEYUP as usize;
        let down = w == WM_KEYDOWN as usize || w == WM_SYSKEYDOWN as usize;
        if up && HELD.load(Relaxed) == k.vkCode { HELD.store(0, Relaxed); }
        if let Some(target) = alt_digit(k) {
            if down && HELD.swap(k.vkCode, Relaxed) != k.vkCode {
                std::thread::spawn(move || {
                    let held = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000 != 0 };
                    if held {
                        if let Some(h) = dragged_window() {
                            if move_window(h, target) {
                                switch(target);
                                return;
                            }
                        }
                    }
                    switch(target);
                });
            }
            return 1;
        }
        if k.vkCode == 0xC0 && k.flags & LLKHF_ALTDOWN != 0 && k.flags & LLKHF_INJECTED == 0 {
            if down && HELD.swap(k.vkCode, Relaxed) != k.vkCode {
                std::thread::spawn(|| switch(lock().prev));
            }
            return 1;
        }
    }
    unsafe { CallNextHookEx(HOOK.load(Relaxed) as HHOOK, n, w, l) }
}

fn run() -> i32 {
    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), GetModuleHandleW(std::ptr::null()), 0) };
    if h.is_null() { return 1; }
    HOOK.store(h as isize, Relaxed);
    let mut m: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut m, std::ptr::null_mut(), 0, 0) } > 0 {}
    0
}

fn restore_stale(unlink: bool) {
    let s = load();
    for (h, e) in &s.windows {
        if e.hidden && unsafe { IsWindow(*h as HWND) } != 0 { show(*h, e.minimized); }
    }
    if unlink { let _ = fs::remove_file(state_file()); }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.iter().any(|x| x == "--restore") {
        restore_stale(true);
        std::process::exit(0);
    }
    unsafe {
        let name: Vec<u16> = "Local\\rs-spaces-hotkeys\0".encode_utf16().collect();
        CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS { std::process::exit(0); }
        FreeConsole();
    }
    restore_stale(false);
    *lock() = load();
    for e in lock().windows.values_mut() { e.hidden = false; }
    save(&lock());
    std::thread::spawn(|| loop {
        scan();
        std::thread::sleep(std::time::Duration::from_millis(700));
    });
    std::process::exit(run());
}

#[cfg(test)]
mod tests {
    #[test]
    fn digit_maps_to_space() {
        assert_eq!(super::digit_index(0x31), Some(0));
        assert_eq!(super::digit_index(0x39), Some(8));
        assert_eq!(super::digit_index(0x30), Some(9));
        assert_eq!(super::digit_index(0x41), None);
    }

    #[test]
    fn only_real_alt_digits_are_ours() {
        let mut k: super::KBDLLHOOKSTRUCT = unsafe { std::mem::zeroed() };
        k.vkCode = 0x32;
        k.flags = super::LLKHF_ALTDOWN;
        assert_eq!(super::alt_digit(&k), Some(1));
        k.flags |= super::LLKHF_INJECTED;
        assert_eq!(super::alt_digit(&k), None);
        k.flags = 0;
        assert_eq!(super::alt_digit(&k), None);
    }
}
