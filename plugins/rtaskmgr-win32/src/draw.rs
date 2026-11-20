//! Отрисовка страниц: боковая панель, шапка, таблицы, «Производительность», «Что тормозит», «Параметры».
use crate::{app::*, data::*, model::*, ui::*};
use std::collections::{HashMap, VecDeque};
use winapi::shared::windef::POINT;

pub const ROW_H: f64 = 28.0;
pub const NAV_W: f64 = 48.0;
pub const NAV_W_OPEN: f64 = 220.0;
pub const BAND_H: f64 = 52.0;
pub const MINI_BAND_H: f64 = 54.0;
pub const MINI_NAV_H: f64 = 40.0;

pub fn series_color(i: usize, dark: bool) -> Rgb {
    let c = [Rgb(10, 132, 255), Rgb(94, 92, 230), Rgb(48, 209, 88), Rgb(255, 159, 10), Rgb(191, 90, 242)][i.min(4)]; // Apple system blue, indigo, green, orange, purple
    if dark { c } else { c.lerp(Rgb(0, 0, 0), 0.18) }
}

/// График: сетка, заливка под линией и сама линия; самое новое значение — справа.
pub fn draw_graph(g: &G, r: R, series: &[(&VecDeque<f32>, Rgb)], ymax: f32, grid: bool) {
    let th = g.th;
    g.fill(r, th.bg.lerp(if th.dark { Rgb(0, 0, 0) } else { Rgb(255, 255, 255) }, 0.12));
    if grid {
        let (cols, rows) = (20, 10);
        let gc = th.border.lerp(th.bg, 0.25);
        for i in 1..cols {
            g.vline(r.l + r.w() * i / cols, r.t, r.b, gc);
        }
        for i in 1..rows {
            g.hline(r.l, r.r, r.t + r.h() * i / rows, gc);
        }
    }
    g.clip(r.inset(1));
    for (data, color) in series {
        let n = data.len();
        if n < 2 {
            continue;
        }
        let pt = |i: usize, v: f32| POINT {
            x: r.r - ((n - 1 - i) as i32) * r.w() / (CAP as i32 - 1),
            y: r.b - 1 - ((v / ymax.max(0.001)).clamp(0.0, 1.0) * (r.h() - 2) as f32) as i32,
        };
        let pts: Vec<POINT> = data.iter().enumerate().map(|(i, &v)| pt(i, v)).collect();
        let mut poly = pts.clone();
        poly.push(POINT { x: pts[n - 1].x, y: r.b });
        poly.push(POINT { x: pts[0].x, y: r.b });
        g.polygon(&poly, th.bg.lerp(*color, 0.28));
        g.polyline(&pts, *color, g.px(1.5).max(1));
    }
    g.unclip();
    let first = series.first().map_or(th.border, |s| s.1);
    g.frame(r, th.bg.lerp(first, 0.75));
}

impl App {
    // ---------------- боковая панель ----------------
    pub fn draw_nav(&self, g: &G, ui: &Ui, lay: &Lay) {
        let th = g.th;
        if ui.mini {
            g.fill(lay.nav, th.nav);
            g.hline(lay.nav.l, lay.nav.r, lay.nav.t, th.border);
            for (i, r) in lay.nav_items.iter().enumerate() {
                let (page, icon, _) = NAV[i];
                let sel = page == ui.page;
                let hov = ui.hit == Hit::Nav(i);
                if sel || hov { g.round(r.inset(g.px(2.0)), g.px(4.0), Some(if sel { th.sel } else { th.hover }), None); }
                g.glyph(*r, icon, if sel { th.accent } else { th.text }, false);
            }
            return;
        }
        g.fill(lay.nav, th.nav);
        g.glyph(lay.nav_toggle, '\u{E700}', th.text, false);
        if ui.hit == Hit::NavToggle {
            g.round(lay.nav_toggle.inset(g.px(2.0)), g.px(4.0), Some(th.hover), None);
            g.glyph(lay.nav_toggle, '\u{E700}', th.text, false);
        }
        for (i, r) in lay.nav_items.iter().enumerate() {
            let (page, icon, label) = NAV[i];
            let sel = page == ui.page;
            let hov = ui.hit == Hit::Nav(i);
            if sel || hov {
                g.round(r.inset(g.px(3.0)), g.px(5.0), Some(if sel { th.sel } else { th.hover }), None);
            }
            if sel {
                g.round(R::new(r.l + g.px(3.0), r.t + r.h() / 2 - g.px(8.0), r.l + g.px(6.0), r.t + r.h() / 2 + g.px(8.0)), g.px(2.0), Some(th.accent), None);
            }
            let ir = R::new(r.l, r.t, r.l + g.px(NAV_W), r.b);
            g.glyph(ir, icon, if sel { th.accent } else { th.text }, false);
            if ui.nav_open {
                g.text(R::new(r.l + g.px(NAV_W), r.t, r.r - g.px(8.0), r.b), label, Font::Ui, th.text);
            }
        }
    }

