//! Окно приложения: состояние интерфейса, раскладка, события, меню и действия.
use crate::{data::*, draw::*, model::*, startup, store, sys, ui::*};
use native_windows_gui as nwg;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use winapi::shared::windef::{HMENU, HWND};
use winapi::um::winuser::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Btn {
    Search,
    RunTask,
    EndTask,
    Efficiency,
    More,
    SvcStart,
    SvcStop,
    StartEnable,
    StartDisable,
    RuleDelete,
    BoostOn,
    BoostOff,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    None,
    NavToggle,
    Nav(usize),
    Button(Btn),
    Header(usize),
    Row(usize),
    Chevron(usize),
    VBar,
    PerfItem(usize),
    Setting(usize),
}

#[derive(Clone)]
pub struct Lay {
    pub client: R,
    pub nav: R,
    pub nav_toggle: R,
    pub nav_items: Vec<R>,
    pub band: R,
    pub title: R,
    pub search: R,
    pub buttons: Vec<(R, Btn, &'static str, char)>,
    pub body: R,
    pub header: R,
    pub rows_area: R,
    pub col_x: Vec<i32>,
    pub vbar: R,
    pub perf_list: R,
    pub perf_items: Vec<R>,
    pub perf_main: R,
    pub setting_cards: Vec<(R, SettingId)>,
}

pub struct Ui {
    pub page: Page,
    pub nav_open: bool,
    pub th: Theme,
    pub rows: Vec<Row>,
    pub sel: Option<String>,
    pub expanded: HashSet<String>,
    pub sorts: HashMap<Page, (usize, bool)>,
    pub search: String,
    pub hit: Hit,
    pub mouse: (i32, i32),
    pub scroll: f64,
    pub drag: Option<(i32, f64)>,
    pub perf_sel: PerfKind,
    pub toast: Option<(String, Instant)>,
    pub tip: Option<(String, (i32, i32))>,
    pub lay: Option<Lay>,
    pub mini: bool,
    pub search_open: bool,
    pub last_click: Option<(Instant, String)>,
}

impl Ui {
    pub fn sort_of(&self, p: Page) -> (usize, bool) {
        self.sorts.get(&p).copied().unwrap_or_else(|| default_sort(p))
    }
    pub fn page_has_search(&self) -> bool {
        matches!(self.page, Page::Processes | Page::Users | Page::Details | Page::Services | Page::Startup | Page::History | Page::Rules)
    }
    pub fn has_table(&self) -> bool {
        self.page_has_search() || self.page == Page::Boost
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    RunTask,
    EndTask,
    EndTree,
    Efficiency,
    Suspend(bool),
    Prio(u32),
    Affinity(bool),
    OpenDir,
    CopyCmd,
    RuleLower,
    RuleNotify,
    RuleDelete,
    SvcStart,
    SvcStop,
    StartEnable,
    StartDisable,
    BoostOn,
    BoostOff,
    Export,
    SetTheme(u8),
    ToggleTray,
    ToggleMini,
    ToggleAutostart,
    ToggleEtw,
    ToggleGpuTemp,
    ShowWindow,
    RelaunchAdmin,
    Exit,
    Toggle,
    Go(Page),
}

pub enum MI {
    Item(&'static str, Act),
    Check(&'static str, Act, bool),
    Sep,
    Sub(&'static str, Vec<MI>),
}

pub struct App {
    pub win: nwg::Window,
    pub search: nwg::TextInput,
    #[allow(dead_code, deprecated)] // owned only to stay alive: dropping it stops the 1 s refresh
    pub timer: nwg::Timer,
    pub tray: nwg::TrayNotification,
    #[allow(dead_code)] // the tray and the window keep using these handles: they must outlive them
    pub tray_icon: nwg::Icon,
    #[allow(dead_code)]
    pub icon_big: nwg::Icon,
    pub data: RefCell<Data>,
    pub ui: RefCell<Ui>,
    pub fonts: Fonts,
    pub scale: f64,
    pub edit_brush: Cell<isize>,
    pub search_rect: Cell<R>,
    pub last_tick: Cell<Instant>,
}

fn layout_log_path() -> Option<&'static std::ffi::OsString> {
    static P: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();
    P.get_or_init(|| std::env::var_os("RTASKMGR_LAYOUT_LOG")).as_ref()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn page_buttons(p: Page) -> Vec<(Btn, &'static str, char)> {
    match p {
        Page::Processes => vec![(Btn::RunTask, t!("Запустить новую задачу", "Run new task"), '\u{E710}'), (Btn::EndTask, t!("Снять задачу", "End task"), '\u{E711}'), (Btn::Efficiency, t!("Режим эффективности", "Efficiency mode"), '\u{E945}'), (Btn::More, "", '\u{E712}')],
        Page::Users => vec![(Btn::EndTask, t!("Снять задачу", "End task"), '\u{E711}'), (Btn::More, "", '\u{E712}')],
        Page::Details => vec![(Btn::RunTask, t!("Запустить новую задачу", "Run new task"), '\u{E710}'), (Btn::EndTask, t!("Снять задачу", "End task"), '\u{E711}'), (Btn::More, "", '\u{E712}')],
        Page::Services => vec![(Btn::SvcStart, t!("Запустить", "Start"), '\u{E768}'), (Btn::SvcStop, t!("Остановить", "Stop"), '\u{E71A}'), (Btn::More, "", '\u{E712}')],
        Page::Startup => vec![(Btn::StartEnable, t!("Включить", "Enable"), '\u{E73E}'), (Btn::StartDisable, t!("Отключить", "Disable"), '\u{E711}'), (Btn::More, "", '\u{E712}')],
        Page::Rules => vec![(Btn::RuleDelete, t!("Удалить правило", "Delete rule"), '\u{E74D}'), (Btn::More, "", '\u{E712}')],
        Page::History => vec![(Btn::More, "", '\u{E712}')],
        Page::Boost => vec![(Btn::BoostOn, t!("Включить", "Enable"), '\u{E73E}'), (Btn::BoostOff, t!("Выключить", "Disable"), '\u{E711}')],
        _ => vec![],
    }
}

impl App {
    // ================= раскладка =================
    pub fn compute_layout(&self, g: &G, ui: &Ui, d: &Data, w: i32, h: i32) -> Lay {
        let s = g.s;
        let px = |v: f64| (v * s).round() as i32;
        let (nav, nav_toggle, nav_items, band, title, buttons, search, body);
        if ui.mini {
            let (bh, nh) = (px(MINI_BAND_H), px(MINI_NAV_H));
            band = R::new(0, 0, w, bh);
            nav = R::new(0, h - nh, w, h);
            nav_toggle = R::new(0, 0, 0, 0);
            let (cell, gap) = (px(30.0), px(4.0));
            let mut x = (w - (NAV.len() as i32 * (cell + gap) - gap)) / 2;
            nav_items = NAV.iter().map(|_| {
                let r = R::new(x, nav.t + (nh - cell) / 2, x + cell, nav.t + (nh - cell) / 2 + cell);
                x += cell + gap;
                r
            }).collect();
            title = R::new(0, 0, 0, 0);
            let mut entries = vec![];
            if ui.page_has_search() { entries.push((Btn::Search, t!("Поиск", "Search"), '\u{E721}')); }
            entries.extend(page_buttons(ui.page));
            let (bw, bgap) = (px(52.0), px(4.0));
            let mut bx = px(6.0);
            buttons = entries.into_iter().map(|(btn, label, glyph)| {
                let r = R::new(bx, px(3.0), bx + bw, bh - px(3.0));
                bx += bw + bgap;
                (r, btn, label, glyph)
            }).collect();
            let l = (bx + bgap).min((w - px(140.0)).max(px(6.0)));
            let rr = (l + px(140.0)).min(w - px(8.0));
            search = if ui.page_has_search() && ui.search_open { R::new(l, px(12.0), rr.max(l + px(60.0)), px(42.0)) } else { R::new(0, 0, 0, 0) };
            body = R::new(0, band.b, w, nav.t);
        } else {
            let nav_w = px(if ui.nav_open { NAV_W_OPEN } else { NAV_W });
            nav = R::new(0, 0, nav_w, h);
            nav_toggle = R::new(0, px(6.0), px(NAV_W), px(6.0 + 44.0));
            nav_items = (0..NAV.len()).map(|i| R::new(0, px(56.0 + i as f64 * 44.0), nav_w, px(56.0 + i as f64 * 44.0 + 44.0))).collect();
            band = R::new(nav_w, 0, w, px(BAND_H));
            title = R::new(band.l + px(20.0), 0, band.l + px(320.0), band.b);
            let mut bs = vec![];
            let mut right = band.r - px(12.0);
            for (btn, label, glyph) in page_buttons(ui.page).into_iter().rev() {
                let bw = if label.is_empty() { px(36.0) } else { g.text_w(label, Font::Ui) + px(48.0) };
                let r = R::new(right - bw, px(10.0), right, px(10.0 + 32.0));
                bs.push((r, btn, label, glyph));
                right -= bw + px(4.0);
            }
            bs.reverse();
            let btn_left = bs.first().map_or(band.r, |b| b.0.l);
            let sw = px(440.0).min((btn_left - title.l - px(220.0)).max(px(160.0)));
            let sx = (band.l + btn_left) / 2 - sw / 2 + px(60.0);
            let sl = sx.max(title.l + px(190.0));
            search = R::new(sl, px(10.0), sl + sw, px(10.0 + 32.0));
            buttons = bs;
            body = R::new(nav_w, band.b, w, h);
        }
        let mut lay = Lay {
            client: R::new(0, 0, w, h), nav, nav_toggle, nav_items, band, title, search, buttons, body,
            header: R::new(0, 0, 0, 0),
            rows_area: R::new(0, 0, 0, 0),
            col_x: vec![],
            vbar: R::new(0, 0, 0, 0),
            perf_list: R::new(0, 0, 0, 0),
            perf_items: vec![],
            perf_main: R::new(0, 0, 0, 0),
            setting_cards: vec![],
        };
        if ui.has_table() {
            let cols = columns(ui.page);
            let hh = px(if ui.mini { 28.0 } else if ui.page == Page::Processes { 56.0 } else { 36.0 });
            lay.header = R::new(body.l, body.t, body.r, body.t + hh);
            lay.rows_area = R::new(body.l, body.t + hh, body.r, body.b);
            let fixed: f64 = cols.iter().map(|c| c.w).sum();
            let rest = (body.w() as f64 - fixed * s).max(px(140.0) as f64);
            let mut x = body.l;
            lay.col_x.push(x);
            for c in &cols {
                x += if c.w == 0.0 { rest as i32 } else { px(c.w) };
                lay.col_x.push(x);
            }
            lay.vbar = lay.rows_area;
        }
        match ui.page {
            Page::Performance => {
                let pl = px(if ui.mini { 148.0 } else { 268.0 });
                lay.perf_list = R::new(body.l, body.t, body.l + pl, body.b);
                let n = perf_kinds(d).len().max(1) as i32;
                let item_h = px(if ui.mini { 52.0 } else { 76.0 }).min((body.h() - px(16.0)) / n);
                lay.perf_items = (0..n).map(|i| R::new(lay.perf_list.l + px(6.0), body.t + px(8.0) + i * item_h, lay.perf_list.r - px(6.0), body.t + px(8.0) + (i + 1) * item_h)).collect();
                lay.perf_main = R::new(lay.perf_list.r + px(if ui.mini { 8.0 } else { 24.0 }), body.t + px(16.0), body.r - px(if ui.mini { 8.0 } else { 24.0 }), body.b - px(12.0));
            }
            Page::Settings => {
                let ids = [SettingId::Theme, SettingId::Tray, SettingId::Autostart, SettingId::Etw, SettingId::GpuTemp];
                let cw = (body.w() - px(48.0)).min(px(900.0));
                lay.setting_cards = ids.iter().enumerate().map(|(i, id)| (R::new(body.l + px(24.0), body.t + px(16.0) + i as i32 * px(76.0), body.l + px(24.0) + cw, body.t + px(16.0) + i as i32 * px(76.0) + px(66.0)), *id)).collect();
            }
            _ => {}
        }
        lay
    }

    fn content_height(&self, g: &G, ui: &Ui, lay: &Lay, d: &Data) -> f64 {
        match ui.page {
            Page::Insights => {
                let w = (lay.body.w() - g.px(48.0)).min(g.px(900.0));
                d.hints().iter().map(|h| (g.wrap_h(h, Font::Ui, w - g.px(32.0)) + g.px(34.0)) as f64).sum::<f64>() + g.px(32.0) as f64
            }
            _ => ui.rows.len() as f64 * g.px(ROW_H) as f64,
        }
    }

    // ================= рисование =================
    pub fn paint(&self, hwnd: HWND) {
        let (Ok(d), Ok(mut ui)) = (self.data.try_borrow(), self.ui.try_borrow_mut()) else {
            unsafe { InvalidateRect(hwnd, null(), 0) };
            return;
        };
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let (w, h) = (rc.right, rc.bottom);
            if w > 0 && h > 0 {
                let mem = winapi::um::wingdi::CreateCompatibleDC(hdc);
                let bmp = winapi::um::wingdi::CreateCompatibleBitmap(hdc, w, h);
                let old = winapi::um::wingdi::SelectObject(mem, bmp as *mut _);
                let g = G { dc: mem, s: self.scale, th: ui.th, fonts: &self.fonts };
                let lay = self.compute_layout(&g, &ui, &d, w, h);
                // прокрутка не должна выходить за содержимое
                let area_h = if ui.has_table() { lay.rows_area.h() } else { lay.body.h() } as f64;
                let max_scroll = (self.content_height(&g, &ui, &lay, &d) - area_h).max(0.0);
                ui.scroll = ui.scroll.clamp(0.0, max_scroll);
                g.fill(lay.client, ui.th.bg);
                match ui.page {
                    Page::Performance => self.draw_performance(&g, &ui, &d, &lay),
                    Page::Insights => self.draw_insights(&g, &ui, &d, &lay),
                    Page::Settings => self.draw_settings(&g, &ui, &d, &lay),
                    _ => self.draw_table(&g, &ui, &d, &lay),
                }
                self.draw_band(&g, &ui, &d, &lay);
                self.draw_nav(&g, &ui, &lay);
                self.draw_overlay(&g, &ui, lay.client);
                winapi::um::wingdi::BitBlt(hdc, 0, 0, w, h, mem, 0, 0, winapi::um::wingdi::SRCCOPY);
                winapi::um::wingdi::SelectObject(mem, old);
                winapi::um::wingdi::DeleteObject(bmp as *mut _);
                winapi::um::wingdi::DeleteDC(mem);
                if let Some(path) = layout_log_path() {
                    // только для автотестов: где лежат кнопки, шапка и первая строка (физические пиксели клиентской области)
                    let mut t = format!("scale={}\nrow0={:?}\nheader={:?}\n", self.scale, (lay.rows_area.l, lay.rows_area.t, lay.rows_area.r, lay.rows_area.b), (lay.header.l, lay.header.t, lay.header.r, lay.header.b));
                    for (r, b, _, _) in &lay.buttons {
                        t += &format!("{:?}={},{},{},{}\n", b, r.l, r.t, r.r, r.b);
                    }
                    for (i, r) in lay.nav_items.iter().enumerate() {
                        t += &format!("nav{i}={},{},{},{}\n", r.l, r.t, r.r, r.b);
                    }
                    t += &format!("toggle={},{},{},{}\ncols={:?}\n", lay.nav_toggle.l, lay.nav_toggle.t, lay.nav_toggle.r, lay.nav_toggle.b, lay.col_x);
                    let _ = std::fs::write(path, t);
                }
                ui.lay = Some(lay);
            }
            EndPaint(hwnd, &ps);
        }
        drop(ui);
        self.place_search();
    }

    /// Ставит поле поиска в шапку (оно настоящее, а рамка вокруг нарисована).
    pub fn place_search(&self) {
        let ui = self.ui.borrow();
        let Some(lay) = &ui.lay else { return };
        let show = ui.page_has_search() && (!ui.mini || ui.search_open);
        let s = self.scale;
        let r = lay.search;
        if self.search_rect.get() != r || self.search.visible() != show {
            self.search_rect.set(r);
            let lg = |v: i32| (v as f64 / s).round() as i32;
            self.search.set_position(lg(r.l) + 10, lg(r.t) + 7);
            self.search.set_size((lg(r.w()) - 44).max(40) as u32, 20);
            self.search.set_visible(show);
        }
    }

    pub fn invalidate(&self) {
        if let Some(h) = self.win.handle.hwnd() {
            unsafe { InvalidateRect(h, null(), 0) };
        }
    }

    // ================= проверка попаданий =================
    fn hit_test(&self, ui: &Ui, x: i32, y: i32) -> Hit {
        let Some(lay) = &ui.lay else { return Hit::None };
        if lay.nav_toggle.contains(x, y) {
            return Hit::NavToggle;
        }
        if let Some(i) = lay.nav_items.iter().position(|r| r.contains(x, y)) {
            return Hit::Nav(i);
        }
        if lay.band.contains(x, y) {
            return lay.buttons.iter().find(|b| b.0.contains(x, y)).map_or(Hit::None, |b| Hit::Button(b.1));
        }
        match ui.page {
            Page::Performance => lay.perf_items.iter().position(|r| r.contains(x, y)).map_or(Hit::None, Hit::PerfItem),
            Page::Settings => lay.setting_cards.iter().position(|c| c.0.contains(x, y)).map_or(Hit::None, Hit::Setting),
            _ if ui.has_table() => {
                if lay.header.contains(x, y) {
                    let cols = columns(ui.page);
                    return (0..cols.len()).find(|&i| x >= lay.col_x[i] && x < lay.col_x[i + 1]).map_or(Hit::None, Hit::Header);
                }
                if lay.rows_area.contains(x, y) {
                    let content = ui.rows.len() as f64 * (ROW_H * self.scale).round();
                    if content > lay.rows_area.h() as f64 && x >= lay.rows_area.r - (10.0 * self.scale) as i32 {
                        return Hit::VBar;
                    }
                    let i = ((y - lay.rows_area.t) as f64 + ui.scroll) / (ROW_H * self.scale).round();
                    let i = i.floor() as usize;
                    if i < ui.rows.len() {
                        let r = &ui.rows[i];
                        let chev_x = lay.col_x[0] + (12.0 * self.scale) as i32 + r.depth as i32 * (22.0 * self.scale) as i32;
                        if matches!(ui.page, Page::Processes | Page::Users) && r.expandable && x >= chev_x - (4.0 * self.scale) as i32 && x < chev_x + (22.0 * self.scale) as i32 {
                            return Hit::Chevron(i);
                        }
                        return Hit::Row(i);
                    }
                }
                Hit::None
            }
            _ => Hit::None,
        }
    }

    pub fn btn_enabled(&self, ui: &Ui, d: &Data, b: Btn) -> bool {
        let has_sel = ui.sel.is_some();
        match b {
            Btn::Search | Btn::RunTask | Btn::More => true,
            Btn::EndTask | Btn::Efficiency | Btn::SvcStart | Btn::SvcStop | Btn::StartEnable | Btn::StartDisable | Btn::RuleDelete | Btn::BoostOn | Btn::BoostOff => {
                let _ = d;
                has_sel
            }
        }
    }

    // ================= события мыши/клавиатуры =================
    pub fn on_mouse_move(&self, hwnd: HWND, x: i32, y: i32) {
        let mut ui = self.ui.borrow_mut();
        ui.mouse = (x, y);
        let hit = self.hit_test(&ui, x, y);
        let mut redraw = hit != ui.hit;
        ui.hit = hit;
        if let Some((sy, s0)) = ui.drag
            && let Some(lay) = ui.lay.clone() {
                let content = ui.rows.len() as f64 * (ROW_H * self.scale).round();
                let area = lay.rows_area.h() as f64;
                if content > area {
                    let thumb = ((area / content) * area).max(24.0 * self.scale);
                    ui.scroll = (s0 + (y - sy) as f64 * (content - area) / (area - thumb)).clamp(0.0, content - area);
                    redraw = true;
                }
            }
        // подсказка над ячейкой «Подозрительное»: появляется после короткой паузы
        let mut want_tip: Option<(String, (i32, i32))> = None;
        if let (Hit::Row(i), Some(lay)) = (hit, &ui.lay)
            && ui.page == Page::Processes {
                let last = lay.col_x.len() - 2;
                if x >= lay.col_x[last] && x < lay.col_x[last + 1]
                    && let Some(r) = ui.rows.get(i)
                        && !r.tip.is_empty() {
                            want_tip = Some((r.tip.clone(), (x, y)));
                        }
            }
        if want_tip.is_none() && ui.tip.is_some() {
            ui.tip = None;
            redraw = true;
        }
        let pending = want_tip.is_some();
        drop(ui);
        unsafe {
            if pending {
                *self.pending_tip().borrow_mut() = want_tip;
                SetTimer(hwnd, 77, 350, None);
            } else {
                KillTimer(hwnd, 77);
            }
            let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            TrackMouseEvent(&mut t);
        }
        if redraw {
            self.invalidate();
        }
    }

    fn pending_tip(&self) -> &'static RefCell<Option<(String, (i32, i32))>> {
        thread_local! { static P: &'static RefCell<Option<(String, (i32, i32))>> = Box::leak(Box::new(RefCell::new(None))); }
        P.with(|p| *p)
    }

    pub fn on_tip_timer(&self, hwnd: HWND) {
        unsafe { KillTimer(hwnd, 77) };
        let Some(tip) = self.pending_tip().borrow_mut().take() else { return };
        if let Some(log) = std::env::var_os("RTASKMGR_TIP_LOG") {
            let _ = std::fs::write(log, &tip.0); // только для автотестов: какой текст показала бы подсказка
        }
        self.ui.borrow_mut().tip = Some(tip);
        self.invalidate();
    }

    pub fn on_mouse_leave(&self) {
        let mut ui = self.ui.borrow_mut();
        ui.hit = Hit::None;
        ui.tip = None;
        drop(ui);
        self.invalidate();
    }

    pub fn on_lbutton_down(&self, hwnd: HWND, x: i32, y: i32) {
        unsafe { SetFocus(hwnd) };
        let hit = {
            let mut ui = self.ui.borrow_mut();
            ui.tip = None;
            self.hit_test(&ui, x, y)
        };
        match hit {
            Hit::NavToggle => {
                let mut ui = self.ui.borrow_mut();
                ui.nav_open = !ui.nav_open;
            }
            Hit::Nav(i) => self.go_page(NAV[i].0),
            Hit::Button(b) => self.on_button(hwnd, b),
            Hit::Header(i) => {
                {
                    let mut ui = self.ui.borrow_mut();
                    let cols = columns(ui.page);
                    let cur = ui.sort_of(ui.page);
                    let numeric = cols[i].align == Align::Right || cols[i].metric.is_some();
                    let ns = if cur.0 == i { (i, !cur.1) } else { (i, numeric) };
                    let p = ui.page;
                    ui.sorts.insert(p, ns);
                }
                self.rebuild();
            }
            Hit::Chevron(i) => {
                let key = self.ui.borrow().rows.get(i).map(|r| r.key.clone());
                if let Some(k) = key {
                    self.toggle_expand(&k);
                }
            }
            Hit::Row(i) => {
                let mut dbl: Option<String> = None;
                {
                    let mut ui = self.ui.borrow_mut();
                    if let Some(r) = ui.rows.get(i) {
                        let key = r.key.clone();
                        let now = Instant::now();
                        if let Some((t, k)) = &ui.last_click
                            && *k == key && now.duration_since(*t) < Duration::from_millis(450) {
                                dbl = Some(key.clone());
                            }
                        ui.last_click = Some((now, key.clone()));
                        ui.sel = Some(key);
                    }
                }
                if let Some(k) = dbl {
                    self.toggle_expand(&k);
                }
            }
            Hit::VBar => {
                let mut ui = self.ui.borrow_mut();
                ui.drag = Some((y, ui.scroll));
                unsafe { SetCapture(hwnd) };
            }
            Hit::PerfItem(i) => {
                let kind = perf_kinds(&self.data.borrow()).get(i).copied();
                if let Some(k) = kind {
                    self.ui.borrow_mut().perf_sel = k;
                    self.data.borrow_mut().want_gpu_temp = matches!(k, PerfKind::Gpu(_)); // температура сразу, не ждать следующего такта
                }
            }
            Hit::Setting(i) => self.on_setting(i, x),
            Hit::None => {}
        }
        self.invalidate();
    }

    pub fn on_lbutton_up(&self) {
        let mut ui = self.ui.borrow_mut();
        if ui.drag.take().is_some() {
            unsafe { ReleaseCapture() };
        }
    }

    pub fn on_wheel(&self, delta: i16) {
        let mut ui = self.ui.borrow_mut();
        ui.scroll -= delta as f64 / 120.0 * 3.0 * (ROW_H * self.scale).round();
        ui.tip = None;
        drop(ui);
        self.invalidate();
    }

    pub fn on_rbutton_up(&self, hwnd: HWND, x: i32, y: i32) {
        let hit = {
            let ui = self.ui.borrow();
            self.hit_test(&ui, x, y)
        };
        if let Hit::Row(i) | Hit::Chevron(i) = hit {
            {
                let mut ui = self.ui.borrow_mut();
                if let Some(r) = ui.rows.get(i) {
                    ui.sel = Some(r.key.clone());
                }
            }
            self.invalidate();
            let items = self.row_menu();
            let (px, py) = self.to_screen(hwnd, x, y);
            if let Some(a) = self.show_menu(hwnd, &items, px, py) {
                self.do_act(a);
            }
        }
    }

    fn to_screen(&self, hwnd: HWND, x: i32, y: i32) -> (i32, i32) {
        let mut p = winapi::shared::windef::POINT { x, y };
        unsafe { ClientToScreen(hwnd, &mut p) };
        (p.x, p.y)
    }

    pub fn on_key(&self, hwnd: HWND, vk: u32) {
        let ctrl = unsafe { GetKeyState(VK_CONTROL) } < 0;
        let (page, n) = {
            let ui = self.ui.borrow();
            (ui.page, ui.rows.len())
        };
        if ctrl && vk == 'F' as u32 && self.ui.borrow().page_has_search() {
            unsafe { SetFocus(self.search.handle.hwnd().unwrap_or(hwnd)) };
            return;
        }
        if !self.ui.borrow().has_table() || n == 0 {
            return;
        }
        let cur = {
            let ui = self.ui.borrow();
            ui.sel.as_ref().and_then(|k| ui.rows.iter().position(|r| &r.key == k))
        };
        let step = ((self.ui.borrow().lay.as_ref().map_or(10, |l| l.rows_area.h()) as f64 / (ROW_H * self.scale).round()) as usize).max(1);
        let target = match vk as i32 {
            VK_DOWN => Some(cur.map_or(0, |c| (c + 1).min(n - 1))),
            VK_UP => Some(cur.map_or(0, |c| c.saturating_sub(1))),
            VK_NEXT => Some(cur.map_or(0, |c| (c + step).min(n - 1))),
            VK_PRIOR => Some(cur.map_or(0, |c| c.saturating_sub(step))),
            VK_HOME => Some(0),
            VK_END => Some(n - 1),
            VK_RIGHT | VK_LEFT => {
                let key = self.ui.borrow().sel.clone();
                if let Some(k) = key {
                    let want_open = vk as i32 == VK_RIGHT;
                    let open = self.ui.borrow().expanded.contains(&k);
                    if want_open != open && matches!(page, Page::Processes | Page::Users) {
                        self.toggle_expand(&k);
                    }
                }
                None
            }
            VK_DELETE => {
                self.do_act(Act::EndTask);
                None
            }
            VK_F5 => {
                self.tick();
                None
            }
            _ => None,
        };
        if let Some(t) = target {
            let mut ui = self.ui.borrow_mut();
            ui.sel = Some(ui.rows[t].key.clone());
            // держим выбранную строку в видимой области
            if let Some(lay) = ui.lay.clone() {
                let rh = (ROW_H * self.scale).round();
                let (top, bot) = (t as f64 * rh, (t as f64 + 1.0) * rh);
                if top < ui.scroll {
                    ui.scroll = top;
                } else if bot > ui.scroll + lay.rows_area.h() as f64 {
                    ui.scroll = bot - lay.rows_area.h() as f64;
                }
            }
        }
        self.invalidate();
    }

    // ================= страницы, строки =================
    pub fn go_page(&self, p: Page) {
        {
            let mut ui = self.ui.borrow_mut();
            if ui.page == p {
                return;
            }
            ui.page = p;
            ui.scroll = 0.0;
            ui.sel = None;
            ui.search.clear();
            ui.search_open = false;
            ui.rows.clear();
            ui.tip = None;
        }
        self.search.set_text("");
        {
            let mut d = self.data.borrow_mut();
            match p {
                Page::Startup => d.load_startup(),
                Page::History => d.load_history(),
                _ => {}
            }
        }
        self.rebuild();
        self.save_settings();
    }

    pub fn rebuild(&self) {
        let d = self.data.borrow();
        let mut ui = self.ui.borrow_mut();
        if !ui.has_table() {
            return;
        }
        let rows = {
            let ctx = BuildCtx { page: ui.page, search: &ui.search, expanded: &ui.expanded, sort: ui.sort_of(ui.page) };
            build_rows(&d, &ctx)
        };
        if let Some(k) = ui.sel.clone()
            && !rows.iter().any(|r| r.key == k) {
                ui.sel = None;
            }
        ui.rows = rows;
    }

    fn toggle_expand(&self, key: &str) {
        {
            let mut ui = self.ui.borrow_mut();
            if !ui.expanded.remove(key) {
                ui.expanded.insert(key.to_string());
            }
        }
        self.rebuild();
    }

    /// Из таймера: Windows-таймер может тикать чаще заданного, поэтому режем частоту.
    pub fn on_timer(&self) {
        if self.last_tick.get().elapsed() >= Duration::from_millis(900) {
            self.last_tick.set(Instant::now());
            self.tick();
        }
    }

    pub fn tick(&self) {
        {
            let ui = self.ui.borrow();
            self.data.borrow_mut().want_gpu_temp = ui.page == Page::Performance && matches!(ui.perf_sel, PerfKind::Gpu(_));
        }
        let mut notes = self.data.borrow_mut().refresh();
        notes.extend(crate::boost::take_done()); // фоновые действия страницы «Ускорение»
        if !notes.is_empty() {
            let msg = notes.join("; ");
            self.toast(&msg);
            self.tray.show(&msg, Some(t!("Диспетчер задач", "Task Manager")), None, None);
        }
        {
            let (page, ticks) = (self.ui.borrow().page, self.data.borrow().ticks);
            let mut d = self.data.borrow_mut();
            if page == Page::Startup && ticks % 10 == 0 {
                d.load_startup();
            }
            if page == Page::History && ticks % 30 == 0 {
                d.load_history();
            }
        }
        self.rebuild();
        {
            let mut ui = self.ui.borrow_mut();
            if ui.toast.as_ref().is_some_and(|t| t.1.elapsed() > Duration::from_secs(4)) {
                ui.toast = None;
            }
        }
        self.invalidate();
    }

    pub fn toast(&self, msg: &str) {
        self.ui.borrow_mut().toast = Some((msg.to_string(), Instant::now()));
        self.invalidate();
    }

    pub fn on_search_changed(&self) {
        let t = self.search.text();
        {
            let mut ui = self.ui.borrow_mut();
            ui.search = t;
            ui.scroll = 0.0;
        }
        self.rebuild();
        self.invalidate();
    }

    // ================= кнопки, настройки =================
    fn on_button(&self, hwnd: HWND, b: Btn) {
        match b {
            Btn::Search => {
                let open = { let mut u = self.ui.borrow_mut(); u.search_open = !u.search_open; u.search_open };
                self.place_search();
                if open { self.search.set_focus(); }
                self.invalidate();
            }
            Btn::RunTask => self.do_act(Act::RunTask),
            Btn::EndTask => self.do_act(Act::EndTask),
            Btn::Efficiency => self.do_act(Act::Efficiency),
            Btn::SvcStart => self.do_act(Act::SvcStart),
            Btn::SvcStop => self.do_act(Act::SvcStop),
            Btn::StartEnable => self.do_act(Act::StartEnable),
            Btn::StartDisable => self.do_act(Act::StartDisable),
            Btn::RuleDelete => self.do_act(Act::RuleDelete),
            Btn::BoostOn => self.do_act(Act::BoostOn),
            Btn::BoostOff => self.do_act(Act::BoostOff),
            Btn::More => {
                let items = self.more_menu();
                let r = self.ui.borrow().lay.as_ref().and_then(|l| l.buttons.iter().find(|x| x.1 == Btn::More).map(|x| x.0));
                if let Some(r) = r {
                    let (x, y) = self.to_screen(hwnd, r.r - 220, r.b + 2);
                    if let Some(a) = self.show_menu(hwnd, &items, x, y) {
                        self.do_act(a);
                    }
                }
            }
        }
    }

    fn on_setting(&self, i: usize, x: i32) {
        let id = self.ui.borrow().lay.as_ref().and_then(|l| l.setting_cards.get(i).map(|c| (c.1, c.0)));
        let Some((id, r)) = id else { return };
        match id {
            SettingId::Theme => {
                let ctl_l = r.r - (200.0 * self.scale) as i32;
                let seg = ((x - ctl_l) / (((200.0 * self.scale) as i32 - (16.0 * self.scale) as i32) / 3).max(1)).clamp(0, 2);
                self.do_act(Act::SetTheme(seg as u8));
            }
            SettingId::Tray => self.do_act(Act::ToggleTray),
            SettingId::Autostart => self.do_act(Act::ToggleAutostart),
            SettingId::Etw => self.do_act(Act::ToggleEtw),
            SettingId::GpuTemp => self.do_act(Act::ToggleGpuTemp),
        }
    }

    // ================= меню =================
    fn selected_row(&self) -> Option<Row> {
        let ui = self.ui.borrow();
        ui.sel.as_ref().and_then(|k| ui.rows.iter().find(|r| &r.key == k).cloned())
    }

    fn row_menu(&self) -> Vec<MI> {
        let page = self.ui.borrow().page;
        match page {
            Page::Processes | Page::Users | Page::Details => {
                let mut v = vec![];
                if let Some(r) = self.selected_row()
                    && r.expandable {
                        v.push(MI::Item(if r.expanded { t!("Свернуть", "Collapse") } else { t!("Развернуть", "Expand") }, Act::Toggle));
                        v.push(MI::Sep);
                    }
                v.extend([
                    MI::Item(t!("Снять задачу", "End task"), Act::EndTask),
                    MI::Item(t!("Завершить дерево процессов", "End process tree"), Act::EndTree),
                    MI::Item(t!("Режим эффективности", "Efficiency mode"), Act::Efficiency),
                    MI::Item(t!("Приостановить", "Suspend"), Act::Suspend(true)),
                    MI::Item(t!("Возобновить", "Resume"), Act::Suspend(false)),
                    MI::Sep,
                    MI::Sub(t!("Приоритет", "Priority"), sys::PRIORITIES.iter().map(|p| MI::Item(p.0, Act::Prio(p.1))).collect()),
                    MI::Sub(t!("Ядра процессора", "CPU cores"), vec![MI::Item(t!("Все ядра", "All cores"), Act::Affinity(false)), MI::Item(t!("Только половина ядер", "Half of the cores only"), Act::Affinity(true))]),
                    MI::Sep,
                    MI::Item(t!("Открыть расположение файла", "Open file location"), Act::OpenDir),
                    MI::Item(t!("Копировать командную строку", "Copy command line"), Act::CopyCmd),
                    MI::Sep,
                    MI::Item(t!("Правило: понижать приоритет при ЦП > 25% дольше 60 с", "Rule: lower priority when CPU > 25% for over 60 s"), Act::RuleLower),
                    MI::Item(t!("Правило: уведомлять при ЦП > 25% дольше 60 с", "Rule: notify when CPU > 25% for over 60 s"), Act::RuleNotify),
                ]);
                v
            }
            Page::Services => vec![MI::Item(t!("Запустить службу", "Start service"), Act::SvcStart), MI::Item(t!("Остановить службу", "Stop service"), Act::SvcStop)],
            Page::Startup => vec![MI::Item(t!("Включить", "Enable"), Act::StartEnable), MI::Item(t!("Отключить", "Disable"), Act::StartDisable), MI::Sep, MI::Item(t!("Открыть расположение файла", "Open file location"), Act::OpenDir)],
            Page::Rules => vec![MI::Item(t!("Удалить правило", "Delete rule"), Act::RuleDelete)],
            Page::Boost => vec![MI::Item(t!("Включить", "Enable"), Act::BoostOn), MI::Item(t!("Выключить", "Disable"), Act::BoostOff)],
            _ => vec![],
        }
    }

    fn more_menu(&self) -> Vec<MI> {
        let d = self.data.borrow();
        let mini = self.ui.borrow().mini;
        vec![
            MI::Item(t!("Запустить новую задачу", "Run new task"), Act::RunTask),
            MI::Item(t!("Экспорт в CSV", "Export to CSV"), Act::Export),
            MI::Sep,
            MI::Check(t!("Компактный режим", "Compact mode"), Act::ToggleMini, mini),
            MI::Sub(t!("Тема", "Theme"), vec![MI::Check(t!("Светлая", "Light"), Act::SetTheme(0), d.cfg.theme == 0), MI::Check(t!("Тёмная", "Dark"), Act::SetTheme(1), d.cfg.theme == 1), MI::Check(t!("Системная", "System"), Act::SetTheme(2), d.cfg.theme == 2)]),
            MI::Item(t!("Параметры", "Settings"), Act::Go(Page::Settings)),
        ]
        .into_iter()
        .chain((!d.is_admin).then_some([MI::Sep, MI::Item(t!("Перезапустить от имени администратора", "Restart as administrator"), Act::RelaunchAdmin)]).into_iter().flatten())
        .collect()
    }

    pub fn tray_menu(&self) -> Vec<MI> {
        let d = self.data.borrow();
        let mini = self.ui.borrow().mini;
        vec![
            MI::Item(t!("Показать окно", "Show window"), Act::ShowWindow),
            MI::Sep,
            MI::Check(t!("Компактный режим", "Compact mode"), Act::ToggleMini, mini),
            MI::Check(t!("Закрытие окна сворачивает в трей", "Closing the window minimizes to tray"), Act::ToggleTray, d.cfg.to_tray),
            MI::Check(t!("Запускать при входе в Windows", "Start with Windows"), Act::ToggleAutostart, startup::self_autostart()),
            MI::Check(t!("Сеть по процессам (ETW, администратор)", "Per-process network (ETW, administrator)"), Act::ToggleEtw, d.etw.is_some()),
            MI::Sep,
        ]
        .into_iter()
        .chain((!d.is_admin).then_some(MI::Item(t!("Перезапустить от имени администратора", "Restart as administrator"), Act::RelaunchAdmin)))
        .chain([MI::Item(t!("Выход", "Exit"), Act::Exit)])
        .collect()
    }

    pub fn show_menu(&self, hwnd: HWND, items: &[MI], x: i32, y: i32) -> Option<Act> {
        if items.is_empty() {
            return None;
        }
        set_menu_theme(self.ui.borrow().th.dark);
        let mut acts: Vec<Act> = vec![];
        let menu = build_menu(items, &mut acts);
        let id = unsafe {
            SetForegroundWindow(hwnd);
            TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, x, y, hwnd, null_mut())
        };
        unsafe { DestroyMenu(menu) };
        (id > 0).then(|| acts[id as usize - 1])
    }

    // ================= действия =================
    pub fn do_act(&self, act: Act) {
        match act {
            Act::ShowWindow => return self.show_window(),
            Act::Exit => return self.quit(),
            Act::RelaunchAdmin => {
                self.save_settings(); // the new instance reads them at start: same page, same columns
                if sys::relaunch_as_admin("") {
                    return self.quit();
                }
                return self.toast(t!("Запуск от имени администратора отменён", "Running as administrator was cancelled"));
            }
            Act::Go(p) => return self.go_page(p),
            Act::RunTask => {
                // стандартное окно «Выполнить»
                let _ = std::process::Command::new("explorer.exe").arg("shell:::{2559a1f3-21d7-11d4-bdaf-00c04f60b9f0}").spawn();
                return;
            }
            Act::SetTheme(t) => {
                self.data.borrow_mut().cfg.theme = t;
                self.apply_theme();
                self.save_settings();
                return;
            }
            Act::ToggleTray => {
                let on = {
                    let mut d = self.data.borrow_mut();
                    d.cfg.to_tray = !d.cfg.to_tray;
                    d.cfg.to_tray
                };
                self.toast(if on { t!("Закрытие окна будет сворачивать в трей", "Closing the window will minimize to tray") } else { t!("Закрытие окна завершит программу", "Closing the window will quit the program") });
                self.save_settings();
                return;
            }
            Act::ToggleMini => {
                let old = self.win.size();
                let mini = {
                    let mut u = self.ui.borrow_mut();
                    u.mini = !u.mini;
                    u.search_open = false;
                    u.mini
                };
                {
                    let mut d = self.data.borrow_mut();
                    if mini { d.cfg.full_size = old; } else { d.cfg.mini_size = old; }
                    d.cfg.mini = mini;
                }
                let size = {
                    let d = self.data.borrow();
                    if mini { d.cfg.mini_size } else { d.cfg.full_size }
                };
                self.win.set_size(size.0, size.1);
                self.search.set_visible(false);
                self.invalidate();
                self.save_settings();
                return;
            }
            Act::ToggleAutostart => {
                let want = !startup::self_autostart();
                let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
                let ok = startup::set_self_autostart(want, &exe);
                self.toast(match (ok, startup::self_autostart()) {
                    (true, true) => t!("Программа будет запускаться при входе в Windows (сразу в трей)", "The program will start with Windows (straight to the tray)"),
                    (true, false) => t!("Автозапуск программы отключён", "Autostart is disabled"),
                    _ => t!("Не удалось изменить автозапуск", "Could not change autostart"),
                });
                return;
            }
            Act::ToggleEtw => {
                let msg = {
                    let mut d = self.data.borrow_mut();
                    if !d.is_admin {
                        t!("Сеть по процессам требует прав администратора — запустите программу от имени администратора", "Per-process network needs administrator rights - run the program as administrator").to_string()
                    } else if let Some(s) = d.etw.take() {
                        crate::net::stop(s);
                        d.net.clear();
                        d.cfg.etw = false;
                        t!("Сеть по процессам выключена", "Per-process network is off").into()
                    } else {
                        d.etw = crate::net::start();
                        d.cfg.etw = d.etw.is_some();
                        if d.etw.is_some() { t!("Сеть по процессам включена (колонка «Сеть»)", "Per-process network is on ('Network' column)").into() } else { t!("Не удалось запустить ETW", "Could not start ETW").into() }
                    }
                };
                self.toast(&msg);
                self.save_settings();
                return;
            }
            Act::ToggleGpuTemp => {
                let on = {
                    let mut d = self.data.borrow_mut();
                    d.cfg.gpu_temp_always = !d.cfg.gpu_temp_always;
                    d.cfg.gpu_temp_always
                };
                self.toast(if on { t!("Температура GPU читается постоянно", "GPU temperature is read continuously") } else { t!("Температура GPU читается только на странице GPU", "GPU temperature is read only on the GPU page") });
                self.save_settings();
                return;
            }
            Act::Export => return self.export(),
            _ => {}
        }
        let page = self.ui.borrow().page;
        let Some(row) = self.selected_row() else {
            self.toast(t!("Сначала выберите строку в списке", "Select a row in the list first"));
            return;
        };
        if act == Act::Toggle {
            return self.toggle_expand(&row.key);
        }
        match page {
            Page::Services => return self.act_service(&row, act),
            Page::Startup => return self.act_startup(&row, act),
            Page::Boost => return self.act_boost(&row, act),
            Page::Rules => {
                if act == Act::RuleDelete {
                    if let Ok(i) = row.key.parse::<usize>() {
                        let mut d = self.data.borrow_mut();
                        if i < d.rules.len() {
                            d.rules.remove(i);
                        }
                    }
                    self.save_settings();
                    self.toast(t!("Правило удалено", "Rule deleted"));
                    self.rebuild();
                }
                return;
            }
            _ => {}
        }
        self.act_process(&row, act);
    }

    fn act_process(&self, row: &Row, act: Act) {
        let mut pids: Vec<u32> = row.pids.iter().copied().filter(|&p| p != 0).collect();
        if pids.is_empty() {
            return;
        }
        if matches!(act, Act::EndTask | Act::EndTree | Act::Suspend(true)) {
            let d = self.data.borrow();
            let total = pids.len();
            pids.retain(|&p| d.sys.process(sysinfo::Pid::from_u32(p)).is_none_or(|x| !CRITICAL.contains(&x.name().to_string_lossy().to_lowercase().as_str())));
            if pids.len() < total {
                drop(d);
                self.toast(t!("Критичные системные процессы (csrss, lsass, smss и т.п.) защищены и пропущены", "Critical system processes (csrss, lsass, smss, etc.) are protected and skipped"));
                if pids.is_empty() {
                    return;
                }
            }
        }
        if (act == Act::EndTask && pids.len() > 1 || act == Act::EndTree) && !self.confirm(&tf!("Завершить процессов: {} (для дерева — вместе с потомками)?", "End {} processes (for a tree, together with their children)?", pids.len())) {
            return;
        }
        let report = |what: &str, ok: usize| {
            self.toast(&if ok == pids.len() { tf!("{what}: готово ({ok})", "{what}: done ({ok})") } else { tf!("{what}: удалось {ok} из {} — для остальных нужны права администратора{}", "{what}: {ok} of {} succeeded - the rest need administrator rights{}", pids.len(), if self.data.borrow().is_admin { "" } else { t!(" (меню «…» → «Перезапустить от имени администратора»)", " (menu '…' → 'Restart as administrator')") }) });
        };
        match act {
            Act::EndTask => {
                let d = self.data.borrow();
                let ok = pids.iter().filter(|&&p| d.sys.process(sysinfo::Pid::from_u32(p)).is_some_and(|x| x.kill())).count();
                drop(d);
                report(t!("Завершение", "Ending"), ok);
            }
            Act::EndTree => {
                let d = self.data.borrow();
                let parents: HashMap<u32, u32> = d.procs.iter().map(|p| (p.pid, p.ppid)).collect();
                let all = descendants(&parents, &pids);
                let ok = all.iter().filter(|&&p| d.sys.process(sysinfo::Pid::from_u32(p)).is_some_and(|x| x.kill())).count();
                drop(d);
                self.toast(&tf!("Завершено {ok} из {} (с потомками)", "Ended {ok} of {} (with children)", all.len()));
            }
            Act::Prio(c) => report(t!("Приоритет", "Priority"), pids.iter().filter(|&&p| sys::set_priority(p, c)).count()),
            Act::Suspend(on) => {
                let ok: Vec<u32> = pids.iter().copied().filter(|&p| sys::suspend(p, on)).collect();
                {
                    let mut d = self.data.borrow_mut();
                    for p in &ok {
                        if on { d.suspended.insert(*p) } else { d.suspended.remove(p) };
                    }
                }
                report(if on { t!("Приостановка", "Suspending") } else { t!("Возобновление", "Resuming") }, ok.len());
            }
            Act::Efficiency => {
                let on = !pids.iter().all(|p| self.data.borrow().efficiency.contains(p));
                let ok: Vec<u32> = pids.iter().copied().filter(|&p| sys::set_efficiency(p, on)).collect();
                {
                    let mut d = self.data.borrow_mut();
                    for p in &ok {
                        if on { d.efficiency.insert(*p) } else { d.efficiency.remove(p) };
                    }
                }
                report(if on { t!("Режим эффективности включён", "Efficiency mode on") } else { t!("Режим эффективности выключен", "Efficiency mode off") }, ok.len());
            }
            Act::Affinity(half) => report(t!("Привязка к ядрам", "Core affinity"), pids.iter().filter(|&&p| sys::set_affinity(p, half)).count()),
            Act::OpenDir => {
                let d = self.data.borrow();
                let path = pids.iter().find_map(|&p| d.procs.iter().find(|x| x.pid == p).and_then(|x| x.exe.clone()));
                drop(d);
                match path {
                    Some(p) => {
                        use std::os::windows::process::CommandExt;
                        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", p.replace("\\..\\", "\\"))).spawn();
                    }
                    None => self.toast(t!("Путь к файлу недоступен (системный или защищённый процесс)", "File path unavailable (system or protected process)")),
                }
            }
            Act::CopyCmd => {
                let d = self.data.borrow();
                let cmd = process_cmd(&d, pids[0]);
                drop(d);
                if cmd.is_empty() {
                    self.toast(t!("Командная строка недоступна для этого процесса", "Command line unavailable for this process"));
                } else {
                    nwg::Clipboard::set_data_text(&self.win, &cmd);
                    self.toast(t!("Командная строка скопирована", "Command line copied"));
                }
            }
            Act::RuleLower | Act::RuleNotify => {
                let name = {
                    let d = self.data.borrow();
                    d.procs.iter().find(|p| p.pid == pids[0]).map(|p| p.name.to_lowercase())
                };
                let Some(name) = name else { return };
                let act = if act == Act::RuleLower { store::RuleAct::Lower } else { store::RuleAct::Notify };
                let msg = {
                    let mut d = self.data.borrow_mut();
                    if d.rules.iter().any(|r| r.proc == name && r.act == act) {
                        t!("Такое правило уже есть (страница «Правила»)", "This rule already exists ('Rules' page)").to_string()
                    } else {
                        d.rules.push(store::Rule { proc: name.clone(), cpu: 25.0, secs: 60, act, hits: 0 });
                        tf!("Правило добавлено: «{name}» — ЦП > 25% дольше 60 с", "Rule added: '{name}' - CPU > 25% for over 60 s")
                    }
                };
                self.save_settings();
                self.toast(&msg);
            }
            _ => {}
        }
    }

    fn act_service(&self, row: &Row, act: Act) {
        let start = match act {
            Act::SvcStart => true,
            Act::SvcStop => false,
            _ => return,
        };
        let n = row.aux.clone();
        self.toast(&match sys::service_ctl(&n, start) {
            Ok(()) => tf!("Служба {n}: команда отправлена", "Service {n}: command sent"),
            Err(5) => tf!("Служба {n}: отказано в доступе — запустите от администратора", "Service {n}: access denied - run as administrator"),
            Err(e) => tf!("Служба {n}: ошибка Win32 {e}", "Service {n}: Win32 error {e}"),
        });
    }

    fn act_startup(&self, row: &Row, act: Act) {
        let on = match act {
            Act::StartEnable => true,
            Act::StartDisable => false,
            Act::OpenDir => {
                if let Some(p) = &row.icon_path {
                    use std::os::windows::process::CommandExt;
                    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{p}\"")).spawn();
                }
                return;
            }
            _ => return,
        };
        let msg = {
            let d = self.data.borrow();
            match d.startup.iter().find(|s| s.key() == row.key) {
                Some(it) => match startup::set_enabled(it, on) {
                    Ok(()) => format!("«{}»: {}", it.name, if on { t!("включено", "enabled") } else { t!("отключено — не будет запускаться при входе в Windows", "disabled - will not start with Windows") }),
                    Err(5) => tf!("«{}»: нужны права администратора", "'{}': administrator rights needed", it.name),
                    Err(e) => tf!("«{}»: ошибка Win32 {e}", "'{}': Win32 error {e}", it.name),
                },
                None => return,
            }
        };
        self.data.borrow_mut().load_startup();
        self.rebuild();
        self.toast(&msg);
    }

