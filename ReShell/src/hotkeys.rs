use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering::Relaxed};
use windows_sys::Win32::{Foundation::*, System::LibraryLoader::GetModuleHandleW
, UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*}};

static HOOK: AtomicIsize = AtomicIsize::new(0);
static HELD: AtomicU32 = AtomicU32::new(0);

fn tap(vk: u16, up: bool) -> INPUT {
    let mut i: INPUT = unsafe { std::mem::zeroed() };
    i.r#type = INPUT_KEYBOARD;
    i.Anonymous.ki = KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 };
    i
}

fn send(keys: &[INPUT]) {
    unsafe { SendInput(keys.len() as u32, keys.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
}

fn open_files() {
    use std::{path::PathBuf, process::{Command, Stdio}};
    let mine = std::env::var_os("LOCALAPPDATA").map(|b| PathBuf::from(b).join(r"plugin-launcher\bin\rust-explorer.exe"));
    let beside = std::env::current_exe().ok().map(|e| e.with_file_name("rust-explorer.exe"));
    let installed = std::env::var_os("ProgramFiles").map(|b| PathBuf::from(b).join(r"rust-explorer\rust-explorer.exe"));
    // the newest copy wins: a stale one in the user's folder must not shadow the installed one (two bar buttons, old UI)
    let exe = [installed, mine, beside].into_iter().flatten().filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p))).max().map(|(_, p)| p).unwrap_or_else(|| "explorer.exe".into());
    let _ = Command::new(exe).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

fn open_settings() {
    winrust_common::settings::open("ms-settings:");
}

fn win_action(vk: u32) -> Option<fn()> {
    match vk {
        0x45 => Some(open_files),
        0x44 => Some(winrust_common::desktop::toggle),
        0x49 => Some(open_settings),
        _ => None,
    }
}

unsafe extern "system" fn hook(n: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if n >= 0 {
        let k = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
        let win = unsafe { (GetAsyncKeyState(VK_LWIN as i32) | GetAsyncKeyState(VK_RWIN as i32)) as u16 & 0x8000 != 0 };
        if (w == WM_KEYUP as usize || w == WM_SYSKEYUP as usize) && HELD.load(Relaxed) == k.vkCode {
            HELD.store(0, Relaxed);
        }
        if let (Some(act), true) = (win_action(k.vkCode), win && k.flags & LLKHF_INJECTED == 0) {
            if w == WM_KEYDOWN as usize && HELD.swap(k.vkCode, Relaxed) != k.vkCode {
                send(&[tap(0xE8, false), tap(0xE8, true)]);
                std::thread::spawn(act);
            }
            return 1;
        }
    }
    unsafe { CallNextHookEx(HOOK.load(Relaxed) as _, n, w, l) }
}

pub fn run() -> i32 {
    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), GetModuleHandleW(std::ptr::null()), 0) };
    if h.is_null() { return 1; }
    HOOK.store(h as isize, Relaxed);
    let mut m: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut m, std::ptr::null_mut(), 0, 0) } > 0 {}
    0
}