    // ---------------- шапка страницы ----------------
    pub fn draw_band(&self, g: &G, ui: &Ui, d: &Data, lay: &Lay) {
        let th = g.th;
        g.fill(lay.band, th.bg);
        if ui.mini {
            for (r, btn, label, glyph) in &lay.buttons {
                let enabled = self.btn_enabled(ui, d, *btn);
                let hov = enabled && ui.hit == Hit::Button(*btn);
                if hov { g.round(*r, g.px(4.0), Some(th.hover), None); }
                let c = if enabled { th.text } else { th.dim.lerp(th.bg, 0.35) };
                let top = R::new(r.l, r.t + g.px(2.0), r.r, r.t + g.px(25.0));
                let bot = R::new(r.l, r.t + g.px(25.0), r.r, r.b - g.px(1.0));
                g.glyph(top, *glyph, if enabled && *btn == Btn::Efficiency { Rgb(48, 209, 88) } else { c }, true);
                if !label.is_empty() { g.text_center(bot, label, Font::Small, c); }
            }
            if ui.search_open {
                g.round(lay.search, g.px(5.0), Some(th.panel), Some(th.accent.lerp(th.panel, 0.5)));
            }
            g.hline(lay.band.l, lay.band.r, lay.band.b - 1, th.border);
            return;
        }
        g.text(lay.title, page_title(ui.page), Font::Title, th.text);
        if ui.page_has_search() {
            g.round(lay.search, g.px(5.0), Some(th.panel), Some(th.border));
            g.hline(lay.search.l + g.px(5.0), lay.search.r - g.px(5.0), lay.search.b - 1, th.accent.lerp(th.panel, 0.4));
            g.glyph(R::new(lay.search.r - g.px(32.0), lay.search.t, lay.search.r - g.px(4.0), lay.search.b), '\u{E721}', th.dim, true);
        }
        for (r, btn, label, glyph) in &lay.buttons {
            let enabled = self.btn_enabled(ui, d, *btn);
            let hov = enabled && ui.hit == Hit::Button(*btn);
            if hov {
                g.round(*r, g.px(5.0), Some(th.hover), None);
            }
            let c = if enabled { th.text } else { th.dim.lerp(th.bg, 0.35) };
            let gr = R::new(r.l + g.px(8.0), r.t, r.l + g.px(30.0), r.b);
            g.glyph(gr, *glyph, if enabled && *btn == Btn::Efficiency { Rgb(48, 209, 88) } else { c }, true);
            if !label.is_empty() {
                g.text(R::new(r.l + g.px(32.0), r.t, r.r - g.px(8.0), r.b), label, Font::Ui, c);
            }
        }
        g.hline(lay.band.l, lay.band.r, lay.band.b - 1, th.border);
    }

    // ---------------- таблица ----------------
    pub fn draw_table(&self, g: &G, ui: &Ui, d: &Data, lay: &Lay) {
        let th = g.th;
        let page = ui.page;
        let cols = columns(page);
        let xs = &lay.col_x;
        let area = lay.rows_area;

        // шапка
        let sort = ui.sort_of(page);
        for (i, c) in cols.iter().enumerate() {
            let cell = R::new(xs[i], lay.header.t, xs[i + 1], lay.header.b);
            let mut fill = None;
            if let Some(m) = c.metric {
                let agg = agg_value(d, m);
                fill = Some(heat(&th, agg, 100.0));
            }
            if let Some(f) = fill {
                g.fill(cell, f);
            }
            let hov = ui.hit == Hit::Header(i);
            if hov || sort.0 == i {
                g.fill(cell, if th.dark { th.heat1.lerp(th.bg, 0.45).lerp(fill.unwrap_or(th.bg), 0.5) } else { th.hover });
                if let Some(f) = fill.filter(|_| sort.0 != i) {
                    g.fill(cell, f.lerp(th.text, 0.06));
                }
            }
            let inner = cell.inset(g.px(8.0));
            let big = page == Page::Processes && !ui.mini;
            if let Some(m) = c.metric {
                let val = agg_text(d, m);
                if big {
                    let top = R::new(inner.l, cell.t + g.px(8.0), inner.r, cell.t + g.px(32.0));
                    g.text_right(top, &val, Font::Big, th.text);
                    g.text_right(R::new(inner.l, cell.t + g.px(32.0), inner.r, cell.b - g.px(4.0)), c.title, Font::Small, th.dim);
                } else {
                    g.text_right(inner, c.title, Font::Small, th.dim);
                }
            } else if c.align == Align::Right {
                g.text_right(R::new(inner.l, cell.t, inner.r, cell.b - if big { g.px(4.0) } else { 0 }), c.title, if big { Font::Small } else { Font::Semibold }, if big { th.dim } else { th.text });
            } else {
                let lr = R::new(inner.l + if i == 0 { g.px(12.0) } else { 0 }, if big { cell.b - g.px(28.0) } else { cell.t }, inner.r, cell.b - if big { g.px(2.0) } else { 0 });
                g.text(lr, c.title, if big { Font::Small } else { Font::Semibold }, if big { th.dim } else { th.text });
            }
            if sort.0 == i {
                let ch = if sort.1 { '\u{E70D}' } else { '\u{E70E}' }; // стрелка вниз/вверх
                let ar = R::new(cell.l + (cell.w() - g.px(16.0)) / 2, cell.t + g.px(0.0), cell.l + (cell.w() + g.px(16.0)) / 2, cell.t + g.px(12.0));
                if c.metric.is_some() {
                    g.glyph(ar, ch, th.text, true);
                } else {
                    g.glyph(R::new(cell.r - g.px(24.0), cell.t, cell.r - g.px(4.0), cell.t + g.px(14.0)), ch, th.dim, true);
                }
            }
            if i > 0 {
                g.vline(cell.l, lay.header.t, lay.header.b, th.border);
            }
        }
        g.hline(lay.header.l, lay.header.r, lay.header.b - 1, th.border);

        // строки
        let row_h = g.px(ROW_H);
        g.clip(area);
        let first = (ui.scroll / row_h as f64).floor().max(0.0) as usize;
        let last = ((ui.scroll + area.h() as f64) / row_h as f64).ceil() as usize + 1;
        for (i, row) in ui.rows.iter().enumerate().skip(first).take(last.saturating_sub(first)) {
            let y = area.t + i as i32 * row_h - ui.scroll as i32;
            let selected = ui.sel.as_deref() == Some(row.key.as_str());
            let hovered = matches!(ui.hit, Hit::Row(j) | Hit::Chevron(j) if j == i);
            let base = if selected { th.sel } else if hovered { th.hover } else { th.bg };
            g.fill(R::new(area.l, y, xs[cols.len()].min(area.r), y + row_h), base);
            for (ci, c) in cols.iter().enumerate() {
                let cell = R::new(xs[ci], y, xs[ci + 1], y + row_h);
                let mut tc = th.text;
                if let Some(Some((v, scale))) = row.heat.get(ci) {
                    let h = heat(&th, *v, *scale);
                    g.fill(cell, if selected { h.lerp(th.text, 0.07) } else if hovered { h.lerp(th.text, 0.04) } else { h });
                    if *v < 0.05 {
                        tc = th.dim;
                    }
                }
                let text = row.cells.get(ci).map(String::as_str).unwrap_or("");
                let inner = R::new(cell.l + g.px(8.0), cell.t, cell.r - g.px(8.0), cell.b);
                if ci == 0 {
                    let mut x = inner.l + g.px(4.0) + row.depth as i32 * g.px(22.0);
                    if matches!(page, Page::Processes | Page::Users) {
                        if row.expandable {
                            g.glyph(R::new(x - g.px(4.0), y, x + g.px(14.0), y + row_h), if row.expanded { '\u{E70D}' } else { '\u{E76C}' }, th.dim, true);
                        }
                        x += g.px(18.0);
                    }
                    if row.icon_path.is_some() || page == Page::Processes || (page == Page::Users && row.depth > 0) {
                        let sz = g.px(16.0);
                        let ico = row.icon_path.as_deref().and_then(|p| d.res.get(p)).map_or(0, |r| r.icon);
                        g.icon(if ico != 0 { ico } else { d.res.generic_icon }, x, y + (row_h - sz) / 2, sz);
                        x += sz + g.px(8.0);
                    }
                    g.text(R::new(x, y, inner.r, y + row_h), text, Font::Ui, th.text);
                } else if page == Page::Processes && ci == cols.len() - 1 && row.sev > 0 {
                    let col = match row.sev {
                        3 => th.bad,
                        2 => th.warn,
                        _ => th.dim,
                    };
                    g.text(inner, text, Font::Ui, col);
                } else if c.align == Align::Right {
                    g.text_right(inner, text, Font::Ui, tc);
                } else {
                    g.text(inner, text, Font::Ui, tc);
                }
            }
            for ci in 1..cols.len() {
                if cols[ci].metric.is_some() || cols[ci - 1].metric.is_some() {
                    g.vline(xs[ci], y, y + row_h, th.border);
                }
            }
        }
        g.unclip();

        if ui.rows.is_empty() {
            let msg = if ui.search.is_empty() { t!("Нет данных", "No data") } else { t!("Ничего не найдено", "Nothing found") };
            g.text_center(R::new(area.l, area.t + g.px(30.0), area.r, area.t + g.px(70.0)), msg, Font::Ui, th.dim);
        }

        // полоса прокрутки
        let content = ui.rows.len() as f64 * row_h as f64;
        if content > area.h() as f64 {
            let track = lay.vbar;
            let thumb_h = ((area.h() as f64 / content) * track.h() as f64).max(g.px(24.0) as f64);
            let max_scroll = content - area.h() as f64;
            let ty = track.t as f64 + (ui.scroll / max_scroll) * (track.h() as f64 - thumb_h);
            let active = ui.drag.is_some() || ui.hit == Hit::VBar;
            let w = if active { g.px(6.0) } else { g.px(3.0) };
            g.round(R::new(track.r - w - g.px(2.0), ty as i32, track.r - g.px(2.0), (ty + thumb_h) as i32), g.px(2.0), Some(th.dim.lerp(th.bg, if active { 0.0 } else { 0.35 })), None);
        }
    }

