//! wiztree, photoview, rust-settings, debloat, purge: small stand-alone tools, one exe each in its own Program Files folder.
use crate::*;

// ---------- wiztree, photoview, debloat: small stand-alone tools, one exe each in its own Program Files folder ----------

pub(crate) fn wz_dir() -> PathBuf {
    program_files().join("wiztree")
}

pub(crate) fn pv_dir() -> PathBuf {
    program_files().join("photoview")
}

pub(crate) fn st_dir() -> PathBuf {
    program_files().join("rust-settings")
}

pub(crate) fn pu_dir() -> PathBuf {
    program_files().join("purge")
}

pub(crate) fn db_dir() -> PathBuf {
    program_files().join("debloat")
}

pub(crate) fn sp_dir() -> PathBuf {
    program_files().join("rs-spaces")
}

/// Copies `exe` out of the payload into `dir` (an update: the running copy is killed first) and adds the Start Menu shortcut `lnk`.
pub(crate) fn tool_install(exe: &str, dir: &Path, lnk: Option<&str>) -> Result<(), String> {
    with_payload(&[exe], |d| {
        let _ = run("taskkill", &["/f", "/im", exe]);
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let dst = dir.join(exe);
        put(&d.join(exe), &dst)?;
        if let Some(n) = lnk {
            soft(&tf!("ярлык «{n}» в меню Пуск не создан, поиск его не увидит", "Start menu shortcut '{n}' was not created, search will not find it"), shortcut(&start_lnk(n), &dst, ""));
        }
        Ok(())
    })
}

pub(crate) fn tool_remove(exe: &str, dir: &Path, lnk: Option<&str>) {
    if let Some(n) = lnk {
        let _ = fs::remove_file(start_lnk(n));
    }
    let _ = run("taskkill", &["/f", "/im", exe]);
    let _ = fs::remove_dir_all(dir);
}

pub(crate) fn st_install() -> Result<(), String> {
    tool_install("rust-settings.exe", &st_dir(), Some("WinRust Settings"))
}