    fn act_boost(&self, row: &Row, act: Act) {
        let on = match act {
            Act::BoostOn => true,
            Act::BoostOff => false,
            _ => return,
        };
        let Some(id) = crate::boost::Id::from_key(&row.key) else { return };
        if on && id == crate::boost::Id::Defender && !self.confirm(t!("Defender перестанет проверять кэши сборки и библиотеки игр (cargo, npm, pip, Steam, Epic…). Вредоносный файл в этих папках не будет найден при открытии. Включить?", "Defender will stop scanning build caches and game libraries (cargo, npm, pip, Steam, Epic…). A malicious file in those folders will not be caught when opened. Enable?")) {
            return;
        }
        // донор токена для программ, которые профиль запустит обратно: sihost.exe работает в каждом сеансе без повышения
        let donors: Vec<u32> = self.data.borrow().procs.iter().filter(|p| p.name.eq_ignore_ascii_case("sihost.exe")).map(|p| p.pid).collect();
        match crate::boost::start(id, on, donors) {
            Ok(()) => self.toast(&tf!("{}: выполняется…", "{}: running…", id.title())),
            Err(e) => self.toast(&format!("{}: {e}", id.title())),
        }
        self.rebuild();
    }

    fn confirm(&self, text: &str) -> bool {
        let p = nwg::MessageParams { title: t!("Подтверждение", "Confirmation"), content: text, buttons: nwg::MessageButtons::YesNo, icons: nwg::MessageIcons::Warning };
        nwg::modal_message(&self.win, &p) == nwg::MessageChoice::Yes
    }

