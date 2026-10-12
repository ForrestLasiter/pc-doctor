use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

mod update;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const LONG_TIMEOUT: Duration = Duration::from_secs(300);
const REPAIR_TIMEOUT: Duration = Duration::from_secs(2700);

// How a check's fix runs, so the UI knows what's safe to batch:
//   "auto"    - an automatic software fix; safe to run in "Fix All".
//   "guided"  - opens a Windows tool/website for the user; never batched.
//   "manual"  - disruptive (network drop, screen blackout); needs explicit
//               confirmation and is never batched.
//   "restore" - the restore-point step, handled specially before other fixes.
const FIX_AUTO: &str = "auto";
const FIX_GUIDED: &str = "guided";
const FIX_MANUAL: &str = "manual";
const FIX_RESTORE: &str = "restore";

// Shared list of DirectX and GPU-vendor shader cache folders, summed to MB.
const SHADER_CACHE_SIZE_SCRIPT: &str = r#"
    $paths = @(
        "$env:LOCALAPPDATA\D3DSCache",
        "$env:LOCALAPPDATA\NVIDIA\DXCache",
        "$env:LOCALAPPDATA\NVIDIA\GLCache",
        "$env:LOCALAPPDATA\AMD\DxCache",
        "$env:LOCALAPPDATA\AMD\DX9Cache"
    )
    $bytes = 0
    foreach ($p in $paths) {
        if (Test-Path $p) {
            $s = (Get-ChildItem -Path $p -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
            if ($s) { $bytes += $s }
        }
    }
    [math]::Round($bytes / 1MB, 1)
"#;

fn active_pids() -> &'static Mutex<HashSet<u32>> {
    static ACTIVE_PIDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    ACTIVE_PIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

// Serializes all history-file access so concurrent spawn_blocking tasks can't
// interleave an append with another task's read/trim and lose entries.
fn history_mutex() -> &'static Mutex<()> {
    static HISTORY_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    HISTORY_MUTEX.get_or_init(|| Mutex::new(()))
}

fn kill_pid(pid: u32) {
    let _ = Command::new("taskkill.exe")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

fn kill_all_active_powershell() {
    let pids: Vec<u32> = active_pids().lock().unwrap().drain().collect();
    for pid in pids {
        kill_pid(pid);
    }
}

#[derive(Serialize, Clone)]
struct CheckInfo {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    fix_mode: &'static str,
}

#[derive(Serialize)]
struct ScanResult {
    // "ok" | "issue" | "action" | "unavailable" | "error"
    //   ok          - healthy, nothing to do
    //   issue       - a real problem was detected
    //   action      - an optional action is available (not a detected problem)
    //   unavailable - the check doesn't apply or couldn't read state
    //   error       - the check failed to run
    status: String,
    detail: String,
}

#[derive(Serialize)]
struct FixResult {
    success: bool,
    output: String,
}

fn checks() -> Vec<CheckInfo> {
    vec![
        CheckInfo {
            id: "restore_point",
            name: "System Restore Point",
            description: "Creates a restore point so any changes made here can be undone.",
            fix_mode: FIX_RESTORE,
        },
        CheckInfo {
            id: "temp_files",
            name: "Temporary Files",
            description: "Checks how much space is used by temp files and clears them.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "disk_space",
            name: "Low Disk Space (C:)",
            description: "Checks free space on the C: drive and runs disk cleanup if low.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "dns_cache",
            name: "DNS Cache",
            description: "Flushes the DNS resolver cache, useful for connectivity issues.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "windows_update_service",
            name: "Windows Update Service",
            description: "Checks if the Windows Update service is running and restarts it if not.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "print_spooler",
            name: "Print Spooler",
            description: "Checks if the Print Spooler service is running and restarts it if stuck.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "winsock_reset",
            name: "Network Stack Reset",
            description: "Resets Winsock catalog to fix network connectivity issues. May require a restart.",
            fix_mode: FIX_MANUAL,
        },
        CheckInfo {
            id: "windows_update_cache",
            name: "Windows Update Cache",
            description: "Checks for a bloated or corrupt Windows Update download cache and clears it.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "defender_scan",
            name: "Antivirus Quick Scan",
            description: "Checks Windows Defender status and runs a quick scan for malware.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "startup_programs",
            name: "Startup Programs",
            description: "Lists apps launching at startup that may be slowing down boot time. Opens Task Manager for you to review — nothing is disabled automatically.",
            fix_mode: FIX_GUIDED,
        },
        CheckInfo {
            id: "steam_cache",
            name: "Steam Download Cache",
            description: "Checks the size of Steam's cached web and download data and clears it if large. Fixes many stalled downloads and store-page glitches. Steam rebuilds these automatically.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "steam_reset",
            name: "Steam Won't Open or Log In",
            description: "Closes Steam and resets its local client registry — the most common fix for Steam failing to launch, freezing on login, or showing a blank window. You'll need to log back in afterward.",
            fix_mode: FIX_GUIDED,
        },
        CheckInfo {
            id: "epic_cache",
            name: "Epic Games Launcher Cache",
            description: "Checks the size of the Epic Games Launcher's web cache and clears it if large. Fixes a blank/white launcher window, a missing game library, or stuck downloads.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "epic_launcher_reset",
            name: "Epic Games Launcher Frozen",
            description: "Closes stuck Epic Games processes (launcher, web helper, overlay) and reopens the launcher. Fixes a frozen or unresponsive launcher.",
            fix_mode: FIX_GUIDED,
        },
        CheckInfo {
            id: "system_file_check",
            name: "Corrupted System Files",
            description: "Checks Windows' protected system files for corruption — a common cause of random crashes, missing DLL errors, and apps that won't start. The fix runs the DISM and SFC repair in the background (10-30 minutes) and reports the result in History, so you don't have to wait.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "gpu_shader_cache",
            name: "Graphics Shader Cache",
            description: "Checks the size of the DirectX and graphics-card shader caches and clears them. A common fix for stuttering, texture glitches, or flickering, especially after a driver update. The cache rebuilds automatically.",
            fix_mode: FIX_AUTO,
        },
        CheckInfo {
            id: "gpu_driver_health",
            name: "Graphics Driver Health",
            description: "Checks your graphics card for driver errors (like Code 43), a missing or generic display driver, or a very old driver. The fix opens the right place to get the correct driver.",
            fix_mode: FIX_GUIDED,
        },
        CheckInfo {
            id: "gpu_restart_driver",
            name: "Restart Graphics Driver",
            description: "Restarts the graphics driver to recover from a frozen screen, black screen, or visual artifacts. Your screen will go black for a second or two. Available as a manual fix.",
            fix_mode: FIX_MANUAL,
        },
        CheckInfo {
            id: "disk_health",
            name: "Disk Health (SMART)",
            description: "Reads each drive's built-in health status and warns you if a drive may be failing, so you can back up your files before you lose them.",
            fix_mode: FIX_GUIDED,
        },
        CheckInfo {
            id: "device_problems",
            name: "Hardware Device Problems",
            description: "Scans all connected components and built-in hardware for devices Windows can't use — usually a missing or broken driver — and helps you get them working.",
            fix_mode: FIX_GUIDED,
        },
        CheckInfo {
            id: "battery_health",
            name: "Battery Health",
            description: "On laptops, checks how much of the battery's original capacity has been lost to wear. Skipped automatically on desktops.",
            fix_mode: FIX_GUIDED,
        },
        CheckInfo {
            id: "memory_check",
            name: "Memory (RAM)",
            description: "Reports your installed memory and checks Windows' logs for recent hardware errors. Can launch the built-in Windows Memory Diagnostic to test for faulty RAM.",
            fix_mode: FIX_GUIDED,
        },
    ]
}

fn run_ps(script: &str) -> (bool, String) {
    run_ps_with_timeout(script, DEFAULT_TIMEOUT)
}

fn run_ps_with_timeout(script: &str, timeout: Duration) -> (bool, String) {
    let child = match Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return (false, format!("Failed to launch powershell: {}", e)),
    };

    let pid = child.id();
    active_pids().lock().unwrap().insert(pid);

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    let result = match rx.recv_timeout(timeout) {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let combined = if stderr.is_empty() {
                stdout
            } else {
                format!("{}\n{}", stdout, stderr)
            };
            (out.status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("Failed to run powershell: {}", e)),
        Err(_) => {
            kill_pid(pid);
            (
                false,
                format!(
                    "Timed out after {}s. This can happen on the first run if a required PowerShell module needs to be downloaded over a slow connection — try again once it's installed.",
                    timeout.as_secs()
                ),
            )
        }
    };

    active_pids().lock().unwrap().remove(&pid);
    result
}

