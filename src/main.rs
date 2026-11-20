// WinRust installer (plugin-launcher.exe): a small dark Win32 window and a command line that install / remove the plugins.
// The plugin exes are embedded (payload.txt, build.rs); payload\ next to this exe wins over them. Needs admin (see app.manifest).
//   main.rs     the plugin table (PLUGINS) and shared state
//   install/    what each plugin changes in the system and how it is undone
//   cli.rs      the command line;  ui.rs  the window
#![windows_subsystem = "windows"]
#![allow(unsafe_op_in_unsafe_fn)]
#[macro_use]
extern crate winrust_common;
use winrust_common as i18n;
mod cli;
mod install;
mod selection;
mod ui;
use {cli::*, install::*, selection::*, ui::*};
use std::{
    cell::Cell,
    env, fs,
    io::Write,
    mem::{size_of, zeroed},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    ptr::{null, null_mut},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    },
    thread,
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
    System::{Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE}, LibraryLoader::GetModuleHandleW},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const BG: u32 = rgb(22, 22, 24); // the shared WinRust graphite (common/src/style.rs, normal style)
const PANEL: u32 = rgb(34, 34, 38);
const BORDER: u32 = rgb(50, 50, 55);
const TEXT: u32 = rgb(245, 245, 247);
const DIM: u32 = rgb(142, 142, 147);
const ACCENT: u32 = rgb(10, 132, 255);
const GOOD: u32 = winrust_common::style::GOOD;
const BAD: u32 = winrust_common::style::BAD;
const WM_DONE: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3; // not exported by windows-sys under the enabled features

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// A text in both languages for static tables: `.get()` picks the current one.
#[derive(Clone, Copy)]
struct Tr(&'static str, &'static str);
impl Tr {
    fn get(&self) -> &'static str {
        t!(self.0, self.1)
    }
}