    // ---------------- «Что тормозит» ----------------
    pub fn draw_insights(&self, g: &G, ui: &Ui, d: &Data, lay: &Lay) {
        let th = g.th;
        let mut y = lay.body.t + g.px(16.0) - ui.scroll as i32;
        let w = (lay.body.w() - g.px(48.0)).min(g.px(900.0));
        g.clip(lay.body);
        for h in d.hints() {
            let th_h = g.wrap_h(&h, Font::Ui, w - g.px(32.0)) + g.px(24.0);
            let r = R::new(lay.body.l + g.px(24.0), y, lay.body.l + g.px(24.0) + w, y + th_h);
            g.round(r, g.px(6.0), Some(th.panel), Some(th.border));
            g.text_wrapped(R::new(r.l + g.px(16.0), r.t + g.px(12.0), r.r - g.px(16.0), r.b - g.px(12.0)), &h, Font::Ui, th.text);
            y += th_h + g.px(10.0);
        }
        g.unclip();
    }

    // ---------------- «Параметры» ----------------
    pub fn draw_settings(&self, g: &G, ui: &Ui, d: &Data, lay: &Lay) {
        let th = g.th;
        g.clip(lay.body);
        for (r, id) in &lay.setting_cards {
            g.round(*r, g.px(6.0), Some(th.panel), Some(th.border));
            let (title, desc) = setting_text(*id);
            g.text(R::new(r.l + g.px(16.0), r.t + g.px(8.0), r.r - g.px(220.0), r.t + g.px(30.0)), title, Font::Semibold, th.text);
            g.text(R::new(r.l + g.px(16.0), r.t + g.px(30.0), r.r - g.px(220.0), r.b - g.px(6.0)), desc, Font::Small, th.dim);
            let ctl = R::new(r.r - g.px(200.0), r.t, r.r - g.px(16.0), r.b);
            match id {
                SettingId::Theme => {
                    let seg = [t!("Светлая", "Light"), t!("Тёмная", "Dark"), t!("Системная", "System")];
                    let sw = ctl.w() / 3;
                    for (i, s) in seg.iter().enumerate() {
                        let sr = R::new(ctl.l + i as i32 * sw, r.t + g.px(14.0), ctl.l + (i as i32 + 1) * sw, r.b - g.px(14.0));
                        let on = d.cfg.theme as usize == i;
                        g.round(sr, g.px(4.0), Some(if on { th.accent } else { th.bg }), Some(th.border));
                        g.text_center(sr, s, Font::Small, if on { if th.dark { Rgb(0, 0, 0) } else { Rgb(255, 255, 255) } } else { th.text });
                    }
                }
                _ => {
                    let on = setting_on(*id, d);
                    let enabled = !(*id == SettingId::Etw && !d.is_admin);
                    let t = R::new(ctl.r - g.px(44.0), r.t + r.h() / 2 - g.px(10.0), ctl.r, r.t + r.h() / 2 + g.px(10.0));
                    g.round(t, g.px(10.0), Some(if on { th.accent } else { th.bg }), Some(if on { th.accent } else { th.dim }));
                    let kn = g.px(7.0);
                    let cx = if on { t.r - g.px(11.0) } else { t.l + g.px(11.0) };
                    g.round(R::new(cx - kn, t.t + g.px(10.0) - kn, cx + kn, t.t + g.px(10.0) + kn), kn, Some(if on { if th.dark { Rgb(0, 0, 0) } else { Rgb(255, 255, 255) } } else { th.dim }), None);
                    if !enabled {
                        g.text_right(R::new(ctl.l, r.t, t.l - g.px(10.0), r.b), t!("нужен администратор", "administrator needed"), Font::Small, th.dim);
                    }
                }
            }
        }
        g.unclip();
        let _ = ui;
    }

