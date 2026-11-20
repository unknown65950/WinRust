//! Настройки, правила и история в %APPDATA%\rtaskmgr (обычные текстовые файлы).
use std::{fs, io::Write, path::PathBuf};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RuleAct {
    Lower,  // понизить приоритет
    Notify, // только уведомить
}

#[derive(Clone, PartialEq, Debug)]
pub struct Rule {
    pub proc: String, // имя процесса в нижнем регистре
    pub cpu: f32,     // порог, % от всех ядер
    pub secs: u32,    // сколько секунд подряд выше порога
    pub act: RuleAct,
    pub hits: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub view: usize,
    pub sort: usize,
    pub desc: bool,
    pub pos: (i32, i32),
    pub size: (u32, u32),
    pub to_tray: bool,
    pub theme: u8, // 0 светлая, 1 тёмная, 2 как в системе
    pub etw: bool, // сеть по процессам (нужен администратор), по умолчанию выключено
    pub gpu_temp_always: bool, // читать температуру GPU и вне страницы GPU (может будить видеокарту ноутбука)
    pub mini: bool, // компактный режим по умолчанию
    pub mini_size: (u32, u32),
    pub full_size: (u32, u32),
    pub rules: Vec<Rule>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { view: 0, sort: 2, desc: true, pos: (150, 60), size: (520, 360), to_tray: true, theme: 2, etw: false, gpu_temp_always: false, mini: true, mini_size: (520, 360), full_size: (1000, 740), rules: vec![] }
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("rtaskmgr")
}

pub fn render(s: &Settings) -> String {
    let mut t = format!(
        "view={}\npsort={}\ndesc={}\nx={}\ny={}\nw={}\nh={}\nto_tray={}\ntheme={}\netw={}\ngputemp={}\nmini={}\nmw={}\nmh={}\nfw={}\nfh={}\n",
        s.view, s.sort, s.desc as u8, s.pos.0, s.pos.1, s.size.0, s.size.1, s.to_tray as u8, s.theme, s.etw as u8, s.gpu_temp_always as u8,
        s.mini as u8, s.mini_size.0, s.mini_size.1, s.full_size.0, s.full_size.1
    );
    for r in &s.rules {
        t += &format!("rule={}|{}|{}|{}|{}\n", r.proc, r.cpu, r.secs, if r.act == RuleAct::Lower { "lower" } else { "notify" }, r.hits);
    }
    t
}

pub fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    let (mut saw_mini, mut saw_mini_size, mut saw_full_size) = (false, false, false);
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "view" => s.view = v.parse().unwrap_or(s.view),
            "psort" => s.sort = v.parse().unwrap_or(s.sort), // «sort» из версии v1 имел другой смысл — не читаем
            "desc" => s.desc = v == "1",
            "x" => s.pos.0 = v.parse().unwrap_or(s.pos.0),
            "y" => s.pos.1 = v.parse().unwrap_or(s.pos.1),
            "w" => s.size.0 = v.parse().unwrap_or(s.size.0),
            "h" => s.size.1 = v.parse().unwrap_or(s.size.1),
            "to_tray" => s.to_tray = v == "1",
            "theme" => s.theme = v.parse().unwrap_or(2).min(2),
            "etw" => s.etw = v == "1",
            "gputemp" => s.gpu_temp_always = v == "1",
            "mini" => { s.mini = v == "1"; saw_mini = true; },
            "mw" => { s.mini_size.0 = v.parse().unwrap_or(s.mini_size.0); saw_mini_size = true; },
            "mh" => { s.mini_size.1 = v.parse().unwrap_or(s.mini_size.1); saw_mini_size = true; },
            "fw" => { s.full_size.0 = v.parse().unwrap_or(s.full_size.0); saw_full_size = true; },
            "fh" => { s.full_size.1 = v.parse().unwrap_or(s.full_size.1); saw_full_size = true; },
            "rule" => {
                let p: Vec<&str> = v.split('|').collect();
                if let [name, cpu, secs, act, hits] = p[..]
                    && let (Ok(cpu), Ok(secs)) = (cpu.parse(), secs.parse()) {
                        s.rules.push(Rule {
                            proc: name.to_string(),
                            cpu,
                            secs,
                            act: if act == "lower" { RuleAct::Lower } else { RuleAct::Notify },
                            hits: hits.parse().unwrap_or(0),
                        });
                    }
            }
            _ => {}
        }
    }
    // старые файлы без key mini: открываем компактным окном, но сохраняем прежний размер для полного режима
    if !saw_mini {
        s.mini = true;
        s.mini_size = Settings::default().mini_size;
        s.full_size = s.size;
    } else {
        if !saw_mini_size { s.mini_size = s.size; }
        if !saw_full_size { s.full_size = s.size; }
    }
    let clamp = |p: (u32, u32), mini: bool| {
        let (w, h) = if mini { (360, 260) } else { (640, 480) };
        (p.0.clamp(w, 4000), p.1.clamp(h, 3000))
    };
    s.mini_size = clamp(s.mini_size, true);
    s.full_size = clamp(s.full_size, false);
    s.size = if s.mini { s.mini_size } else { s.full_size };
    s.pos = (s.pos.0.clamp(-200, 6000), s.pos.1.clamp(-50, 4000));
    s
}