struct Plugin {
    name: &'static str,
    desc: Tr,
    warn: Tr, // shown in a confirmation box before install
    installed: fn() -> bool,
    actions: Option<(fn() -> Result<(), String>, fn() -> Result<(), String>)>, // (install, uninstall); None = stub
    settings: bool, // shows a gear button that opens the plugin's settings panel
    bulk: bool, // visible in the window and eligible for bulk install; false = command line only
    glyph: &'static str, // Segoe MDL2 Assets icon
}
const PLUGINS: &[Plugin] = &[
    Plugin {
        name: "rust-search",
        desc: Tr("Быстрый поиск файлов по NTFS (Alt+Space). Заменяет поиск Windows: служба WSearch отключается, SearchHost блокируется.", "Fast NTFS file search (Alt+Space). Replaces Windows Search: the WSearch service is disabled and SearchHost is blocked."),
        warn: Tr("Служба поиска Windows (WSearch) будет ОТКЛЮЧЕНА, SearchHost.exe заблокирован, строка поиска на панели задач скрыта: поиск в Пуске, Проводнике и Outlook перестанет работать.", "The Windows Search service (WSearch) will be DISABLED, SearchHost.exe blocked and the taskbar search box hidden: search in Start, Explorer and Outlook will stop working."),
        installed: || prog_dir().join("rs.exe").is_file(),
        actions: Some((install, uninstall)),
        settings: false,
        bulk: true,
        glyph: "\u{E721}", // search
    },
    Plugin {
        name: "rtaskmgr-win32",
        desc: Tr("Диспетчер задач на Rust. Заменяет стандартный: Ctrl+Shift+Esc и запуск taskmgr.exe открывают его.", "A task manager in Rust. Replaces the stock one: Ctrl+Shift+Esc and launching taskmgr.exe open it."),
        warn: Tr("Стандартный диспетчер задач будет ЗАМЕНЁН: любой запуск taskmgr.exe откроет rtaskmgr-win32.", "The stock Task Manager will be REPLACED: any launch of taskmgr.exe opens rtaskmgr-win32."),
        installed: tm_installed,
        actions: Some((tm_install, tm_uninstall)),
        settings: false,
        bulk: true,
        glyph: "\u{E9D9}", // diagnostic
    },
    Plugin {
        name: "topbar",
        desc: Tr("Нижняя панель в стиле Windows 11 вместо панели задач: окна, часы, меню. Стандартная панель скрывается. Win+1..9/0 открывают иконки приложений на панели.", "A Windows 11-style bottom bar instead of the taskbar: windows, clock, menus. The stock taskbar is hidden. Win+1..9/0 activate app icons on the bar."),
        warn: Tr("Панель задач Windows будет СКРЫТА и заменена панелью внизу экрана. Удаление плагина возвращает её.", "The Windows taskbar will be HIDDEN and replaced by a bar at the bottom of the screen. Uninstalling the plugin brings it back."),
        installed: || bar_dir().join("topbar.exe").is_file(),
        actions: Some((bar_install, bar_uninstall)),
        settings: true,
        bulk: true,
        glyph: "\u{E700}", // menu
    },
    Plugin {
        name: RUST_EXPLORER,
        desc: Tr("Файловый менеджер на Rust вместо Проводника: папки и диски открываются в нём (Win+E тоже), рабочий стол рисует он сам. Обычный explorer.exe как оболочка ОТКЛЮЧАЕТСЯ. Нужен topbar.", "A Rust file manager instead of Explorer: folders and drives open in it (Win+E too), and it draws the desktop itself. The regular explorer.exe as the shell is DISABLED. Requires topbar."),
        warn: Tr("ОБЫЧНЫЙ Explorer будет ОТКЛЮЧЁН: после установки панель задач, рабочий стол и окна папок дают topbar и rust-explorer. Сначала установите topbar, без него не будет панели. Ещё для экономии RAM отключаются службы PcaSvc, InventorySvc, lfsvc (геолокация), cbdhsvc (журнал буфера Win+V), DoSvc ставится вручную, службы svchost объединяются (полностью после перезагрузки); удаление плагина вернёт их. Если что-то пойдёт не так: Ctrl+Alt+Del, Диспетчер задач, Запустить новую задачу, explorer.exe; затем удалите плагин.", "The REGULAR Explorer will be DISABLED: after install, topbar and rust-explorer provide the taskbar, the desktop and folder windows. Install topbar first, without it there will be no bar. To save RAM the services PcaSvc, InventorySvc, lfsvc (geolocation) and cbdhsvc (Win+V clipboard history) are also disabled, DoSvc is set to manual, and svchost services are merged (fully after a reboot); uninstalling the plugin restores them. If something goes wrong: Ctrl+Alt+Del, Task Manager, Run new task, explorer.exe; then uninstall the plugin."),
        installed: || rx_dir().join("rust-explorer.exe").is_file(),
        actions: Some((rx_install, rx_uninstall)),
        settings: false,
        bulk: true,
        glyph: "\u{E8B7}", // folder
    },
    Plugin {
        name: "wiztree",
        desc: Tr("Анализ диска: карта занятого места (treemap) по MFT за секунды. Ярлык «WinRust Disk» в меню Пуск, запускается от администратора.", "Disk analysis: a treemap of used space from the MFT in seconds. A 'WinRust Disk' shortcut in the Start menu, runs as administrator."),
        warn: Tr("Ничего системного не меняется: файл в Program Files и ярлык в меню Пуск.", "Nothing in the system is changed: a file in Program Files and a Start menu shortcut."),
        installed: || wz_dir().join("wiztree.exe").is_file(),
        actions: Some((wz_install, wz_uninstall)),
        settings: false,
        bulk: true,
        glyph: "\u{EDA2}", // hard drive
    },
    Plugin {
        name: "photoview",
        desc: Tr("Просмотр фото: открывается мгновенно, листает папку стрелками, учитывает поворот из EXIF. Появляется в «Открыть с помощью» (по умолчанию не назначается).", "Photo viewer: opens instantly, browses the folder with the arrow keys, honors EXIF rotation. Appears in 'Open with' (not set as the default)."),
        warn: Tr("Программа добавляется в «Открыть с помощью» для jpg, png, bmp, gif, tiff, webp, heic, avif. Приложением по умолчанию её делаете вы сами в Параметрах.", "The program is added to 'Open with' for jpg, png, bmp, gif, tiff, webp, heic, avif. You make it the default app yourself in Settings."),
        installed: || pv_dir().join("photoview.exe").is_file(),
        actions: Some((pv_install, pv_uninstall)),
        settings: false,
        bulk: true,
        glyph: "\u{E91B}", // picture
    },
    Plugin {
        name: "rust-settings",
        desc: Tr("Параметры на Rust: экран (разрешение, частота), звук (громкость), тема, о системе. Win+I и меню панели открывают их, а не Параметры Windows, поэтому Проводник не нужен. Остальные разделы открывают системные.", "Settings in Rust: display (resolution, refresh rate), sound (volume), theme, about. Win+I and the bar menus open it instead of Windows Settings, so Explorer is not needed. Other pages open the system ones."),
        warn: Tr("Ничего системного не меняется: файл в Program Files и ярлык «WinRust Settings» в меню Пуск. Win+I начинает открывать это окно; удалите плагин, и всё вернётся.", "Nothing in the system is changed: a file in Program Files and a 'WinRust Settings' Start menu shortcut. Win+I starts opening this window; uninstall the plugin and it goes back."),
        installed: || st_dir().join("rust-settings.exe").is_file(),
        actions: Some((st_install, st_uninstall)),
        settings: false,
        bulk: true,
        glyph: "\u{E713}", // settings
    },
    Plugin {
        name: "rs-spaces",
        desc: Tr("Свои виртуальные пространства поверх обычного рабочего стола: Alt+1..9/0 переключают их, Alt+` возвращает на предыдущее, Alt+цифра с зажатой мышью переносит окно. Не использует Virtual Desktop из Windows 10 и не создаёт NT-десктопы.", "Custom virtual spaces on top of the normal desktop: Alt+1..9/0 switch them, Alt+` returns to the previous one, Alt+digit with the mouse held moves a window. Uses neither Win10 virtual desktops nor NT desktops."),
        warn: Tr("Окна неактивного пространства скрываются (SW_HIDE) и показываются при возврате. При аварийном завершении процесс оставляет журнал скрытых окон; `rs-spaces.exe --restore` или повторный запуск возвращает их. Системные настройки не меняются, добавляется только автозапуск в HKCU.", "Windows of the inactive space are hidden (SW_HIDE) and shown again on return. After a crash the process leaves a journal of hidden windows; `rs-spaces.exe --restore` or the next start brings them back. No system settings change, only an HKCU autostart value is added."),
        installed: || sp_dir().join("rs-spaces.exe").is_file(),
        actions: Some((sp_install, sp_uninstall)),
        settings: false,
        bulk: true,
        glyph: "\u{E7FC}",
    },
    Plugin {
        name: "debloat",
        desc: Tr("Отключает мусор Windows: телеметрию, Copilot, Recall, рекламу в Пуске, фон Edge. Только обратимое, старые значения запоминаются.", "Disables Windows junk: telemetry, Copilot, Recall, Start menu ads, the Edge background. Reversible changes only, old values are remembered."),
        warn: Tr("Будут ОТКЛЮЧЕНЫ несколько служб (телеметрия, карты, отчёты об ошибках) и выставлены политики реестра (Copilot, Recall, реклама, фон Edge; Edge покажет «управляется организацией»). Старые значения сохраняются, uninstall debloat возвращает всё. Приложения не удаляются.", "Several services will be DISABLED (telemetry, maps, error reporting) and registry policies set (Copilot, Recall, ads, Edge background; Edge will show 'managed by your organization'). Old values are saved, uninstall debloat restores everything. No apps are removed."),
        installed: || db_dir().join("debloat.exe").is_file(),
        actions: Some((db_install, db_uninstall)),
        settings: false,
        bulk: false, // changes system behaviour: only on request, never as part of "install everything"
        glyph: "\u{E74D}", // delete
    },
    Plugin {
        name: "purge",
        desc: Tr("БЕЗВОЗВРАТНО удаляет предустановленный мусор, как Tiny11: Clipchamp, Новости, Погода, Xbox-оверлеи, Teams, Cortana, виджеты, Skype, Solitaire и др., а также задачи телеметрии. Отката нет.", "PERMANENTLY removes the bundled junk like Tiny11 does: Clipchamp, News, Weather, Xbox overlays, Teams, Cortana, Widgets, Skype, Solitaire and more, plus telemetry tasks. There is no undo."),
        warn: Tr("НЕОБРАТИМО: приложения будут удалены для всех пользователей и из образа (новые пользователи их тоже не получат), задачи телеметрии удалены. Вернуть можно только из Store или winget. Магазин, Параметры, Пуск, Калькулятор, Фото и среды выполнения не затрагиваются. OneDrive и Edge не входят: purge.exe --apply --onedrive --edge. Сначала посмотрите список: purge.exe без параметров ничего не меняет.", "IRREVERSIBLE: apps are removed for all users and from the image (new users will not get them), telemetry tasks are deleted. They come back only from the Store or winget. The Store, Settings, Start, Calculator, Photos and runtimes are not touched. OneDrive and Edge are not included: purge.exe --apply --onedrive --edge. Look at the list first: purge.exe with no arguments changes nothing."),
        installed: || pu_dir().join("purge.exe").is_file(),
        actions: Some((pu_install, pu_uninstall)),
        settings: false,
        bulk: false, // destructive and permanent: only on request, never as part of "install everything"
        glyph: "\u{E74D}", // delete
    },
    Plugin { name: "chrome-rs", desc: Tr("Браузер на Rust. Установка пока недоступна (заглушка).", "A browser in Rust. Installation is not available yet (placeholder)."), warn: Tr("", ""), installed: || false, actions: None, settings: false, bulk: true, glyph: "\u{E774}" }, // globe
];