    fn export(&self) {
        let ui = self.ui.borrow();
        if !ui.has_table() {
            drop(ui);
            return self.toast(t!("На этой странице нечего экспортировать", "Nothing to export on this page"));
        }
        let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let mut out = String::from("\u{feff}");
        out += &columns(ui.page).iter().map(|c| esc(c.title)).collect::<Vec<_>>().join(";");
        for r in &ui.rows {
            out += "\r\n";
            out += &r.cells.iter().map(|c| esc(c)).collect::<Vec<_>>().join(";");
        }
        let (y, mo, d, h, mi, s) = sys::local_time();
        let dir = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_default().join("Desktop");
        let path = dir.join(format!("rtaskmgr-{}-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}.csv", page_title(ui.page)));
        drop(ui);
        match std::fs::write(&path, out) {
            Ok(()) => self.toast(&tf!("Сохранено: {}", "Saved: {}", path.display())),
            Err(e) => self.toast(&tf!("Не удалось сохранить CSV: {e}", "Could not save CSV: {e}")),
        }
    }

    // ================= окно, тема, настройки =================
    pub fn apply_theme(&self) {
        let dark = {
            let d = self.data.borrow();
            match d.cfg.theme {
                0 => false,
                1 => true,
                _ => sys::system_dark(),
            }
        };
        let th = theme(dark);
        self.ui.borrow_mut().th = th;
        if let Some(h) = self.win.handle.hwnd() {
            sys::set_dark_titlebar(h, dark);
        }
        unsafe {
            let old = self.edit_brush.replace(winapi::um::wingdi::CreateSolidBrush(th.panel.c()) as isize);
            if old != 0 {
                winapi::um::wingdi::DeleteObject(old as _);
            }
        }
        set_menu_theme(dark);
        self.invalidate();
        if let Some(h) = self.search.handle.hwnd() {
            unsafe { InvalidateRect(h, null(), 1) };
        }
    }

