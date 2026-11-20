//! topbar: the bottom bar instead of the taskbar, plus reshell hotkeys and the PowerShell profile block.
use crate::*;

pub(crate) fn bar_dir() -> PathBuf {
    program_files().join("topbar")
}

pub(crate) const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

// topbar.cfg next to topbar.exe (the bar re-reads it every few seconds). Missing file = OneDrive hidden.
pub(crate) fn cfg_path() -> PathBuf {
    bar_dir().join("topbar.cfg")
}

pub(crate) fn od_hidden() -> bool {
    fs::read_to_string(cfg_path()).map_or(true, |c| !c.contains("hide_onedrive=0"))
}

pub(crate) fn set_od_hidden(v: bool) -> Result<(), String> {
    fs::create_dir_all(bar_dir()).and_then(|_| fs::write(cfg_path(), format!("hide_onedrive={}\n", v as u8))).map_err(|e| e.to_string())
}

// PowerShell profile block (profile.ps1 next to this file: zoxide, grep->rg, ll->eza, cat->bat) that lives exactly as long as topbar is installed.
// Written to Windows PowerShell 5.1 and, if installed, pwsh 7 (each has its own $PROFILE).
// The profile is the signed-in user's (session.rs), also when UAC was answered with another administrator.
pub(crate) fn ps_strip() -> String {
    format!(r"{} $t=[string]$(if(Test-Path $p){{Get-Content $p -Raw}}); $t=[regex]::Replace($t,'(?s)# >>> topbar >>>.*?# <<< topbar <<<\r?\n?','');", profile_path())
}
pub(crate) const PS_BLOCK: &str = include_str!("profile.ps1");
pub(crate) const SHELLS: [&str; 2] = ["powershell", "pwsh"];

pub(crate) fn profile_install() -> Result<(), String> {
    shells_run(&format!("{}$b=@'\n{PS_BLOCK}'@; New-Item -Force -ItemType Directory (Split-Path $p) | Out-Null; Set-Content $p ($t.TrimEnd() + [Environment]::NewLine + $b) -Encoding utf8", ps_strip()))
}

pub(crate) fn profile_uninstall() -> Result<(), String> {
    // delete the file if nothing but our block was in it
    shells_run(&format!("{} if($t.Trim()){{Set-Content $p $t -Encoding utf8}}elseif(Test-Path $p){{Remove-Item $p}}", ps_strip()))
}

/// Runs `script` in every shell. Only the first (always present) can fail the call: a missing pwsh is not an error.
pub(crate) fn shells_run(script: &str) -> Result<(), String> {
    let r = run(SHELLS[0], &["-NoProfile", "-Command", script]);
    let _ = run(SHELLS[1], &["-NoProfile", "-Command", script]);
    r
}

pub(crate) fn bar_install() -> Result<(), String> {
    with_payload(&["topbar.exe", "rs-du.exe", "reshell.exe"], |d| {
        let dst = bar_dir().join("topbar.exe");
        let _ = run("taskkill", &["/f", "/im", "topbar.exe"]);
        fs::create_dir_all(bar_dir()).map_err(|e| e.to_string())?;
        put(&d.join("topbar.exe"), &dst)?;
        fs::create_dir_all(tm_dir()).map_err(|e| e.to_string())?;
        put(&d.join("rs-du.exe"), &tm_dir().join("rs-du.exe"))?; // topbar menu "Чистильщик диска"
        let rsh = bar_dir().join("reshell.exe");
        let _ = run("taskkill", &["/f", "/im", "reshell.exe"]);
        put(&d.join("reshell.exe"), &rsh)?;
        run("reg", &["add", RUN_KEY, "/v", "reshell", "/t", "REG_SZ", "/d", &format!("\"{}\"", rsh.display()), "/f"])?;
        let _ = Command::new("explorer.exe").arg(&rsh).spawn();
        run(dst.to_str().unwrap(), &["--restore"])?; // clean state in case a killed instance left the taskbar hidden
        // The bar runs elevated (a task with the highest run level starts it at logon without a UAC prompt): that lets it close
        // elevated windows and click elevated apps' tray icons. It starts everything else through explorer, i.e. unelevated.
        // Fallback when the task cannot be made: the plain Run key and a normal-rights bar.
        let task = format!("{} Register-ScheduledTask -TaskName topbar -Force -Action (New-ScheduledTaskAction -Execute '{}') -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $u) -Principal (New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Highest) -Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero)) | Out-Null", task_user(), dst.display());
        if run("powershell", &["-NoProfile", "-Command", &task]).is_ok() && run("schtasks", &["/run", "/tn", "topbar"]).is_ok() {
            let _ = run("reg", &["delete", RUN_KEY, "/v", "topbar", "/f"]);
        } else {
            WARN.lock().unwrap().push(t!("задача topbar не создана: запуск через Run, без прав администратора", "topbar task was not created: it starts via Run, without administrator rights").into());
            run("reg", &["add", RUN_KEY, "/v", "topbar", "/t", "REG_SZ", "/d", &format!("\"{}\"", dst.display()), "/f"])?;
            let _ = Command::new("explorer.exe").arg(&dst).spawn(); // normal user token, like rs-launcher
        }
        soft(t!("профиль PowerShell не записан", "PowerShell profile was not written"), profile_install()); // a shell nicety, must not fail the bar install
        Ok(())
    })
}

pub(crate) fn bar_uninstall() -> Result<(), String> {
    let _ = run("reg", &["delete", RUN_KEY, "/v", "topbar", "/f"]);
    let _ = run("schtasks", &["/delete", "/tn", "topbar", "/f"]);
    let _ = run("reg", &["delete", RUN_KEY, "/v", "reshell", "/f"]);
    let _ = run("taskkill", &["/f", "/im", "reshell.exe"]);
    let _ = run("taskkill", &["/f", "/im", "topbar.exe"]);
    let _ = profile_uninstall();
    let r = run(bar_dir().join("topbar.exe").to_str().unwrap(), &["--restore"]); // a killed bar cannot restore the taskbar itself
    let _ = fs::remove_dir_all(bar_dir());
    cleaner_release(tm_dir().join("rtaskmgr-win32.exe").is_file());
    r
}