    // ---------------- «Производительность» ----------------
    pub fn draw_performance(&self, g: &G, ui: &Ui, d: &Data, lay: &Lay) {
        let th = g.th;
        let items = perf_items(d);
        let sel_idx = items.iter().position(|i| i.kind == ui.perf_sel).unwrap_or(0);
        g.fill(lay.perf_list, th.bg);
        for (i, r) in lay.perf_items.iter().enumerate() {
            let it = &items[i];
            let sel = sel_idx == i;
            let hov = ui.hit == Hit::PerfItem(i);
            if sel || hov {
                g.round(r.inset(g.px(2.0)), g.px(6.0), Some(if sel { th.sel } else { th.hover }), None);
            }
            let thumb = R::new(r.l + g.px(10.0), r.t + g.px(10.0), r.l + g.px(10.0) + g.px(72.0), r.b - g.px(10.0));
            draw_graph(g, thumb, &it.graphs[0].series, it.graphs[0].ymax, false);
            let tx = thumb.r + g.px(10.0);
            g.text(R::new(tx, r.t + g.px(10.0), r.r - g.px(6.0), r.t + g.px(30.0)), &it.title, Font::Ui, th.text);
            g.text(R::new(tx, r.t + g.px(30.0), r.r - g.px(6.0), r.t + g.px(48.0)), &it.line1, Font::Small, th.dim);
            g.text(R::new(tx, r.t + g.px(46.0), r.r - g.px(6.0), r.t + g.px(64.0)), &it.line2, Font::Small, th.dim);
        }
        g.vline(lay.perf_list.r, lay.perf_list.t, lay.perf_list.b, th.border);

        let m = lay.perf_main;
        let it = &items[sel_idx];
        g.text(R::new(m.l, m.t, m.r - g.px(420.0), m.t + g.px(48.0)), &it.big_title, Font::Huge, th.text);
        g.text_right(R::new(m.r - g.px(420.0), m.t + g.px(10.0), m.r, m.t + g.px(40.0)), &it.device, Font::Big, th.text);

        let frac = match it.layout {
            GLayout::Single => 0.50,
            GLayout::Stack => 0.50,
            GLayout::Grid => 0.56,
        };
        let block_h = ((m.h() as f64 * frac) as i32).max(g.px(180.0));
        let block = R::new(m.l, m.t + g.px(56.0), m.r, m.t + g.px(56.0) + block_h);
        self.draw_graph_block(g, it, block);
        g.text(R::new(m.l, block.b + g.px(2.0), m.l + g.px(200.0), block.b + g.px(20.0)), t!("60 секунд", "60 seconds"), Font::Small, th.dim);
        g.text_right(R::new(m.r - g.px(100.0), block.b + g.px(2.0), m.r, block.b + g.px(20.0)), "0", Font::Small, th.dim);
        let mut y = block.b + g.px(30.0);
        if let Some((cap, segs)) = &it.composition {
            g.text(R::new(m.l, y, m.r, y + g.px(18.0)), cap, Font::Small, th.dim);
            let br = R::new(m.l, y + g.px(22.0), m.r, y + g.px(22.0) + g.px(28.0));
            g.fill(br, th.bg.lerp(th.text, 0.06));
            let mut x = br.l;
            for (frac, color) in segs {
                let w = (br.w() as f32 * frac) as i32;
                g.fill(R::new(x, br.t, (x + w).min(br.r), br.b), *color);
                x += w;
            }
            g.frame(br, th.border);
            y = br.b + g.px(18.0);
        }
        // крупные показатели слева и перечень сведений справа
        let colw = g.px(200.0);
        let cols = it.stat_cols.max(1);
        let (mut bx, mut by) = (m.l, y);
        for (i, (label, value)) in it.stats.iter().enumerate() {
            if i > 0 && i % cols == 0 {
                bx = m.l;
                by += g.px(66.0);
            }
            g.text(R::new(bx, by, bx + colw, by + g.px(18.0)), label, Font::Small, th.dim);
            g.text(R::new(bx, by + g.px(18.0), bx + colw + g.px(10.0), by + g.px(48.0)), value, Font::Big, th.text);
            bx += colw + g.px(20.0);
        }
        let rx = (m.l + cols as i32 * (colw + g.px(20.0)) + g.px(40.0)).max(m.r - g.px(520.0));
        let mut ry = y;
        for (k, v) in &it.facts {
            g.text(R::new(rx, ry, rx + g.px(230.0), ry + g.px(22.0)), k, Font::Small, th.dim);
            g.text(R::new(rx + g.px(230.0), ry, m.r, ry + g.px(22.0)), v, Font::Small, th.text);
            ry += g.px(22.0);
        }
    }

    /// Графики страницы: один большой, два друг под другом или сетка 2×N.
    fn draw_graph_block(&self, g: &G, it: &PerfItem, r: R) {
        let gap = g.px(10.0);
        let one = |gs: &GraphSpec, rr: R| {
            let lab = g.px(18.0);
            g.text(R::new(rr.l, rr.t, rr.r - g.px(120.0), rr.t + lab), &gs.label, Font::Small, g.th.dim);
            g.text_right(R::new(rr.r - g.px(160.0), rr.t, rr.r, rr.t + lab), &gs.right, Font::Small, g.th.dim);
            draw_graph(g, R::new(rr.l, rr.t + lab + g.px(2.0), rr.r, rr.b), &gs.series, gs.ymax, true);
        };
        match it.layout {
            GLayout::Single => one(&it.graphs[0], r),
            GLayout::Stack => {
                let h1 = (r.h() as f32 * 0.62) as i32;
                one(&it.graphs[0], R::new(r.l, r.t, r.r, r.t + h1 - gap / 2));
                if let Some(g2) = it.graphs.get(1) {
                    one(g2, R::new(r.l, r.t + h1 + gap / 2, r.r, r.b));
                }
            }
            GLayout::Grid => {
                let n = it.graphs.len() as i32;
                let rows = (n + 1) / 2;
                let (cw, ch) = ((r.w() - gap) / 2, (r.h() - gap * (rows - 1)) / rows);
                for (i, gs) in it.graphs.iter().enumerate() {
                    let (cx, cy) = (r.l + (i as i32 % 2) * (cw + gap), r.t + (i as i32 / 2) * (ch + gap));
                    one(gs, R::new(cx, cy, cx + cw, cy + ch));
                }
            }
        }
    }