static BUSY: AtomicBool = AtomicBool::new(false);
static JOB: AtomicUsize = AtomicUsize::new(usize::MAX); // what BUSY is working on: plugin index, or PLUGINS.len() = rollback
static INSTALLED: [AtomicBool; PLUGINS.len()] = [const { AtomicBool::new(false) }; PLUGINS.len()];
fn refresh() {
    for (p, f) in PLUGINS.iter().zip(&INSTALLED) {
        f.store((p.installed)(), Relaxed);
    }
}
// Initialized when the window opens. Refreshing installed state preserves the user's checkbox choices.
static SELECTED: [AtomicBool; PLUGINS.len()] = [const { AtomicBool::new(false) }; PLUGINS.len()];
static MSG: Mutex<(String, bool)> = Mutex::new((String::new(), true));
thread_local! { static SETTINGS: Cell<Option<usize>> = const { Cell::new(None) }; } // plugin whose settings panel is open
thread_local! { static HOVER: Cell<Option<usize>> = const { Cell::new(None) }; }

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if !args.is_empty() {
        std::process::exit(cli(&args));
    }
    refresh();
    for i in ready() {
        SELECTED[i].store(selected_by_default(PLUGINS[i].name, INSTALLED[i].load(Relaxed)), Relaxed);
    }
    unsafe {
        let (mut token, mut input): (usize, GdiplusStartupInput) = (0, zeroed());
        input.GdiplusVersion = 1;
        GdiplusStartup(&mut token, &input, null_mut());
        let hi = GetModuleHandleW(null());
        let cls = wide("WinRust");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hi, hCursor: LoadCursorW(null_mut(), IDC_ARROW), lpszClassName: cls.as_ptr(), ..zeroed() };
        RegisterClassW(&wc);
        let dc = GetDC(null_mut());
        let s = GetDeviceCaps(dc, LOGPIXELSX as i32) as f32 / 96.0;
        ReleaseDC(null_mut(), dc);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT { left: 0, top: 0, right: (WIN_W * s) as i32, bottom: (WIN_H * s) as i32 };
        AdjustWindowRect(&mut r, style, 0);
        let title = wide("WinRust");
        let hwnd = CreateWindowExW(0, cls.as_ptr(), title.as_ptr(), style | WS_VISIBLE, CW_USEDEFAULT, CW_USEDEFAULT, r.right - r.left, r.bottom - r.top, null_mut(), null_mut(), hi, null());
        DwmSetWindowAttribute(hwnd, 20, &1i32 as *const i32 as *const _, 4); // dark title bar
        DwmSetWindowAttribute(hwnd, 35, &BG as *const u32 as *const _, 4); // caption colour = window background (Win11; ignored on Win10)
        let mut m: MSG = zeroed();
        while GetMessageW(&mut m, null_mut(), 0, 0) > 0 {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}