/// Runs a scan script that prints a single `status|detail` line and turns it
/// into a ScanResult. Recognized statuses are "issue", "action" and
/// "unavailable"; anything else is treated as "ok".
fn status_detail_scan(script: &str, timeout: Duration) -> ScanResult {
    let (ok, out) = run_ps_with_timeout(script, timeout);
    if !ok {
        return ScanResult {
            status: "error".into(),
            detail: out,
        };
    }
    let trimmed = out.trim();
    let (status, detail) = trimmed.split_once('|').unwrap_or(("ok", trimmed));
    ScanResult {
        status: normalize_status(status),
        detail: detail.trim().into(),
    }
}

fn normalize_status(raw: &str) -> String {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("issue") {
        "issue".into()
    } else if s.eq_ignore_ascii_case("action") {
        "action".into()
    } else if s.eq_ignore_ascii_case("unavailable") {
        "unavailable".into()
    } else {
        "ok".into()
    }
}

/// Turns a fix script's process result into a FixResult. Scripts that emit a
/// `RESULT|success|msg` or `RESULT|failure|msg` line get an accurate verdict
/// even when PowerShell itself exits 0 after catching an error. Scripts that
/// don't use the marker fall back to the process exit status.
fn parse_fix_output(ps_ok: bool, out: String) -> FixResult {
    if !ps_ok {
        return FixResult {
            success: false,
            output: out,
        };
    }
    let trimmed = out.trim();
    if let Some(rest) = trimmed.strip_prefix("RESULT|") {
        if let Some((status, msg)) = rest.split_once('|') {
            return FixResult {
                success: status.trim().eq_ignore_ascii_case("success"),
                output: msg.trim().to_string(),
            };
        }
    }
    FixResult {
        success: true,
        output: out,
    }
}

fn history_path() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    let dir = PathBuf::from(base).join("PCDoctor");
    fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history.log"))
}

#[tauri::command]
fn list_checks() -> Vec<CheckInfo> {
    checks()
}

/// Reports whether the app is running with administrator rights. Most repairs
/// (service control, DISM/SFC, cache in system folders) need elevation; the UI
/// uses this to warn the user and offer a "restart as administrator" path.
#[tauri::command]
async fn check_elevation() -> bool {
    tauri::async_runtime::spawn_blocking(|| {
        let script = "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)";
        let (ok, out) = run_ps(script);
        ok && out.trim().eq_ignore_ascii_case("True")
    })
    .await
    .unwrap_or(false)
}

// The installer found by the last update check. The page only ever names the
// step; what is downloaded and run is decided here, from GitHub's answer.
fn latest_installer() -> &'static Mutex<Option<update::Installer>> {
    static LATEST: OnceLock<Mutex<Option<update::Installer>>> = OnceLock::new();
    LATEST.get_or_init(|| Mutex::new(None))
}

fn current_version() -> String {
    // Development builds can pretend to be an old version, to see an update offered.
    if cfg!(debug_assertions) {
        if let Ok(v) = std::env::var("PCDOCTOR_PRETEND_VERSION") {
            return v;
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
fn app_version() -> String {
    current_version()
}

/// Asks GitHub whether a newer PC Doctor has been released. Never downloads
/// or installs anything by itself.
#[tauri::command]
async fn check_update() -> Result<update::UpdateInfo, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let (installer, info) = update::check_latest(&current_version())?;
        *latest_installer().lock().unwrap_or_else(|p| p.into_inner()) =
            info.available.then_some(installer);
        Ok(info)
    })
    .await
    .unwrap_or_else(|_| Err("Internal error checking for updates.".into()))
}

/// Downloads the update found by `check_update`, verifies it, starts the
/// installer and closes PC Doctor so its files can be replaced.
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<String, String> {
    if cfg!(debug_assertions) {
        return Err(
            "This copy is running from source, so it cannot install an update over itself.".into(),
        );
    }
    let installer = latest_installer()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .ok_or("There is no update to install. Check for updates first.")?;
    let version = installer.version.clone();
    let path = tauri::async_runtime::spawn_blocking(move || update::download_installer(&installer))
        .await
        .unwrap_or_else(|_| Err("Internal error downloading the update.".into()))?;
    Command::new(&path)
        .spawn()
        .map_err(|e| format!("The installer could not be started: {}", e))?;
    log_event_blocking(format!(
        "Update: started the installer for version {}.",
        version
    ));
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        kill_all_active_powershell();
        app.exit(0);
    });
    Ok(format!(
        "Starting the installer for version {} and closing PC Doctor...",
        version
    ))
}

const HISTORY_MAX_LINES: usize = 500;

fn log_event_blocking(text: String) -> bool {
    let Some(path) = history_path() else {
        return false;
    };
    // Hold the lock across the append AND the read/trim so two concurrent
    // callers can't lose each other's lines.
    let _guard = history_mutex().lock().unwrap_or_else(|p| p.into_inner());
    // Native timestamp - the old code spawned a whole PowerShell process just
    // to run Get-Date, adding ~200ms of overhead to every logged event.
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{}] {}\n", ts, text);
    let ok = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => f.write_all(line.as_bytes()).is_ok(),
        Err(_) => false,
    };
    // Keep the log from growing unbounded: trim to the newest entries.
    if ok {
        if let Ok(content) = fs::read_to_string(&path) {
            let lines: Vec<&str> = content.lines().collect();
            if lines.len() > HISTORY_MAX_LINES {
                let keep = lines[lines.len() - HISTORY_MAX_LINES..].join("\n") + "\n";
                let _ = fs::write(&path, keep);
            }
        }
    }
    ok
}

#[tauri::command]
async fn log_event(text: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || log_event_blocking(text))
        .await
        .unwrap_or(false)
}