    // ---------------- подсказка и уведомление ----------------
    pub fn draw_overlay(&self, g: &G, ui: &Ui, client: R) {
        let th = g.th;
        if let Some((text, (mx, my))) = &ui.tip {
            let w = g.px(430.0);
            let h = g.wrap_h(text, Font::Small, w - g.px(24.0)) + g.px(20.0);
            let mut x = mx + g.px(14.0);
            let mut y = my + g.px(20.0);
            if x + w > client.r {
                x = client.r - w - g.px(6.0);
            }
            if y + h > client.b {
                y = my - h - g.px(8.0);
            }
            let r = R::new(x, y, x + w, y + h);
            g.round(r, g.px(6.0), Some(th.panel), Some(th.dim.lerp(th.bg, 0.4)));
            g.text_wrapped(R::new(r.l + g.px(12.0), r.t + g.px(8.0), r.r - g.px(12.0), r.b - g.px(8.0)), text, Font::Small, th.text);
        }
        if let Some((msg, _)) = &ui.toast {
            let tw = g.text_w(msg, Font::Ui).min(client.w() - g.px(160.0)) + g.px(40.0);
            let r = R::new(client.l + (client.w() - tw) / 2 + g.px(20.0), client.b - g.px(60.0), client.l + (client.w() + tw) / 2 + g.px(20.0), client.b - g.px(20.0));
            g.round(r, g.px(6.0), Some(th.panel), Some(th.dim.lerp(th.bg, 0.5)));
            g.text_center(r.inset(g.px(8.0)), msg, Font::Ui, th.text);
        }
    }
}

fn agg_value(d: &Data, m: Metric) -> f64 {
    match m {
        Metric::Cpu => d.cpu_total as f64,
        Metric::Mem => d.mem_pct as f64,
        Metric::Disk => d.sample.disk_active as f64,
        Metric::Net => d.sample.net_util as f64,
        Metric::Gpu => d.sample.gpu_total as f64,
    }
}

fn agg_text(d: &Data, m: Metric) -> String {
    format!("{:.0}%", agg_value(d, m))
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SettingId {
    Theme,
    Tray,
    Autostart,
    Etw,
    GpuTemp,
}

pub fn setting_text(id: SettingId) -> (&'static str, &'static str) {
    match id {
        SettingId::Theme => (t!("Тема оформления", "Theme"), t!("Светлая, тёмная или как в системе", "Light, dark or follow the system")),
        SettingId::Tray => (t!("Закрытие окна сворачивает в трей", "Closing the window minimizes to tray"), t!("Программа продолжит собирать историю и применять правила; вернуть окно можно по Ctrl+Alt+T", "The program keeps collecting history and applying rules; bring the window back with Ctrl+Alt+T")),
        SettingId::Autostart => (t!("Запускать при входе в Windows", "Start with Windows"), t!("Сразу в трей, без окна", "Straight to the tray, no window")),
        SettingId::Etw => (t!("Сеть по процессам", "Per-process network"), t!("Колонка «Сеть» в процессах (ETW). Требуются права администратора", "'Network' column in Processes (ETW). Needs administrator rights")),
        SettingId::GpuTemp => (t!("Всегда читать температуру GPU", "Always read GPU temperature"), t!("Иначе температура читается только на странице GPU. Постоянный опрос может будить дискретную видеокарту ноутбука", "Otherwise the temperature is read only on the GPU page. Constant polling can wake a laptop's discrete GPU")),
    }
}

pub fn setting_on(id: SettingId, d: &Data) -> bool {
    match id {
        SettingId::Theme => false,
        SettingId::Tray => d.cfg.to_tray,
        SettingId::Autostart => crate::startup::self_autostart(),
        SettingId::Etw => d.etw.is_some(),
        SettingId::GpuTemp => d.cfg.gpu_temp_always,
    }
}

pub struct GraphSpec<'a> {
    pub label: String,
    pub right: String,
    pub series: Vec<(&'a VecDeque<f32>, Rgb)>,
    pub ymax: f32,
}

#[derive(Clone, Copy, PartialEq)]
pub enum GLayout {
    Single,
    Stack,
    Grid,
}

/// Какой ресурс показывает пункт списка слева.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PerfKind {
    Cpu,
    Mem,
    Disk(usize),
    Nic(usize),
    Gpu(usize),
}

/// Пункты списка: ЦП, память, каждый физический диск, каждый сетевой адаптер, каждая видеокарта.
pub fn perf_kinds(d: &Data) -> Vec<PerfKind> {
    let mut v = vec![PerfKind::Cpu, PerfKind::Mem];
    v.extend((0..d.disks.len()).map(PerfKind::Disk));
    v.extend((0..d.nics.len()).map(PerfKind::Nic));
    v.extend((0..d.gpus.len()).map(PerfKind::Gpu));
    v
}

/// Элемент списка ресурсов слева на странице «Производительность» и данные для правой части.
pub struct PerfItem<'a> {
    pub kind: PerfKind,
    pub title: String,
    pub line1: String,
    pub line2: String,
    pub graphs: Vec<GraphSpec<'a>>,
    pub layout: GLayout,
    pub big_title: String,
    pub device: String,
    pub stats: Vec<(String, String)>,
    pub stat_cols: usize,
    pub facts: Vec<(String, String)>,
    pub composition: Option<(String, Vec<(f32, Rgb)>)>,
}

fn gb(b: u64) -> String {
    tf!("{} ГБ", "{} GB", dec(b as f64 / 1_073_741_824.0, 1))
}

fn mb_cache(b: u64) -> String {
    if b >= 1 << 20 { tf!("{} МБ", "{} MB", dec(b as f64 / 1_048_576.0, 1)) } else { tf!("{} КБ", "{} KB", b >> 10) }
}

/// Название подключения по имени адаптера: «Wi-Fi», «Ethernet», «Bluetooth» …
pub fn conn_name(adapter: &str) -> &'static str {
    let l = adapter.to_lowercase();
    if l.contains("wi-fi") || l.contains("wireless") || l.contains("wlan") || l.contains("802.11") {
        "Wi-Fi"
    } else if l.contains("bluetooth") {
        "Bluetooth"
    } else if l.contains("vmware") || l.contains("virtual") || l.contains("hyper-v") || l.contains("vethernet") || l.contains("vpn") || l.contains("tap-") {
        t!("Виртуальный адаптер", "Virtual adapter")
    } else {
        "Ethernet"
    }
}