pub fn load() -> Settings {
    fs::read_to_string(dir().join("settings.ini")).map(|t| parse(&t)).unwrap_or_default()
}

pub fn save(s: &Settings) {
    let _ = fs::create_dir_all(dir());
    let _ = fs::write(dir().join("settings.ini"), render(s));
}

// ---------- история: одна строка в минуту, поля через ';' ----------
fn hist_path() -> PathBuf {
    dir().join("history.csv")
}

pub fn append_history(line: &str) {
    let _ = fs::create_dir_all(dir());
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(hist_path()) {
        let _ = writeln!(f, "{line}");
    }
}

/// Оставляет в файле последние `max` строк (≈ 7 суток при max = 10080).
pub fn trim_history(max: usize) {
    let Ok(t) = fs::read_to_string(hist_path()) else { return };
    let lines: Vec<&str> = t.lines().collect();
    if lines.len() > max {
        let _ = fs::write(hist_path(), lines[lines.len() - max..].join("\n") + "\n");
    }
}

/// Последние `limit` записей, новые сверху.
pub fn read_history(limit: usize) -> Vec<Vec<String>> {
    let t = fs::read_to_string(hist_path()).unwrap_or_default();
    t.lines().rev().take(limit).map(|l| l.split(';').map(str::to_string).collect()).filter(|r: &Vec<String>| r.len() >= 6).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip() {
        let mut s = Settings::default();
        s.view = 3;
        s.sort = 5;
        s.theme = 1;
        s.etw = true;
        s.gpu_temp_always = true;
        s.desc = false;
        s.rules.push(Rule { proc: "chrome.exe".into(), cpu: 50.0, secs: 60, act: RuleAct::Lower, hits: 2 });
        assert_eq!(parse(&render(&s)), s);
    }

    #[test]
    fn garbage_is_clamped_and_ignored() {
        let s = parse("w=1\nh=99999\nx=-99999\nrule=bad\nnonsense\nview=abc");
        assert_eq!(s.size, (520, 360));
        assert_eq!(s.full_size, (640, 3000));
        assert_eq!(s.pos.0, -200);
        assert!(s.rules.is_empty());
        assert_eq!(s.view, 0);
        assert!(s.mini);
    }

    #[test]
    fn old_settings_open_in_mini_and_keep_full_size() {
        let s = parse("w=846\nh=625\nview=2");
        assert!(s.mini);
        assert_eq!(s.size, (520, 360));
        assert_eq!(s.mini_size, (520, 360));
        assert_eq!(s.full_size, (846, 625));
    }
}