pub(crate) fn st_uninstall() -> Result<(), String> {
    tool_remove("rust-settings.exe", &st_dir(), Some("WinRust Settings")); // also ends the night-light helper (same exe)
    // what the night-light switch left for the user: its autostart value and its setting
    let _ = run("reg", &["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "WinRustNightLight", "/f"]);
    let _ = run("reg", &["delete", r"HKCU\Software\WinRust\NightLight", "/f"]);
    Ok(())
}

pub(crate) fn wz_install() -> Result<(), String> {
    tool_install("wiztree.exe", &wz_dir(), Some("WinRust Disk"))
}

pub(crate) fn wz_uninstall() -> Result<(), String> {
    tool_remove("wiztree.exe", &wz_dir(), Some("WinRust Disk"));
    Ok(())
}

pub(crate) fn sp_install() -> Result<(), String> {
    with_payload(&["rs-spaces.exe"], |d| {
        let dir = sp_dir();
        let dst = dir.join("rs-spaces.exe");
        if dst.is_file() {
            let _ = run(dst.to_str().unwrap(), &["--restore"]);
        }
        let _ = run("taskkill", &["/f", "/im", "rs-spaces.exe"]);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        put(&d.join("rs-spaces.exe"), &dst)?;
        run("reg", &["add", RUN_KEY, "/v", "rs-spaces", "/t", "REG_SZ", "/d", &format!("\"{}\"", dst.display()), "/f"])?;
        let _ = Command::new("explorer.exe").arg(&dst).spawn();
        Ok(())
    })
}

pub(crate) fn sp_uninstall() -> Result<(), String> {
    let dst = sp_dir().join("rs-spaces.exe");
    if dst.is_file() {
        let _ = run(dst.to_str().unwrap(), &["--restore"]);
    }
    let _ = run("reg", &["delete", RUN_KEY, "/v", "rs-spaces", "/f"]);
    let _ = run("taskkill", &["/f", "/im", "rs-spaces.exe"]);
    let _ = fs::remove_dir_all(sp_dir());
    Ok(())
}

pub(crate) const PV_EXTS: [&str; 11] = ["jpg", "jpeg", "jfif", "png", "bmp", "gif", "tif", "tiff", "webp", "heic", "avif"]; // keep in step with photoview's EXTS
pub(crate) const PV_PROG: &str = "WinRustPhoto";
pub(crate) const PV_APP: &str = "WinRust Photo Viewer";

/// "Open with" entry and a Default apps entry; the default itself stays the user's choice (Windows does not allow setting it from code).
pub(crate) fn pv_install() -> Result<(), String> {
    tool_install("photoview.exe", &pv_dir(), Some("Photo viewer"))?;
    let exe = pv_dir().join("photoview.exe").display().to_string().replace('\\', r"\\");
    let mut reg = format!(
        "Windows Registry Editor Version 5.00\r\n\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\{PV_PROG}]\r\n@=\"Picture\"\r\n\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\{PV_PROG}\\shell\\open\\command]\r\n@=\"\\\"{exe}\\\" \\\"%1\\\"\"\r\n\r\n\
         [HKEY_LOCAL_MACHINE\\SOFTWARE\\WinRust\\PhotoView\\Capabilities]\r\n\"ApplicationName\"=\"{PV_APP}\"\r\n\"ApplicationDescription\"=\"Fast picture viewer\"\r\n\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\WinRust\\PhotoView\\Capabilities\\FileAssociations]\r\n"
    );
    for e in PV_EXTS {
        reg += &format!("\".{e}\"=\"{PV_PROG}\"\r\n");
    }
    reg += &format!("\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\RegisteredApplications]\r\n\"{PV_APP}\"=\"SOFTWARE\\\\WinRust\\\\PhotoView\\\\Capabilities\"\r\n");
    for e in PV_EXTS {
        reg += &format!("\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\.{e}\\OpenWithProgids]\r\n\"{PV_PROG}\"=hex(0):\r\n");
    }
    // the .reg lives in Program Files (admin-only), not %TEMP%
    let f = pv_dir().join("register.reg");
    fs::write(&f, reg).map_err(|e| format!("{}: {e}", f.display()))?;
    let r = run("reg", &["import", &f.display().to_string()]);
    let _ = fs::remove_file(&f);
    r
}

pub(crate) fn pv_uninstall() -> Result<(), String> {
    for e in PV_EXTS {
        let _ = run("reg", &["delete", &format!(r"HKLM\SOFTWARE\Classes\.{e}\OpenWithProgids"), "/v", PV_PROG, "/f"]);
    }
    let _ = run("reg", &["delete", r"HKLM\SOFTWARE\RegisteredApplications", "/v", PV_APP, "/f"]);
    let _ = run("reg", &["delete", &format!(r"HKLM\SOFTWARE\Classes\{PV_PROG}"), "/f"]);
    let _ = run("reg", &["delete", r"HKLM\SOFTWARE\WinRust\PhotoView", "/f"]);
    tool_remove("photoview.exe", &pv_dir(), Some("Photo viewer"));
    Ok(())
}

/// Installing = applying: debloat.exe --apply writes an undo file first, uninstalling plays it back.
pub(crate) fn db_install() -> Result<(), String> {
    tool_install("debloat.exe", &db_dir(), None)?;
    out(&db_dir().join("debloat.exe").display().to_string(), &["--apply"]).map(|_| ())
}

pub(crate) fn db_uninstall() -> Result<(), String> {
    let exe = db_dir().join("debloat.exe");
    if exe.is_file() {
        out(&exe.display().to_string(), &["--undo"])?; // on failure the exe and the undo file stay, so it can be repeated
    }
    tool_remove("debloat.exe", &db_dir(), None);
    Ok(())
}

/// Installing = removing: purge.exe --apply deletes the bundled apps and telemetry tasks FOR GOOD. Uninstalling only deletes
/// purge.exe itself; there is nothing to put back. OneDrive and Edge are not part of it (`purge.exe --apply --onedrive --edge`).
pub(crate) fn pu_install() -> Result<(), String> {
    tool_install("purge.exe", &pu_dir(), None)?;
    out(&pu_dir().join("purge.exe").display().to_string(), &["--apply"]).map(|_| ())
}

pub(crate) fn pu_uninstall() -> Result<(), String> {
    tool_remove("purge.exe", &pu_dir(), None);
    Ok(())
}