#[tauri::command]
async fn get_history() -> String {
    tauri::async_runtime::spawn_blocking(|| {
        let _guard = history_mutex().lock().unwrap_or_else(|p| p.into_inner());
        match history_path() {
            Some(path) => fs::read_to_string(&path).unwrap_or_default(),
            None => String::new(),
        }
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
async fn scan_check(id: String) -> ScanResult {
    tauri::async_runtime::spawn_blocking(move || scan_check_blocking(id))
        .await
        .unwrap_or(ScanResult {
            status: "error".into(),
            detail: "Internal error running the check.".into(),
        })
}

fn scan_check_blocking(id: String) -> ScanResult {
    match id.as_str() {
        "temp_files" => {
            let script = r#"
                $bytes = (Get-ChildItem -Path $env:TEMP -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                if (-not $bytes) { $bytes = 0 }
                [math]::Round($bytes / 1MB, 1)
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let mb: f64 = out.trim().parse().unwrap_or(0.0);
            if mb > 100.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{:.1} MB of temp files found.", mb),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("Only {:.1} MB of temp files. Nothing to clean.", mb),
                }
            }
        }
        "disk_space" => {
            let script = r#"
                $d = Get-PSDrive C
                $cap = $d.Free + $d.Used
                $freePct = if ($cap -gt 0) { [math]::Round(($d.Free / $cap) * 100, 1) } else { 100 }
                $freeGB = [math]::Round($d.Free / 1GB, 1)
                "$freePct,$freeGB"
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let parts: Vec<&str> = out.trim().split(',').collect();
            let free_pct: f64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(100.0);
            let free_gb: f64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            if free_pct < 10.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("Only {:.1}% ({:.1} GB) free on C:.", free_pct, free_gb),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("{:.1}% ({:.1} GB) free on C:.", free_pct, free_gb),
                }
            }
        }
        "dns_cache" => ScanResult {
            status: "action".into(),
            detail: "The DNS cache can be flushed any time to resolve connectivity issues. No problem detected.".into(),
        },
        "windows_update_service" => {
            let (ok, out) = run_ps("$s = Get-Service -Name wuauserv -ErrorAction SilentlyContinue; if ($s) { $s.Status } else { 'NotFound' }");
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let status = out.trim();
            if status.eq_ignore_ascii_case("Running") {
                ScanResult {
                    status: "ok".into(),
                    detail: "Windows Update service is running.".into(),
                }
            } else if status.eq_ignore_ascii_case("NotFound") {
                ScanResult {
                    status: "issue".into(),
                    detail: "Windows Update service isn't present. The fix will try to start it."
                        .into(),
                }
            } else {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("Windows Update service status: {}", status),
                }
            }
        }
        "print_spooler" => {
            let (ok, out) = run_ps("$s = Get-Service -Name Spooler -ErrorAction SilentlyContinue; if ($s) { $s.Status } else { 'NotFound' }");
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let status = out.trim();
            if status.eq_ignore_ascii_case("Running") {
                ScanResult {
                    status: "ok".into(),
                    detail: "Print Spooler service is running.".into(),
                }
            } else if status.eq_ignore_ascii_case("NotFound") {
                ScanResult {
                    status: "issue".into(),
                    detail: "Print Spooler service isn't present. The fix will try to start it."
                        .into(),
                }
            } else {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("Print Spooler service status: {}", status),
                }
            }
        }
        "winsock_reset" => ScanResult {
            status: "action".into(),
            detail: "Available as a manual fix for persistent network issues. Resets network settings and requires a restart afterward.".into(),
        },
        "restore_point" => ScanResult {
            status: "action".into(),
            detail: "No restore point created yet this session. One is created automatically before any fix runs.".into(),
        },
        "windows_update_cache" => {
            // Measure the download cache directly (fast). We intentionally do NOT
            // run `dism AnalyzeComponentStore` here - it can take 30-60s and would
            // slow every scan. The fix still reclaims the component store via
            // StartComponentCleanup, so deeper cleanup happens when the user acts.
            let script = r#"
                $path = "$env:windir\SoftwareDistribution\Download"
                $bytes = (Get-ChildItem -Path $path -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                if (-not $bytes) { $bytes = 0 }
                [math]::Round($bytes / 1MB, 1)
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let mb: f64 = out.trim().parse().unwrap_or(0.0);
            if mb > 500.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{:.1} MB of old Windows Update files. Cleaning also reclaims superseded updates from the component store.", mb),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("Only {:.1} MB in the download cache.", mb),
                }
            }
        }
        "defender_scan" => {
            let script = r#"
                $s = Get-MpComputerStatus -ErrorAction SilentlyContinue
                if ($null -eq $s) { "unavailable" }
                elseif (-not $s.RealTimeProtectionEnabled) { "disabled" }
                else {
                    $age = (Get-Date) - $s.AntivirusSignatureLastUpdated
                    if ($age.TotalDays -gt 7) { "stale" } else { "ok" }
                }
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            match out.trim() {
                "ok" => ScanResult {
                    status: "ok".into(),
                    detail: "Real-time protection is on and virus definitions are current.".into(),
                },
                "disabled" => ScanResult {
                    status: "issue".into(),
                    detail: "Real-time protection is off. A quick scan is still available.".into(),
                },
                "stale" => ScanResult {
                    status: "issue".into(),
                    detail: "Virus definitions are more than 7 days old. Running a quick scan."
                        .into(),
                },
                _ => ScanResult {
                    status: "unavailable".into(),
                    detail: "Windows Defender isn't active on this PC (another antivirus may be managing protection). Its quick scan isn't available.".into(),
                },
            }
        }
        "startup_programs" => {
            let (ok, out) = run_ps("(Get-CimInstance Win32_StartupCommand | Measure-Object).Count");
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let count: i32 = out.trim().parse().unwrap_or(0);
            if count > 8 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!(
                        "{} apps launch at startup. Review them in Task Manager.",
                        count
                    ),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!(
                        "{} apps launch at startup. That's a reasonable number.",
                        count
                    ),
                }
            }
        }
        "steam_cache" => {
            let script = r#"
                $steamPath = $null
                try { $steamPath = (Get-ItemProperty "HKCU:\Software\Valve\Steam" -ErrorAction Stop).SteamPath } catch {}
                if (-not $steamPath) {
                    try { $steamPath = (Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Valve\Steam" -ErrorAction Stop).InstallPath } catch {}
                }
                if (-not $steamPath -or -not (Test-Path $steamPath)) {
                    "NOTFOUND"
                } else {
                    $folders = "appcache\httpcache", "appcache\stats", "depotcache"
                    $bytes = 0
                    foreach ($f in $folders) {
                        $p = Join-Path $steamPath $f
                        if (Test-Path $p) {
                            $sum = (Get-ChildItem -Path $p -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                            if ($sum) { $bytes += $sum }
                        }
                    }
                    [math]::Round($bytes / 1MB, 1)
                }
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let trimmed = out.trim();
            if trimmed == "NOTFOUND" {
                return ScanResult {
                    status: "unavailable".into(),
                    detail: "Steam isn't installed on this PC.".into(),
                };
            }
            let mb: f64 = trimmed.parse().unwrap_or(0.0);
            if mb > 200.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{:.1} MB of Steam cache found.", mb),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("Only {:.1} MB of Steam cache. Nothing to clean.", mb),
                }
            }
        }
        "steam_reset" => {
            let script = r#"
                $p = $null
                try { $p = (Get-ItemProperty "HKCU:\Software\Valve\Steam" -ErrorAction Stop).SteamPath } catch {}
                if (-not $p) { try { $p = (Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Valve\Steam" -ErrorAction Stop).InstallPath } catch {} }
                if (-not $p -or -not (Test-Path $p)) { "NOTFOUND" } else { "FOUND" }
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            if out.trim() == "NOTFOUND" {
                ScanResult {
                    status: "unavailable".into(),
                    detail: "Steam isn't installed on this PC.".into(),
                }
            } else {
                ScanResult {
                    status: "action".into(),
                    detail: "Available as a manual fix if Steam won't launch, freezes while logging in, or shows a blank window.".into(),
                }
            }
        }
        "epic_cache" => {
            // Presence of a leftover webcache folder does NOT prove Epic is
            // installed. Confirm installation via the launcher binary first,
            // then measure the cache.
            let script = r#"
                $installed = $false
                $exePaths = @(
                    "${env:ProgramFiles(x86)}\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe",
                    "$env:ProgramFiles\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe"
                )
                foreach ($p in $exePaths) { if (Test-Path $p) { $installed = $true; break } }
                if (-not $installed) {
                    "NOTINSTALLED"
                } else {
                    $paths = "$env:LOCALAPPDATA\EpicGamesLauncher\Saved\webcache", "$env:LOCALAPPDATA\EpicGamesLauncher\Saved\webcache_4147"
                    $bytes = 0
                    foreach ($p in $paths) {
                        if (Test-Path $p) {
                            $sum = (Get-ChildItem -Path $p -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                            if ($sum) { $bytes += $sum }
                        }
                    }
                    [math]::Round($bytes / 1MB, 1)
                }
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let trimmed = out.trim();
            if trimmed == "NOTINSTALLED" {
                return ScanResult {
                    status: "unavailable".into(),
                    detail: "Epic Games Launcher isn't installed on this PC.".into(),
                };
            }
            let mb: f64 = trimmed.parse().unwrap_or(0.0);
            if mb > 50.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{:.1} MB of Epic Games Launcher cache found.", mb),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("Only {:.1} MB of launcher cache. Nothing to clean.", mb),
                }
            }
        }
        "epic_launcher_reset" => {
            // Only the launcher binary proves installation - a leftover
            // %LOCALAPPDATA%\EpicGamesLauncher folder can survive an uninstall.
            let script = r#"
                $exePaths = @(
                    "${env:ProgramFiles(x86)}\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe",
                    "$env:ProgramFiles\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe"
                )
                $found = $false
                foreach ($p in $exePaths) { if (Test-Path $p) { $found = $true; break } }
                if ($found) { "FOUND" } else { "NOTFOUND" }
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            if out.trim() == "NOTFOUND" {
                ScanResult {
                    status: "unavailable".into(),
                    detail: "Epic Games Launcher isn't installed on this PC.".into(),
                }
            } else {
                ScanResult {
                    status: "action".into(),
                    detail: "Available as a manual fix if the Epic Games Launcher is frozen, blank, or won't open.".into(),
                }
            }
        }
        "system_file_check" => {
            // Localization-safe: rely on DISM's exit code, not English output
            // strings. `/CheckHealth` exits 0 when the component store is healthy
            // and non-zero when corruption (repairable or not) is detected.
            let script = r#"
                $ProgressPreference = 'SilentlyContinue'
                & dism.exe /Online /Cleanup-Image /CheckHealth 2>&1 | Out-Null
                if ($LASTEXITCODE -eq 0) {
                    "ok|No system file corruption detected."
                } else {
                    "issue|Windows component store corruption detected (DISM code $LASTEXITCODE). Repair is available."
                }
            "#;
            status_detail_scan(script, LONG_TIMEOUT)
        }
        "gpu_shader_cache" => {
            let script = SHADER_CACHE_SIZE_SCRIPT;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult {
                    status: "error".into(),
                    detail: out,
                };
            }
            let mb: f64 = out.trim().parse().unwrap_or(0.0);
            if mb > 100.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!(
                        "{:.1} MB of shader cache. Clearing it can fix stuttering or visual glitches.",
                        mb
                    ),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("Only {:.1} MB of shader cache. Nothing to clear.", mb),
                }
            }
        }
        "gpu_driver_health" => {
            let script = r#"
                $status = "ok"
                $detail = "Graphics driver looks healthy."
                $vcs = @(Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue)
                $bad = Get-PnpDevice -Class Display -ErrorAction SilentlyContinue |
                    Where-Object { $_.ConfigManagerErrorCode -ne $null -and $_.ConfigManagerErrorCode -ne 0 } |
                    Select-Object -First 1
                $basic = $vcs | Where-Object { $_.Name -match "Basic Display|Standard VGA" } | Select-Object -First 1
                if ($bad) {
                    $status = "issue"
                    $detail = "Your graphics card reports a driver error (Code $($bad.ConfigManagerErrorCode)). Reinstalling the driver usually fixes it."
                } elseif ($basic) {
                    $status = "issue"
                    $detail = "Windows is using a basic display driver - your graphics card's full driver isn't installed. Installing it fixes display problems and unlocks full performance."
                } else {
                    $vc = $vcs | Where-Object { $_.DriverDate } | Sort-Object DriverDate | Select-Object -First 1
                    if ($vc -and $vc.DriverDate) {
                        $age = (Get-Date) - $vc.DriverDate
                        if ($age.TotalDays -gt 730) {
                            $status = "issue"
                            $yrs = [math]::Round($age.TotalDays / 365, 1)
                            $detail = "Your graphics driver is about $yrs years old ($($vc.Name)). A newer driver may fix glitches and improve performance."
                        } else {
                            $detail = "Graphics driver looks healthy ($($vc.Name))."
                        }
                    }
                }
                "$status|$detail"
            "#;
            status_detail_scan(script, DEFAULT_TIMEOUT)
        }
        "gpu_restart_driver" => ScanResult {
            status: "action".into(),
            detail: "Available as a manual fix if your screen freezes, goes black, or shows visual artifacts. Your screen will briefly go black.".into(),
        },
        "disk_health" => {
            let script = r#"
                $disks = Get-PhysicalDisk -ErrorAction SilentlyContinue
                if (-not $disks) { "unavailable|Couldn't read drive health on this system." }
                else {
                    $bad = $disks | Where-Object { $_.HealthStatus -and $_.HealthStatus -ne "Healthy" }
                    $total = ($disks | Measure-Object).Count
                    if ($bad) {
                        $names = ($bad | ForEach-Object { "$($_.FriendlyName) [$($_.HealthStatus)]" }) -join "; "
                        "issue|Warning: $names. Back up important files now and consider replacing the drive."
                    } else {
                        "ok|All $total drive(s) report healthy."
                    }
                }
            "#;
            status_detail_scan(script, DEFAULT_TIMEOUT)
        }
        "device_problems" => {
            let script = r#"
                $bad = Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
                    Where-Object { $_.ConfigManagerErrorCode -ne $null -and $_.ConfigManagerErrorCode -ne 0 }
                $count = ($bad | Measure-Object).Count
                if ($count -gt 0) {
                    $names = ($bad | Select-Object -First 4 | ForEach-Object {
                        if ($_.FriendlyName) { $_.FriendlyName }
                        elseif ($_.Class) { "$($_.Class) device" }
                        else { "Unknown device" }
                    }) -join ", "
                    $more = if ($count -gt 4) { " (+$($count - 4) more)" } else { "" }
                    "issue|$count device(s) aren't working, usually a missing driver: $names$more."
                } else {
                    "ok|All connected devices are working."
                }
            "#;
            status_detail_scan(script, DEFAULT_TIMEOUT)
        }
        "battery_health" => {
            let script = r#"
                $b = Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue
                if (-not $b) { "unavailable|No battery detected (this looks like a desktop)." }
                else {
                    $static = Get-CimInstance -Namespace root\wmi -Class BatteryStaticData -ErrorAction SilentlyContinue | Select-Object -First 1
                    $full = Get-CimInstance -Namespace root\wmi -Class BatteryFullChargedCapacity -ErrorAction SilentlyContinue | Select-Object -First 1
                    if ($static -and $full -and $static.DesignedCapacity -gt 0) {
                        $wear = [math]::Round((1 - ($full.FullChargedCapacity / $static.DesignedCapacity)) * 100, 0)
                        if ($wear -lt 0) { $wear = 0 }
                        if ($wear -ge 30) {
                            "issue|Your battery has lost about $wear% of its original capacity. It may not hold a charge well - consider replacing it."
                        } else {
                            "ok|Battery health is good (about $wear% capacity lost to wear)."
                        }
                    } else {
                        "ok|Battery detected. Detailed wear data isn't available from this laptop."
                    }
                }
            "#;
            status_detail_scan(script, DEFAULT_TIMEOUT)
        }
        "memory_check" => {
            let script = r#"
                $cs = Get-CimInstance Win32_ComputerSystem
                $totalGB = [math]::Round($cs.TotalPhysicalMemory / 1GB, 1)
                $mods = Get-CimInstance Win32_PhysicalMemory -ErrorAction SilentlyContinue
                $slotsUsed = ($mods | Measure-Object).Count
                $arr = Get-CimInstance Win32_PhysicalMemoryArray -ErrorAction SilentlyContinue | Select-Object -First 1
                $slotsTotal = if ($arr) { $arr.MemoryDevices } else { $slotsUsed }
                $speed = ($mods | Select-Object -First 1).Speed
                $summary = "$totalGB GB RAM, $slotsUsed of $slotsTotal slots used at $speed MHz."
                $whea = $null
                try {
                    $whea = Get-WinEvent -FilterHashtable @{LogName="System"; ProviderName="Microsoft-Windows-WHEA-Logger"; Level=1,2; StartTime=(Get-Date).AddDays(-14)} -MaxEvents 20 -ErrorAction Stop
                } catch {}
                $wheaCount = ($whea | Measure-Object).Count
                if ($wheaCount -gt 0) {
                    "issue|$summary Windows logged $wheaCount hardware error(s) in the last 2 weeks - worth running a memory test."
                } else {
                    "ok|$summary No recent hardware errors logged."
                }
            "#;
            status_detail_scan(script, DEFAULT_TIMEOUT)
        }
        _ => ScanResult {
            status: "error".into(),
            detail: "Unknown check.".into(),
        },
    }
}