    pub fn save_settings(&self) {
        let ui = self.ui.borrow();
        let mut d = self.data.borrow_mut();
        d.cfg.view = NAV.iter().position(|n| n.0 == ui.page).unwrap_or(0);
        let (sc, sd) = ui.sort_of(Page::Processes);
        d.cfg.sort = sc;
        d.cfg.desc = sd;
        d.cfg.rules = d.rules.clone();
        d.cfg.etw = d.etw.is_some();
        d.cfg.mini = ui.mini;
        let (p, s) = (self.win.position(), self.win.size());
        if p.0 > -10000 && s.0 > 100 {
            d.cfg.pos = p;
            d.cfg.size = s;
            if ui.mini { d.cfg.mini_size = s; } else { d.cfg.full_size = s; }
        }
        store::save(&d.cfg);
    }

    pub fn show_window(&self) {
        self.win.set_visible(true);
        if let Some(h) = self.win.handle.hwnd() {
            unsafe {
                ShowWindow(h, SW_RESTORE);
                SetForegroundWindow(h);
            }
        }
    }

    pub fn quit(&self) {
        self.save_settings();
        if let Some(s) = self.data.borrow_mut().etw.take() {
            crate::net::stop(s);
        }
        self.tray.set_visibility(false);
        nwg::stop_thread_dispatch();
    }
}

/// pid + все его потомки (по таблице pid -> родитель).
pub fn descendants(parent_of: &HashMap<u32, u32>, roots: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = roots.to_vec();
    let mut i = 0;
    while i < out.len() {
        let cur = out[i];
        for (&pid, &par) in parent_of {
            if par == cur && !out.contains(&pid) {
                out.push(pid);
            }
        }
        i += 1;
    }
    out
}

fn build_menu(items: &[MI], acts: &mut Vec<Act>) -> HMENU {
    unsafe {
        let m = CreatePopupMenu();
        for it in items {
            match it {
                MI::Item(t, a) => {
                    acts.push(*a);
                    AppendMenuW(m, MF_STRING, acts.len(), wide(t).as_ptr());
                }
                MI::Check(t, a, on) => {
                    acts.push(*a);
                    AppendMenuW(m, MF_STRING | if *on { MF_CHECKED } else { 0 }, acts.len(), wide(t).as_ptr());
                }
                MI::Sep => {
                    AppendMenuW(m, MF_SEPARATOR, 0, null());
                }
                MI::Sub(t, sub) => {
                    let h = build_menu(sub, acts);
                    AppendMenuW(m, MF_POPUP, h as usize, wide(t).as_ptr());
                }
            }
        }
        m
    }
}

/// Тёмные/светлые системные меню (недокументированные функции uxtheme: 135 — SetPreferredAppMode, 136 — FlushMenuThemes).
pub fn set_menu_theme(dark: bool) {
    use winapi::um::libloaderapi::{GetProcAddress, LoadLibraryW};
    unsafe {
        let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if lib.is_null() {
            return;
        }
        let set = GetProcAddress(lib, 135 as *const i8);
        let flush = GetProcAddress(lib, 136 as *const i8);
        if !set.is_null() {
            let f: extern "system" fn(i32) -> i32 = std::mem::transmute(set);
            f(if dark { 2 } else { 3 }); // ForceDark / ForceLight
        }
        if !flush.is_null() {
            let f: extern "system" fn() = std::mem::transmute(flush);
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_collects_all_once() {
        let parents = HashMap::from([(2, 1), (3, 2), (4, 1), (9, 8)]);
        let mut got = descendants(&parents, &[1]);
        got.sort();
        assert_eq!(got, vec![1, 2, 3, 4]);
        assert_eq!(descendants(&parents, &[9]), vec![9]);
    }
}