pub fn perf_items(d: &Data) -> Vec<PerfItem<'_>> {
    let dark = d.cfg.theme != 0 && (d.cfg.theme == 1 || crate::sys::system_dark());
    let col = |i| series_color(i, dark);
    let s = &d.series;
    let last = |q: &VecDeque<f32>| q.back().copied().unwrap_or(0.0);
    let si = &d.sysinfo;
    let speed = si.base_mhz as f32 * d.sample.cpu_perf.max(1.0) / 100.0 / 1000.0;
    let total = d.sys.total_memory();
    let used = d.sys.used_memory();
    let cpu_brand = d.sys.cpus().first().map_or(String::new(), |c| c.brand().to_string());
    let m = &d.meminfo;
    let ms = |x: f64| tf!("{} МБ/с", "{} MB/s", dec(x / 1_048_576.0, 1));
    let gspec = |label: &str, right: &str, q, color, ymax| GraphSpec { label: label.into(), right: right.into(), series: vec![(q, color)], ymax };
    let kinds = perf_kinds(d);
    let mut v = vec![];

    // для одинаковых названий подключений добавляем номер: «Wi-Fi», «Wi-Fi 2»
    let nic_titles: Vec<String> = {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        d.nics
            .iter()
            .map(|n| {
                let c = conn_name(&n.name);
                let k = seen.entry(c).or_insert(0);
                *k += 1;
                if *k > 1 { format!("{c} {k}") } else { c.to_string() }
            })
            .collect()
    };

    for kind in kinds {
        match kind {
            PerfKind::Cpu => v.push(PerfItem {
                kind,
                title: t!("ЦП", "CPU").into(),
                line1: tf!("{:.0}%  {} ГГц", "{:.0}%  {} GHz", last(&s.cpu), dec(speed as f64, 2)),
                line2: String::new(),
                graphs: vec![gspec(t!("% использования", "% utilization"), "100%", &s.cpu, col(0), 100.0)],
                layout: GLayout::Single,
                big_title: t!("ЦП", "CPU").into(),
                device: cpu_brand.clone(),
                stats: vec![
                    (t!("Использование", "Utilization").into(), format!("{:.0}%", last(&s.cpu))),
                    (t!("Скорость", "Speed").into(), tf!("{} ГГц", "{} GHz", dec(speed as f64, 2))),
                    (t!("Процессы", "Processes").into(), m.processes.to_string()),
                    (t!("Потоки", "Threads").into(), m.threads.to_string()),
                    (t!("Дескрипторы", "Handles").into(), m.handles.to_string()),
                    (t!("Время работы", "Up time").into(), fmt_uptime(crate::perf::uptime_secs())),
                ],
                stat_cols: 2,
                facts: vec![
                    (t!("Базовая скорость:", "Base speed:").into(), tf!("{} ГГц", "{} GHz", dec(si.base_mhz as f64 / 1000.0, 2))),
                    (t!("Сокетов:", "Sockets:").into(), si.sockets.to_string()),
                    (t!("Ядер:", "Cores:").into(), sysinfo::System::physical_core_count().unwrap_or(0).to_string()),
                    (t!("Логических процессоров:", "Logical processors:").into(), d.sys.cpus().len().to_string()),
                    (t!("Виртуализация:", "Virtualization:").into(), if si.virt { t!("Включено", "Enabled") } else { t!("Отключено", "Disabled") }.into()),
                    (t!("Кэш L1:", "L1 cache:").into(), mb_cache(si.l1)),
                    (t!("Кэш L2:", "L2 cache:").into(), mb_cache(si.l2)),
                    (t!("Кэш L3:", "L3 cache:").into(), mb_cache(si.l3)),
                ],
                composition: None,
            }),

            PerfKind::Mem => {
                let avail = total.saturating_sub(used);
                let mh = &d.hw_mem;
                let kind_txt = mh.kind();
                let reserved = (mh.installed_mb() * 1_048_576).saturating_sub(total);
                let compressed = d.procs.iter().find(|p| p.name.eq_ignore_ascii_case("Memory Compression")).map_or(0, |p| p.mem);
                let cached = if d.sample.mem_modified + d.sample.mem_standby > 0 { d.sample.mem_modified + d.sample.mem_standby } else { m.cache };
                let mut facts: Vec<(String, String)> = vec![];
                if !mh.dimms.is_empty() {
                    facts.push((t!("Скорость:", "Speed:").into(), tf!("{} МТ/с", "{} MT/s", mh.speed())));
                    facts.push((t!("Использовано слотов:", "Slots used:").into(), tf!("{} из {}", "{} of {}", mh.dimms.len(), mh.slots.max(mh.dimms.len() as u32))));
                    facts.push((t!("Форм-фактор:", "Form factor:").into(), mh.form().into()));
                    facts.push((t!("Зарезервировано аппаратурой:", "Hardware reserved:").into(), tf!("{} МБ", "{} MB", dec(reserved as f64 / 1_048_576.0, 0))));
                    if !kind_txt.is_empty() {
                        facts.push((t!("Тип:", "Type:").into(), kind_txt.into()));
                    }
                    if let Some(d0) = mh.dimms.first().filter(|x| !x.mfr.is_empty()) {
                        facts.push((t!("Производитель:", "Manufacturer:").into(), d0.mfr.clone()));
                    }
                }
                let t = total.max(1) as f32;
                let (mut seg, mut parts): (Vec<(f32, Rgb)>, Vec<String>) = (vec![], vec![]);
                seg.push((used as f32 / t, col(1)));
                parts.push(tf!("Используется {}", "In use {}", gb(used)));
                if d.sample.mem_modified > 0 {
                    seg.push((d.sample.mem_modified as f32 / t, col(1).lerp(Rgb(255, 255, 255), 0.25)));
                    parts.push(tf!("Изменено {}", "Modified {}", gb(d.sample.mem_modified)));
                }
                if d.sample.mem_standby > 0 {
                    seg.push(((d.sample.mem_standby.min(avail)) as f32 / t, col(1).lerp(Rgb(128, 128, 128), 0.6)));
                    parts.push(tf!("В режиме ожидания {}", "Standby {}", gb(d.sample.mem_standby)));
                }
                if d.sample.mem_free > 0 {
                    parts.push(tf!("Свободно {}", "Free {}", gb(d.sample.mem_free)));
                }
                v.push(PerfItem {
                    kind,
                    title: t!("Память", "Memory").into(),
                    line1: format!("{}/{} ({:.0}%)", dec(used as f64 / 1_073_741_824.0, 1), gb(total), d.mem_pct),
                    line2: String::new(),
                    graphs: vec![gspec(t!("Использование памяти", "Memory usage"), &gb(total), &s.mem, col(1), 100.0)],
                    layout: GLayout::Single,
                    big_title: t!("Память", "Memory").into(),
                    device: if kind_txt.is_empty() { gb(total) } else { format!("{} {}", gb(total), kind_txt) },
                    stats: vec![
                        (if compressed > 0 { t!("Используется (сжато)", "In use (compressed)") } else { t!("Используется", "In use") }.into(), if compressed > 0 { format!("{} ({})", gb(used), gb(compressed)) } else { gb(used) }),
                        (t!("Доступно", "Available").into(), gb(avail)),
                        (t!("Выделено", "Committed").into(), format!("{}/{}", dec(m.commit_total as f64 / 1_073_741_824.0, 1), gb(m.commit_limit))),
                        (t!("Кэшировано", "Cached").into(), gb(cached)),
                        (t!("Выгружаемый пул", "Paged pool").into(), tf!("{} МБ", "{} MB", dec(m.paged as f64 / 1_048_576.0, 0))),
                        (t!("Невыгружаемый пул", "Non-paged pool").into(), tf!("{} МБ", "{} MB", dec(m.nonpaged as f64 / 1_048_576.0, 0))),
                    ],
                    stat_cols: 2,
                    facts,
                    composition: Some((tf!("Состав памяти: {}", "Memory composition: {}", parts.join(" · ")), seg)),
                });
            }

            PerfKind::Disk(i) => {
                let dk = &d.disks[i];
                let info = &dk.info;
                let model = info.model.replace('_', " ");
                let kind_txt = match (info.kind.as_str(), info.bus.as_str()) {
                    ("", "") => "—".to_string(),
                    (k, "") => k.to_string(),
                    ("", b) => b.to_string(),
                    (k, b) => format!("{k} ({b})"),
                };
                let title = tf!("Диск {} ({})", "Disk {} ({})", dk.number, dk.letters.trim());
                let xfer_max = dk.s_rd.iter().chain(dk.s_wr.iter()).cloned().fold(1.0f32, f32::max) * 1.15;
                let system = dk.letters.to_uppercase().contains("C:");
                v.push(PerfItem {
                    kind,
                    title: title.clone(),
                    line1: if info.kind.is_empty() { t!("Локальный диск", "Local disk").into() } else { kind_txt.clone() },
                    line2: format!("{:.0}%", dk.active),
                    graphs: vec![
                        gspec(t!("Время активности", "Active time"), "100%", &dk.s_active, col(2), 100.0),
                        GraphSpec { label: t!("Скорость передачи данных", "Transfer rate").into(), right: tf!("{} МБ/с", "{} MB/s", dec(xfer_max as f64, 0)), series: vec![(&dk.s_rd, col(2)), (&dk.s_wr, col(2).lerp(Rgb(255, 255, 255), 0.45))], ymax: xfer_max },
                    ],
                    layout: GLayout::Stack,
                    big_title: title,
                    device: model,
                    stats: vec![
                        (t!("Время активности", "Active time").into(), format!("{:.0}%", dk.active)),
                        (t!("Средний отклик", "Average response time").into(), tf!("{} мс", "{} ms", dec(dk.ms as f64, 1))),
                        (t!("Скорость чтения", "Read speed").into(), ms(dk.rd)),
                        (t!("Скорость записи", "Write speed").into(), ms(dk.wr)),
                    ],
                    stat_cols: 2,
                    facts: vec![
                        (t!("Ёмкость:", "Capacity:").into(), gb(info.capacity.max(dk.formatted))),
                        (t!("Форматированный объём:", "Formatted:").into(), gb(dk.formatted)),
                        (t!("Системный диск:", "System disk:").into(), if system { t!("Да", "Yes") } else { t!("Нет", "No") }.into()),
                        (t!("Файл подкачки:", "Page file:").into(), if system { t!("Да", "Yes") } else { t!("Нет", "No") }.into()),
                        (t!("Тип:", "Type:").into(), kind_txt),
                    ],
                    composition: None,
                });
            }

            PerfKind::Nic(i) => {
                let n = &d.nics[i];
                let title = nic_titles[i].clone();
                let (rx, tx) = (last(&n.s_rx), last(&n.s_tx));
                let ymax = n.s_rx.iter().chain(n.s_tx.iter()).cloned().fold(1000.0f32, f32::max) * 1.15;
                let conn = conn_name(&n.name);
                v.push(PerfItem {
                    kind,
                    title: title.clone(),
                    line1: conn.into(),
                    line2: tf!("О: {} П: {} Кбит/с", "S: {} R: {} Kbps", tx as i64, rx as i64),
                    graphs: vec![GraphSpec { label: t!("Пропускная способность", "Throughput").into(), right: tf!("{} Кбит/с", "{} Kbps", ymax as i64), series: vec![(&n.s_rx, col(3)), (&n.s_tx, col(3).lerp(Rgb(255, 255, 255), 0.4))], ymax }],
                    layout: GLayout::Single,
                    big_title: title,
                    device: n.name.clone(),
                    stats: vec![(t!("Отправка", "Send").into(), tf!("{} Кбит/с", "{} Kbps", tx as i64)), (t!("Получение", "Receive").into(), tf!("{} Кбит/с", "{} Kbps", rx as i64))],
                    stat_cols: 2,
                    facts: vec![(t!("Имя адаптера:", "Adapter name:").into(), n.name.clone()), (t!("Тип подключения:", "Connection type:").into(), conn.into()), (t!("Скорость канала:", "Link speed:").into(), tf!("{:.0} Мбит/с", "{:.0} Mbps", n.bw / 1e6))],
                    composition: None,
                });
            }

            PerfKind::Gpu(i) => {
                let g = &d.gpus[i];
                let a = &g.adapter;
                let vram = a.dedicated.max(g.reg.vram);
                let shared_total = a.shared;
                let temp = g.temp.map(|t| format!("{} °C", dec(t as f64, 0)));
                let name = a.name.clone();
                let title = format!("GPU {i}");
                let vram_mb = (vram >> 20).max(1) as f32;
                v.push(PerfItem {
                    kind,
                    title: title.clone(),
                    line1: name.clone(),
                    line2: match &temp {
                        Some(t) => format!("{:.0}% ({t})", g.util),
                        None => format!("{:.0}%", g.util),
                    },
                    graphs: vec![
                        gspec("3D", "100%", &g.series.g3d, col(4), 100.0),
                        gspec(t!("Копирование", "Copy"), "100%", &g.series.gcopy, col(4), 100.0),
                        gspec(t!("Видеокодирование", "Video encode"), "100%", &g.series.genc, col(4), 100.0),
                        gspec(t!("Видеодекодирование", "Video decode"), "100%", &g.series.gdec, col(4), 100.0),
                        gspec(t!("Использование выделенной памяти GPU", "Dedicated GPU memory usage"), &gb(vram), &g.series.ded, col(4), vram_mb),
                        gspec(t!("Использование общей памяти GPU", "Shared GPU memory usage"), &gb(shared_total), &g.series.shr, col(4), (shared_total >> 20).max(1) as f32),
                    ],
                    layout: GLayout::Grid,
                    big_title: title,
                    device: name.clone(),
                    stats: vec![
                        (t!("Использование", "Utilization").into(), format!("{:.0}%", g.util)),
                        (t!("Общая память GPU", "Shared GPU memory").into(), format!("{}/{}", dec(g.shr_used as f64 / 1_073_741_824.0, 1), gb(shared_total))),
                        (t!("Память GPU", "GPU memory").into(), format!("{}/{}", dec((g.ded_used + g.shr_used) as f64 / 1_073_741_824.0, 1), gb(vram + shared_total))),
                        (t!("Температура GPU", "GPU temperature").into(), temp.clone().unwrap_or_else(|| "—".into())),
                    ],
                    stat_cols: 2,
                    facts: {
                        let dash = |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
                        vec![
                            (t!("Выделенная память GPU:", "Dedicated GPU memory:").into(), format!("{}/{}", dec(g.ded_used as f64 / 1_073_741_824.0, 1), gb(vram))),
                            (t!("Версия драйвера:", "Driver version:").into(), dash(&g.reg.driver_version)),
                            (t!("Дата разработки:", "Driver date:").into(), if g.reg.driver_date.is_empty() { "—".into() } else { ru_date(&g.reg.driver_date) }),
                            (t!("Версия DirectX:", "DirectX version:").into(), dash(&a.dx)),
                            (t!("Физическое расположение:", "Physical location:").into(), dash(&a.location)),
                        ]
                    },
                    composition: None,
                });
            }
        }
    }
    v
}

