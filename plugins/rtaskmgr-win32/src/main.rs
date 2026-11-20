#![windows_subsystem = "windows"]
#[macro_use]
extern crate winrust_common;
mod app;
mod boost;
mod data;
mod draw;
mod gpu;
mod hw;
mod model;
mod net;
mod perf;
mod res;
mod startup;
mod store;
mod suspect;
mod sys;
mod ui;

use app::*;
use data::*;
use model::*;
use native_windows_gui as nwg;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};
use ui::*;
use winapi::um::winuser::*;

/// Самопроверка ETW: сессия + трафик через localhost + чтение счётчиков своего процесса. Результат — в файл.
fn net_selftest(out: &str) {
    use std::io::{Read, Write};
    let report = (|| -> Result<String, String> {
        let ses = net::start().ok_or(t!("не удалось запустить ETW-сессию (нужны права администратора)", "could not start the ETW session (administrator rights needed)"))?;
        std::thread::sleep(Duration::from_millis(500));
        let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let addr = l.local_addr().map_err(|e| e.to_string())?;
        let srv = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 100_000];
            let mut got = 0;
            while got < 200_000 {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
            }
        });
        let mut c = std::net::TcpStream::connect(addr).map_err(|e| e.to_string())?;
        c.write_all(&vec![7u8; 200_000]).map_err(|e| e.to_string())?;
        drop(c);
        let _ = srv.join();
        std::thread::sleep(Duration::from_secs(2));
        let me = std::process::id();
        let all = net::drain();
        net::stop(ses);
        let (rx, tx) = all.get(&me).copied().unwrap_or((0, 0));
        Ok(tf!("pid={me} принято={rx} отправлено={tx} процессов_в_данных={}", "pid={me} received={rx} sent={tx} processes_in_data={}", all.len()))
    })();
    let _ = std::fs::write(out, report.unwrap_or_else(|e| tf!("ОШИБКА: {e}", "ERROR: {e}")));
}

fn xy(l: isize) -> (i32, i32) {
    ((l & 0xFFFF) as i16 as i32, ((l >> 16) & 0xFFFF) as i16 as i32)
}