#[tauri::command]
async fn fix_check(id: String) -> FixResult {
    tauri::async_runtime::spawn_blocking(move || fix_check_blocking(id))
        .await
        .unwrap_or(FixResult {
            success: false,
            output: "Internal error running the fix.".into(),
        })
}

/// Nudges Windows Update to look for (and, per the OS's own settings, download
/// and install) updates in the background. Returns immediately - it does NOT
/// wait for updates to download or install; Windows does that on its own
/// schedule. This is an explicit user action, not part of scanning.
#[tauri::command]
async fn trigger_windows_update() -> FixResult {
    tauri::async_runtime::spawn_blocking(|| {
        let script = r#"
            $ok = $false
            try {
                (New-Object -ComObject Microsoft.Update.AutoUpdate).DetectNow()
                $ok = $true
            } catch {}
            # Belt-and-suspenders nudge to the Update Orchestrator (fire-and-forget).
            try {
                Start-Process -FilePath "$env:windir\System32\UsoClient.exe" -ArgumentList "StartScan" -WindowStyle Hidden -ErrorAction SilentlyContinue
            } catch {}
            if ($ok) {
                "RESULT|success|Windows Update is now looking for updates in the background. Windows will download and install them on its own schedule - you don't need to wait here."
            } else {
                "RESULT|failure|Couldn't ask Windows Update to check for updates. Make sure the Windows Update service is running and try again."
            }
        "#;
        let (ok, out) = run_ps(script);
        parse_fix_output(ok, out)
    })
    .await
    .unwrap_or(FixResult {
        success: false,
        output: "Internal error triggering Windows Update.".into(),
    })
}