/// «10-19-2026» (месяц-день-год, как хранит драйвер) -> «19.10.2026».
pub fn ru_date(s: &str) -> String {
    let p: Vec<&str> = s.split(['-', '/']).collect();
    if let [m, d, y] = p[..] { format!("{d}.{m}.{y}") } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nic(name: &str) -> NicState {
        NicState { name: name.into(), rx: 0.0, tx: 0.0, bw: 1e9, s_rx: Default::default(), s_tx: Default::default() }
    }

    fn disk(number: u32, letters: &str) -> DiskState {
        DiskState {
            number,
            letters: letters.into(),
            info: crate::hw::DiskInfo { number, model: "TEST_DISK".into(), kind: "HDD".into(), bus: "SATA".into(), capacity: 2 << 40, formatted: 0 },
            formatted: 1 << 40,
            active: 0.0,
            rd: 0.0,
            wr: 0.0,
            ms: 0.0,
            s_active: Default::default(),
            s_rd: Default::default(),
            s_wr: Default::default(),
        }
    }

    fn gpu(name: &str, vendor: u32) -> GpuState {
        GpuState {
            adapter: crate::gpu::Adapter { name: name.into(), vendor, luid: (vendor, 0), dedicated: 4 << 30, shared: 8 << 30, dx: "DirectX 12 (FL 12_1)".into(), location: "PCI-шина 0, устройство 2, функция 0".into() },
            reg: Default::default(),
            util: 0.0,
            ded_used: 0,
            shr_used: 0,
            temp: Some(55.0),
            series: Default::default(),
        }
    }

    #[test]
    fn several_disks_adapters_and_gpus_get_their_own_items() {
        let mut d = Data::new(crate::store::Settings::default());
        d.disks = vec![disk(0, "C:"), disk(1, "D: E:")];
        d.nics = vec![nic("Intel Wi-Fi 6 AX200"), nic("Realtek PCIe GbE Family Controller"), nic("Qualcomm Wireless 802.11ac")];
        d.gpus = vec![gpu("Intel UHD Graphics", 0x8086), gpu("AMD Radeon RX", 0x1002)];
        let items = perf_items(&d);
        let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["ЦП", "Память", "Диск 0 (C:)", "Диск 1 (D: E:)", "Wi-Fi", "Ethernet", "Wi-Fi 2", "GPU 0", "GPU 1"]);
        assert_eq!(perf_kinds(&d).len(), items.len());
        // у каждого GPU — свои сведения: DirectX, расположение и температура (в том числе не NVIDIA)
        let g1 = items.last().unwrap();
        assert!(g1.facts.iter().any(|f| f.0.contains("DirectX") && f.1.contains("12")));
        assert!(g1.facts.iter().any(|f| f.0.contains("расположение") && f.1.contains("PCI")));
        assert!(g1.line2.contains("55 °C"));
        // второй диск — не системный
        let d1 = &items[3];
        assert!(d1.facts.iter().any(|f| f.0.contains("Системный") && f.1 == "Нет"));
        assert_eq!(conn_name("Bluetooth Device (Personal Area Network)"), "Bluetooth");
    }

    #[test]
    fn driver_date_is_converted_to_russian_order() {
        assert_eq!(ru_date("10-19-2026"), "19.10.2026");
        assert_eq!(ru_date("garbage"), "garbage");
    }
}