fn main() {
    if let Some(i) = std::env::args().position(|a| a == "--net-selftest")
        && let Some(out) = std::env::args().nth(i + 1) {
            return net_selftest(&out);
        }
    let args: Vec<String> = std::env::args().skip(1).collect();
    // установщик при удалении: вернуть всё, что включили на странице «Ускорение» (код выхода 1 = что-то не вернулось)
    if args.first().is_some_and(|a| a == "--undo-speed") {
        let errs = boost::undo_all();
        for e in &errs {
            eprintln!("{e}");
        }
        std::process::exit(!errs.is_empty() as i32);
    }
    // повторный вызов с правами администратора из start_real_taskmgr: только открыть системный диспетчер
    if args.first().is_some_and(|a| a == "--real-taskmgr") {
        sys::start_real_taskmgr(&args[1..].join(" "), false);
        return;
    }
    // Без прав администратора диспетчер не закроет защищённые процессы (Malwarebytes и т.п.): перезапускаемся через
    // задачу планировщика — с правами и без UAC. "--tray" передаём файлом: у задачи нет аргументов.
    let tray_flag = store::dir().join("tray.flag");
    if !sys::is_admin() {
        if args.iter().any(|a| a == "--tray") {
            let _ = std::fs::create_dir_all(store::dir());
            let _ = std::fs::write(&tray_flag, "");
        }
        if sys::run_elevated_task() {
            return;
        }
        let _ = std::fs::remove_file(&tray_flag);
    }
    let ifeo = sys::taskmgr_args(&args); // Some = нас запустили вместо taskmgr.exe
    // Мы подменяем системный диспетчер: если упадём, Ctrl+Shift+Esc не должен остаться без ответа.
    // (panic = "abort" всё равно сначала вызывает этот хук.)
    let fallback_args = ifeo.clone().unwrap_or_default();
    std::panic::set_hook(Box::new(move |info| {
        let dir = store::dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("crash.log"), format!("{info}\n{}", std::backtrace::Backtrace::force_capture()));
        sys::start_real_taskmgr(&fallback_args, true);
    }));
    let hidden = args.iter().any(|a| a == "--tray") || (sys::is_admin() && std::fs::remove_file(&tray_flag).is_ok()); // запуск из автозагрузки: сразу в трей
    nwg::enable_visual_styles();
    nwg::init().expect("не удалось инициализировать Win32");
    let scale = nwg::scale_factor();
    let mut font = nwg::Font::default();
    nwg::Font::builder().family("Segoe UI").size(17).build(&mut font).unwrap();
    nwg::Font::set_global_default(Some(font));

    store::trim_history(10_080);
    let cfg = store::load();
    // `taskmgr /0 /startup` (так «Параметры» открывают автозагрузку) — сразу на эту страницу
    let startup_asked = ifeo.as_deref().is_some_and(|a| a.to_lowercase().contains("/startup"));
    let page = if startup_asked { Page::Startup } else { NAV.get(cfg.view).map_or(Page::Processes, |n| n.0) };

    // иконки из ресурса exe (id 1); если ресурса нет — системная
    let embed = nwg::EmbedResource::load(None).ok();
    let load_icon = |out: &mut nwg::Icon, size: (u32, u32)| {
        let ok = embed.as_ref().is_some_and(|e| nwg::Icon::builder().source_embed(Some(e)).source_embed_id(1).size(Some(size)).build(out).is_ok());
        if !ok {
            nwg::Icon::builder().source_system(Some(nwg::OemIcon::Information)).build(out).unwrap();
        }
    };
    let (mut icon_big, mut tray_icon) = (nwg::Icon::default(), nwg::Icon::default());
    load_icon(&mut icon_big, (32, 32));
    load_icon(&mut tray_icon, (16, 16));

    let mut win = nwg::Window::default();
    nwg::Window::builder()
        .flags(if hidden { nwg::WindowFlags::MAIN_WINDOW } else { nwg::WindowFlags::MAIN_WINDOW | nwg::WindowFlags::VISIBLE })
        .icon(Some(&icon_big))
        .size((cfg.size.0 as i32, cfg.size.1 as i32))
        .position(cfg.pos)
        .title(t!("Диспетчер задач", "Task Manager"))
        .build(&mut win)
        .unwrap();
    let mut search = nwg::TextInput::default();
    nwg::TextInput::builder()
        .parent(&win)
        .placeholder_text(Some(t!("Введите имя, издателя или PID для поиска", "Type a name, publisher or PID to search")))
        .flags(nwg::TextInputFlags::VISIBLE | nwg::TextInputFlags::TAB_STOP)
        .position((0, 0))
        .size((300, 20))
        .build(&mut search)
        .unwrap();
    #[allow(deprecated)] // the plain WM_TIMER is all a 1-second refresh needs; AnimationTimer runs its own thread
    let mut timer = nwg::Timer::default();
    #[allow(deprecated)]
    nwg::Timer::builder().parent(&win).interval(1000).stopped(false).build(&mut timer).unwrap();
    let mut tray = nwg::TrayNotification::default();
    nwg::TrayNotification::builder().parent(&win).icon(Some(&tray_icon)).tip(Some(t!("Диспетчер задач (Rust)", "Task Manager (Rust)"))).build(&mut tray).unwrap();

    let ui = Ui {
        page,
        nav_open: false,
        th: theme(true),
        rows: vec![],
        sel: None,
        expanded: HashSet::new(),
        sorts: HashMap::from([(Page::Processes, (cfg.sort.min(7), cfg.desc))]),
        search: String::new(),
        hit: Hit::None,
        mouse: (0, 0),
        scroll: 0.0,
        drag: None,
        perf_sel: draw::PerfKind::Cpu,
        toast: None,
        tip: None,
        lay: None,
        mini: cfg.mini,
        search_open: false,
        last_click: None,
    };
    let app = Rc::new(App {
        win,
        search,
        timer,
        tray,
        tray_icon,
        icon_big,
        data: RefCell::new(Data::new(cfg.clone())),
        ui: RefCell::new(ui),
        fonts: Fonts::new(scale),
        scale,
        edit_brush: Cell::new(0),
        search_rect: Cell::new(R::new(0, 0, 0, 0)),
        last_tick: Cell::new(Instant::now()),
    });
    app.apply_theme();
    {
        let mut d = app.data.borrow_mut();
        match page {
            Page::Startup => d.load_startup(),
            Page::History => d.load_history(),
            _ => {}
        }
    }
    app.tick();

    // горячая клавиша Ctrl+Alt+T
    let hwnd = app.win.handle.hwnd().unwrap();
    unsafe { RegisterHotKey(hwnd, 1, (MOD_CONTROL | MOD_ALT) as u32, b'T' as u32) };

    let a = app.clone();
    let _raw = nwg::bind_raw_event_handler(&app.win.handle, 0x10000, move |hwnd, msg, w, l| -> Option<isize> {
        match msg {
            WM_PAINT => {
                a.paint(hwnd);
                Some(0)
            }
            WM_ERASEBKGND => Some(1),
            WM_GETMINMAXINFO => unsafe {
                let mmi = &mut *(l as *mut MINMAXINFO);
                let (mw, mh) = if a.ui.borrow().mini { (360.0, 260.0) } else { (860.0, 560.0) };
                mmi.ptMinTrackSize = winapi::shared::windef::POINT { x: (mw * a.scale) as i32, y: (mh * a.scale) as i32 };
                Some(0)
            },
            WM_SIZE => {
                a.invalidate();
                None
            }
            WM_MOUSEMOVE => {
                let (x, y) = xy(l);
                a.on_mouse_move(hwnd, x, y);
                None
            }
            WM_MOUSELEAVE => {
                a.on_mouse_leave();
                None
            }
            WM_LBUTTONDOWN => {
                let (x, y) = xy(l);
                a.on_lbutton_down(hwnd, x, y);
                Some(0)
            }
            WM_LBUTTONUP => {
                a.on_lbutton_up();
                None
            }
            WM_RBUTTONUP => {
                let (x, y) = xy(l);
                a.on_rbutton_up(hwnd, x, y);
                Some(0)
            }
            WM_MOUSEWHEEL => {
                a.on_wheel(((w >> 16) & 0xFFFF) as i16);
                Some(0)
            }
            WM_KEYDOWN => {
                a.on_key(hwnd, w as u32);
                None
            }
            WM_TIMER if w == 77 => {
                a.on_tip_timer(hwnd);
                Some(0)
            }
            WM_CTLCOLOREDIT => unsafe {
                // поле поиска: цвета как у плашки в шапке
                let th = a.ui.borrow().th;
                winapi::um::wingdi::SetTextColor(w as _, th.text.c());
                winapi::um::wingdi::SetBkColor(w as _, th.panel.c());
                Some(a.edit_brush.get())
            },
            WM_HOTKEY if w == 1 => {
                a.show_window();
                Some(0)
            }
            _ => None,
        }
    })
    .unwrap();

    let a = app.clone();
    let _h = nwg::full_bind_event_handler(&app.win.handle, move |evt, data, handle| {
        use nwg::Event as E;
        match evt {
            E::OnWindowClose => {
                if let nwg::EventData::OnWindowClose(d) = data {
                    let to_tray = a.data.borrow().cfg.to_tray;
                    if to_tray {
                        d.close(false);
                        a.save_settings();
                        a.win.set_visible(false);
                    } else {
                        a.quit();
                    }
                }
            }
            E::OnTimerTick => a.on_timer(),
            E::OnTextInput if handle == a.search.handle => a.on_search_changed(),
            E::OnMousePress(nwg::MousePressEvent::MousePressLeftUp) if handle == a.tray.handle => a.show_window(),
            E::OnContextMenu if handle == a.tray.handle => {
                let (x, y) = nwg::GlobalCursor::position();
                let items = a.tray_menu();
                if let Some(h) = a.win.handle.hwnd()
                    && let Some(act) = a.show_menu(h, &items, x, y)
                {
                    a.do_act(act);
                }
            }
            _ => {}
        }
    });
    nwg::dispatch_thread_events();
}