fn fix_check_blocking(id: String) -> FixResult {
    match id.as_str() {
        "temp_files" => {
            let script = r#"
                $ProgressPreference = 'SilentlyContinue'
                $before = (Get-ChildItem -Path $env:TEMP -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                Get-ChildItem -Path $env:TEMP -Recurse -Force -ErrorAction SilentlyContinue |
                    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
                $after = (Get-ChildItem -Path $env:TEMP -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                if (-not $before) { $before = 0 }
                if (-not $after) { $after = 0 }
                $freedMB = [math]::Round(($before - $after) / 1MB, 1)
                if ($freedMB -lt 0) { $freedMB = 0 }
                $leftMB = [math]::Round($after / 1MB, 1)
                if ($leftMB -gt 50) {
                    "RESULT|failure|Cleared $freedMB MB, but $leftMB MB of temp files are still in use by running apps. Close open programs and try again."
                } else {
                    "RESULT|success|Cleared $freedMB MB of temp files."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "disk_space" => {
            // `cleanmgr /sagerun:1` only cleans categories pre-selected with
            // `/sageset:1`, which never runs - so it frees nothing. Instead empty
            // the Recycle Bin and temp files directly (reliable), report how much
            // was freed, and open Storage settings for deeper cleanup.
            let script = r#"
                $ProgressPreference = 'SilentlyContinue'
                $before = (Get-PSDrive C).Free
                Clear-RecycleBin -Force -Confirm:$false -ErrorAction SilentlyContinue
                Get-ChildItem -Path $env:TEMP -Recurse -Force -ErrorAction SilentlyContinue |
                    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
                $after = (Get-PSDrive C).Free
                $freedMB = [math]::Round(($after - $before) / 1MB, 1)
                if ($freedMB -lt 0) { $freedMB = 0 }
                $opened = $true
                try { Start-Process "ms-settings:storagesense" -ErrorAction Stop } catch { $opened = $false }
                if ($opened) {
                    "RESULT|success|Freed about $freedMB MB by emptying the Recycle Bin and temp files, and opened Storage settings where you can remove more (old downloads, Windows Update files, and more)."
                } else {
                    "RESULT|success|Freed about $freedMB MB by emptying the Recycle Bin and temp files. Open Settings > System > Storage to remove more."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "dns_cache" => {
            let script = r#"
                try {
                    Clear-DnsClientCache -ErrorAction Stop
                    "RESULT|success|DNS resolver cache flushed."
                } catch {
                    & ipconfig /flushdns | Out-Null
                    if ($LASTEXITCODE -eq 0) {
                        "RESULT|success|DNS resolver cache flushed."
                    } else {
                        "RESULT|failure|Could not flush the DNS cache: $($_.Exception.Message)"
                    }
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "windows_update_service" => {
            // Preserve the service's configured startup type: only nudge it off
            // "Disabled" so it can start, and leave a correctly configured
            // service alone.
            let script = r#"
                try {
                    $svc = Get-Service -Name wuauserv -ErrorAction Stop
                    if ($svc.StartType -eq 'Disabled') {
                        Set-Service -Name wuauserv -StartupType Manual -ErrorAction SilentlyContinue
                    }
                    Restart-Service -Name wuauserv -Force -ErrorAction Stop
                    $status = (Get-Service -Name wuauserv).Status
                    "RESULT|success|Windows Update service restarted. Status: $status"
                } catch {
                    "RESULT|failure|Could not restart the Windows Update service: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "print_spooler" => {
            let script = r#"
                try {
                    $svc = Get-Service -Name Spooler -ErrorAction Stop
                    if ($svc.StartType -eq 'Disabled') {
                        Set-Service -Name Spooler -StartupType Automatic -ErrorAction SilentlyContinue
                    }
                    Restart-Service -Name Spooler -Force -ErrorAction Stop
                    $status = (Get-Service -Name Spooler).Status
                    "RESULT|success|Print Spooler restarted. Status: $status"
                } catch {
                    "RESULT|failure|Could not restart the Print Spooler: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "winsock_reset" => {
            let script = r#"
                & netsh winsock reset | Out-Null
                if ($LASTEXITCODE -eq 0) {
                    "RESULT|success|Winsock catalog reset. Restart your PC to finish applying the change."
                } else {
                    "RESULT|failure|Winsock reset failed (exit code $LASTEXITCODE). This usually needs administrator rights."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "restore_point" => {
            let script = r#"
                try {
                    Enable-ComputerRestore -Drive "$env:SystemDrive\" -ErrorAction SilentlyContinue
                    Checkpoint-Computer -Description "PC Doctor" -RestorePointType "MODIFY_SETTINGS" -ErrorAction Stop
                    "RESULT|success|Restore point created."
                } catch {
                    "RESULT|failure|Could not create a restore point: $($_.Exception.Message) (Windows allows only one per 24 hours by default, so a recent one may already exist.)"
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "windows_update_cache" => {
            // Clearing the download cache reliably means stopping *all* the
            // services that hold those files open (not just wuauserv + bits, or
            // the locked files silently survive): the Update Orchestrator and
            // Delivery Optimization keep handles too. Then reclaim superseded
            // updates from the component store, which is the larger, separate
            // "Windows Update Cleanup" bucket that a folder delete can't touch.
            let script = r#"
                $ProgressPreference = 'SilentlyContinue'
                $dl = "$env:windir\SoftwareDistribution\Download"
                $before = (Get-ChildItem -Path $dl -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                if (-not $before) { $before = 0 }

                $services = @("wuauserv","bits","usosvc","dosvc")
                foreach ($s in $services) { Stop-Service -Name $s -Force -ErrorAction SilentlyContinue }

                Remove-Item -Path "$dl\*" -Recurse -Force -ErrorAction SilentlyContinue

                foreach ($s in $services) { Start-Service -Name $s -ErrorAction SilentlyContinue }

                $after = (Get-ChildItem -Path $dl -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                if (-not $after) { $after = 0 }
                $freedMB = [math]::Round(($before - $after) / 1MB, 1)
                if ($freedMB -lt 0) { $freedMB = 0 }

                # Reclaim superseded updates from the component store (the
                # "Windows Update Cleanup" item in Disk Cleanup).
                & dism.exe /Online /Cleanup-Image /StartComponentCleanup 2>&1 | Out-Null
                $storeOk = ($LASTEXITCODE -eq 0)

                if ($storeOk) {
                    "RESULT|success|Cleared $freedMB MB from the download cache and reclaimed superseded updates from the component store."
                } else {
                    "RESULT|success|Cleared $freedMB MB from the download cache. Component store cleanup didn't finish (DISM code $LASTEXITCODE) - it can be retried after a restart."
                }
            "#;
            let (ok, out) = run_ps_with_timeout(script, REPAIR_TIMEOUT);
            parse_fix_output(ok, out)
        }
        "defender_scan" => {
            // A quick scan commonly runs 1-5 minutes, so it needs the long
            // timeout - the 60s default would kill it mid-scan.
            let script = r#"
                try {
                    Start-MpScan -ScanType QuickScan -ErrorAction Stop
                    "RESULT|success|Quick scan complete. Check Windows Security for results."
                } catch {
                    "RESULT|failure|Could not run a scan: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps_with_timeout(script, LONG_TIMEOUT);
            parse_fix_output(ok, out)
        }
        "startup_programs" => {
            let script = r#"
                try {
                    Start-Process taskmgr -ErrorAction Stop
                    "RESULT|success|Task Manager opened. Go to the Startup tab to review and disable items."
                } catch {
                    "RESULT|failure|Couldn't open Task Manager: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "steam_cache" => {
            let script = r#"
                Stop-Process -Name "steam" -Force -ErrorAction SilentlyContinue
                Start-Sleep -Seconds 1
                $steamPath = $null
                try { $steamPath = (Get-ItemProperty "HKCU:\Software\Valve\Steam" -ErrorAction Stop).SteamPath } catch {}
                if (-not $steamPath) {
                    try { $steamPath = (Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Valve\Steam" -ErrorAction Stop).InstallPath } catch {}
                }
                if (-not $steamPath -or -not (Test-Path $steamPath)) {
                    "RESULT|failure|Steam installation not found."
                } else {
                    $folders = "appcache\httpcache", "appcache\stats", "depotcache"
                    foreach ($f in $folders) {
                        $p = Join-Path $steamPath $f
                        if (Test-Path $p) {
                            Remove-Item -Path "$p\*" -Recurse -Force -ErrorAction SilentlyContinue
                        }
                    }
                    "RESULT|success|Steam cache cleared. Restart Steam to rebuild it."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "steam_reset" => {
            let script = r#"
                try {
                    Stop-Process -Name "steam" -Force -ErrorAction SilentlyContinue
                    Start-Sleep -Seconds 1
                    $steamPath = $null
                    try { $steamPath = (Get-ItemProperty "HKCU:\Software\Valve\Steam" -ErrorAction Stop).SteamPath } catch {}
                    if (-not $steamPath) {
                        try { $steamPath = (Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Valve\Steam" -ErrorAction Stop).InstallPath } catch {}
                    }
                    if (-not $steamPath -or -not (Test-Path $steamPath)) {
                        "RESULT|failure|Steam installation not found."
                    } else {
                        $blob = Join-Path $steamPath "ClientRegistry.blob"
                        if (Test-Path $blob) { Remove-Item $blob -Force -ErrorAction Stop }
                        "RESULT|success|Steam's local registry was reset. Open Steam and log back in."
                    }
                } catch {
                    "RESULT|failure|Could not reset Steam: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "epic_cache" => {
            let script = r#"
                Stop-Process -Name "EpicGamesLauncher", "EpicWebHelper" -Force -ErrorAction SilentlyContinue
                Start-Sleep -Seconds 1
                $removed = $true
                foreach ($p in @("$env:LOCALAPPDATA\EpicGamesLauncher\Saved\webcache", "$env:LOCALAPPDATA\EpicGamesLauncher\Saved\webcache_4147")) {
                    if (Test-Path $p) {
                        Remove-Item -Path $p -Recurse -Force -ErrorAction SilentlyContinue
                        if (Test-Path $p) { $removed = $false }
                    }
                }
                if ($removed) {
                    "RESULT|success|Epic Games Launcher cache cleared. Restart the launcher to rebuild it."
                } else {
                    "RESULT|failure|Some launcher cache files are locked. Fully close the Epic Games Launcher and try again."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "epic_launcher_reset" => {
            let script = r#"
                Stop-Process -Name "EpicGamesLauncher", "EpicWebHelper", "EpicOnlineServicesUIHelper", "EOSOverlayRenderer-Win64-Shipping", "UnrealCEFSubProcess" -Force -ErrorAction SilentlyContinue
                Start-Sleep -Seconds 1
                $exePaths = @(
                    "${env:ProgramFiles(x86)}\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe",
                    "$env:ProgramFiles\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe"
                )
                $launched = $false
                foreach ($p in $exePaths) {
                    if (Test-Path $p) {
                        Start-Process $p
                        $launched = $true
                        break
                    }
                }
                if ($launched) {
                    "RESULT|success|Closed stuck Epic Games processes and reopened the launcher."
                } else {
                    "RESULT|failure|Closed stuck Epic Games processes, but couldn't find the launcher to reopen it - open it manually."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "system_file_check" => {
            // SFC/DISM have no OS background scheduler (unlike Windows Update), so
            // "run in the background" means launching the repair as a detached,
            // hidden process that survives even if PC Doctor is closed. It writes
            // its progress and final verdict to the PC Doctor history log, so the
            // user can close the app and check History later instead of waiting.
            //
            // The repair itself is the same robust sequence as before (WSUS bypass
            // so DISM can reach Windows Update, DISM RestoreHealth, then SFC with a
            // second pass if needed) - it just logs instead of returning a verdict.
            // DISM verdicts come from exit codes (localization-safe); SFC still
            // falls back to string matching since it has no reliable exit codes.
            let script = r#"
                $work = Join-Path $env:TEMP "pcdoctor-sfc-repair.ps1"
                $repair = @'
$ProgressPreference = "SilentlyContinue"
$logDir = Join-Path $env:LOCALAPPDATA "PCDoctor"
$log = Join-Path $logDir "history.log"
try { New-Item -ItemType Directory -Path $logDir -Force -ErrorAction SilentlyContinue | Out-Null } catch {}
function Log($m) {
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    $line = "[$ts] $m"
    for ($i = 0; $i -lt 5; $i++) {
        try { Add-Content -Path $log -Value $line -ErrorAction Stop; break } catch { Start-Sleep -Milliseconds 200 }
    }
}
Log "System file repair started in the background (this can take 10-30 minutes)."
try { Start-Service wuauserv -ErrorAction SilentlyContinue } catch {}
$auPath = "HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU"
$useWU = $null
try { $useWU = (Get-ItemProperty -Path $auPath -Name UseWUServer -ErrorAction Stop).UseWUServer } catch {}
if ($useWU -eq 1) {
    Set-ItemProperty -Path $auPath -Name UseWUServer -Value 0 -ErrorAction SilentlyContinue
    Restart-Service wuauserv -Force -ErrorAction SilentlyContinue
}
& dism.exe /Online /Cleanup-Image /RestoreHealth 2>&1 | Out-Null
$dismCode = $LASTEXITCODE
$dismOk = ($dismCode -eq 0)
if ($useWU -eq 1) {
    Set-ItemProperty -Path $auPath -Name UseWUServer -Value 1 -ErrorAction SilentlyContinue
    Restart-Service wuauserv -Force -ErrorAction SilentlyContinue
}
if ($dismOk) {
    Log "DISM: Windows component store is healthy."
} elseif ($dismCode -eq 0x800f0906 -or $dismCode -eq 0x800f0907 -or $dismCode -eq 50) {
    Log "DISM: couldn't download repair files (code $dismCode). Connect to the internet with any VPN/work network off, then run System File repair again."
} else {
    Log "DISM: repair didn't complete (code $dismCode) - check your internet connection and run it again."
}
function Get-SfcResult {
    & sfc.exe /scannow 2>&1 | Out-Null
    $code = $LASTEXITCODE
    $raw = ""
    try { $raw = Get-Content "$env:windir\Logs\CBS\CBS.log" -Tail 200 -ErrorAction SilentlyContinue | Out-String } catch {}
    $clean = ($raw -replace "[^\x20-\x7E\r\n]", "")
    if ($clean -match "did not find any integrity violations") { return "clean" }
    if ($clean -match "successfully repaired")                 { return "repaired" }
    if ($clean -match "unable to (fix|repair)")               { return "partial" }
    if ($code -eq 0)                                           { return "clean" }
    return "unknown"
}
$res = Get-SfcResult
if ($res -eq "partial" -and $dismOk) { $res = Get-SfcResult }
switch ($res) {
    "clean"    { Log "System file repair complete: no problems found - your system files are intact." }
    "repaired" { Log "System file repair complete: found and repaired corrupted files. Restart your PC to finish." }
    "partial"  { Log "System file repair: fixed what it could, but some files remain. Restart and run it again; if it persists, run it once in Safe Mode." }
    default    { Log "System file repair finished. Details are in C:\Windows\Logs\CBS\CBS.log." }
}
Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
'@
                try {
                    Set-Content -Path $work -Value $repair -Encoding UTF8 -ErrorAction Stop
                    Start-Process -FilePath "powershell.exe" -WindowStyle Hidden -ArgumentList "-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-WindowStyle","Hidden","-File","`"$work`"" -ErrorAction Stop
                    "RESULT|success|System file repair is now running in the background. It can take 10-30 minutes - you can keep using your PC or even close PC Doctor. The result will appear in History when it finishes."
                } catch {
                    "RESULT|failure|Couldn't start the background repair: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "gpu_shader_cache" => {
            let script = r#"
                $paths = @(
                    "$env:LOCALAPPDATA\D3DSCache",
                    "$env:LOCALAPPDATA\NVIDIA\DXCache",
                    "$env:LOCALAPPDATA\NVIDIA\GLCache",
                    "$env:LOCALAPPDATA\AMD\DxCache",
                    "$env:LOCALAPPDATA\AMD\DX9Cache"
                )
                $freed = 0
                foreach ($p in $paths) {
                    if (Test-Path $p) {
                        $s = (Get-ChildItem -Path $p -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                        if ($s) { $freed += $s }
                        Remove-Item -Path "$p\*" -Recurse -Force -ErrorAction SilentlyContinue
                    }
                }
                $mb = [math]::Round($freed / 1MB, 1)
                "RESULT|success|Cleared $mb MB of shader cache. It rebuilds automatically as you use apps and games."
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "gpu_driver_health" => {
            // Detect-and-guide: open the right vendor's driver download page
            // (or Windows Update if the vendor is unknown).
            let script = r#"
                $gpu = (Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue |
                    Where-Object { $_.Name } | Select-Object -First 1).Name
                if ($gpu -match "NVIDIA") {
                    $url = "https://www.nvidia.com/Download/index.aspx"; $v = "NVIDIA"
                } elseif ($gpu -match "AMD|Radeon") {
                    $url = "https://www.amd.com/en/support"; $v = "AMD"
                } elseif ($gpu -match "Intel") {
                    $url = "https://www.intel.com/content/www/us/en/download-center/home.html"; $v = "Intel"
                } else {
                    $url = "ms-settings:windowsupdate"; $v = ""
                }
                try {
                    Start-Process $url -ErrorAction Stop
                } catch {
                    "RESULT|failure|Couldn't open the driver download page: $($_.Exception.Message)"
                    return
                }
                if ($v -ne "") {
                    "RESULT|success|Opened the $v driver download page in your browser. Download and run the latest driver for your card ($gpu), then restart your PC."
                } else {
                    "RESULT|success|Opened Windows Update. Click 'Check for updates' - it can install the right graphics driver for you."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "gpu_restart_driver" => {
            // Cycle the active display adapter. Always attempt to re-enable, even
            // on error, so the user is never left without a display.
            let script = r#"
                try {
                    $dev = Get-PnpDevice -Class Display -Status OK -ErrorAction Stop | Select-Object -First 1
                    if (-not $dev) {
                        "RESULT|failure|Couldn't find an active display adapter to restart."
                    } else {
                        Disable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction Stop
                        Start-Sleep -Seconds 2
                        Enable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction Stop
                        "RESULT|success|Graphics driver restarted ($($dev.FriendlyName)). If the screen looked frozen, it should respond now."
                    }
                } catch {
                    # Make sure the adapter is back on no matter what went wrong.
                    Get-PnpDevice -Class Display -ErrorAction SilentlyContinue |
                        ForEach-Object { Enable-PnpDevice -InstanceId $_.InstanceId -Confirm:$false -ErrorAction SilentlyContinue }
                    "RESULT|failure|Couldn't fully restart the graphics driver: $($_.Exception.Message). If your screen is acting up, restart your PC."
                }
            "#;
            let (ok, out) = run_ps_with_timeout(script, LONG_TIMEOUT);
            parse_fix_output(ok, out)
        }
        "disk_health" => {
            // No software fix for a failing drive - guide the user to back up.
            let script = r#"
                try {
                    Start-Process "ms-settings:backup" -ErrorAction Stop
                    "RESULT|success|Opened Windows Backup settings. Back up your important files now. A SMART warning often appears before a drive fails - if it keeps reporting problems, replace the drive."
                } catch {
                    "RESULT|failure|Couldn't open Backup settings: $($_.Exception.Message). Open Settings > Accounts > Windows Backup manually and back up your files."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "device_problems" => {
            // Rescan for hardware (can install matching drivers from the driver
            // store) and open Device Manager so the user can act on the rest.
            let script = r#"
                & pnputil.exe /scan-devices 2>&1 | Out-Null
                try {
                    Start-Process "devmgmt.msc" -ErrorAction Stop
                    "RESULT|success|Rescanned for hardware and opened Device Manager. Look for items marked with a yellow '!', right-click one, and choose 'Update driver'. Missing drivers usually come from Windows Update or your PC/motherboard maker's support page."
                } catch {
                    "RESULT|failure|Rescanned for hardware, but couldn't open Device Manager: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps_with_timeout(script, LONG_TIMEOUT);
            parse_fix_output(ok, out)
        }
        "battery_health" => {
            // Generate Windows' detailed battery report and open it.
            let script = r#"
                $out = "$env:USERPROFILE\battery-report.html"
                & powercfg.exe /batteryreport /output "$out" 2>&1 | Out-Null
                if (Test-Path $out) {
                    Start-Process $out
                    "RESULT|success|Generated a detailed battery report and opened it ($out). The 'Battery capacity history' section shows how its capacity has dropped over time."
                } else {
                    "RESULT|failure|Couldn't generate a battery report on this device."
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        "memory_check" => {
            // Launch Windows Memory Diagnostic (it offers restart-now or
            // on-next-reboot, so the user stays in control of the reboot).
            let script = r#"
                try {
                    Start-Process "mdsched.exe" -ErrorAction Stop
                    "RESULT|success|Opened Windows Memory Diagnostic. Choose 'Restart now and check for problems' to test your RAM - your PC will reboot and run the scan. Save your work first."
                } catch {
                    "RESULT|failure|Couldn't open Windows Memory Diagnostic: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps(script);
            parse_fix_output(ok, out)
        }
        _ => FixResult {
            success: false,
            output: "Unknown check.".into(),
        },
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_checks,
            scan_check,
            fix_check,
            log_event,
            get_history,
            trigger_windows_update,
            check_elevation,
            app_version,
            check_update,
            install_update
        ])
        .on_window_event(|_window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                kill_all_active_powershell();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fix_output_failed_process_is_failure() {
        let r = parse_fix_output(false, "boom".into());
        assert!(!r.success);
        assert_eq!(r.output, "boom");
    }

    #[test]
    fn parse_fix_output_result_success_marker() {
        let r = parse_fix_output(true, "RESULT|success|All good".into());
        assert!(r.success);
        assert_eq!(r.output, "All good");
    }

    #[test]
    fn parse_fix_output_result_failure_marker() {
        // The key regression: PowerShell exits 0 but reported a failure.
        let r = parse_fix_output(true, "RESULT|failure|Could not restart the service".into());
        assert!(!r.success);
        assert_eq!(r.output, "Could not restart the service");
    }

    #[test]
    fn parse_fix_output_marker_is_case_insensitive() {
        let r = parse_fix_output(true, "RESULT|SUCCESS|done".into());
        assert!(r.success);
        let r2 = parse_fix_output(true, "RESULT|Failure|nope".into());
        assert!(!r2.success);
    }

    #[test]
    fn parse_fix_output_no_marker_falls_back_to_exit_status() {
        let r = parse_fix_output(true, "Temp files cleared.".into());
        assert!(r.success);
        assert_eq!(r.output, "Temp files cleared.");
    }

    #[test]
    fn parse_fix_output_trims_surrounding_whitespace() {
        let r = parse_fix_output(true, "\n  RESULT|success|ok  \n".into());
        assert!(r.success);
        assert_eq!(r.output, "ok");
    }

    #[test]
    fn normalize_status_recognizes_known_values() {
        assert_eq!(normalize_status("issue"), "issue");
        assert_eq!(normalize_status(" ISSUE "), "issue");
        assert_eq!(normalize_status("action"), "action");
        assert_eq!(normalize_status("unavailable"), "unavailable");
        assert_eq!(normalize_status("ok"), "ok");
    }

    #[test]
    fn normalize_status_unknown_becomes_ok() {
        assert_eq!(normalize_status("healthy"), "ok");
        assert_eq!(normalize_status(""), "ok");
        assert_eq!(normalize_status("garbage"), "ok");
    }

    #[test]
    fn every_check_has_a_known_fix_mode() {
        for c in checks() {
            assert!(
                matches!(c.fix_mode, FIX_AUTO | FIX_GUIDED | FIX_MANUAL | FIX_RESTORE),
                "check {} has unknown fix_mode {}",
                c.id,
                c.fix_mode
            );
        }
    }

    #[test]
    fn disruptive_checks_are_not_auto_fixable() {
        // These must never run unattended in "Fix All".
        let disruptive = ["winsock_reset", "gpu_restart_driver"];
        for c in checks() {
            if disruptive.contains(&c.id) {
                assert_eq!(c.fix_mode, FIX_MANUAL, "{} should be manual", c.id);
            }
        }
    }

    #[test]
    fn guided_checks_open_ui_and_are_not_auto() {
        let guided = [
            "startup_programs",
            "gpu_driver_health",
            "disk_health",
            "device_problems",
            "battery_health",
            "memory_check",
        ];
        for c in checks() {
            if guided.contains(&c.id) {
                assert_eq!(c.fix_mode, FIX_GUIDED, "{} should be guided", c.id);
            }
        }
    }

    #[test]
    fn check_ids_are_unique() {
        let mut seen = HashSet::new();
        for c in checks() {
            assert!(seen.insert(c.id), "duplicate check id {}", c.id);
        }
    }
}
