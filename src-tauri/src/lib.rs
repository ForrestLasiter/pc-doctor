use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const LONG_TIMEOUT: Duration = Duration::from_secs(300);
const REPAIR_TIMEOUT: Duration = Duration::from_secs(2700);

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
}

#[derive(Serialize)]
struct ScanResult {
    status: String, // "ok" | "issue" | "error"
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
        },
        CheckInfo {
            id: "temp_files",
            name: "Temporary Files",
            description: "Checks how much space is used by temp files and clears them.",
        },
        CheckInfo {
            id: "disk_space",
            name: "Low Disk Space (C:)",
            description: "Checks free space on the C: drive and runs disk cleanup if low.",
        },
        CheckInfo {
            id: "dns_cache",
            name: "DNS Cache",
            description: "Flushes the DNS resolver cache, useful for connectivity issues.",
        },
        CheckInfo {
            id: "windows_updates",
            name: "Windows Updates",
            description: "Checks for available Windows updates and installs them. If the PSWindowsUpdate module isn't already present, this installs it from the PowerShell Gallery and trusts that repository system-wide.",
        },
        CheckInfo {
            id: "windows_update_service",
            name: "Windows Update Service",
            description: "Checks if the Windows Update service is running and restarts it if not.",
        },
        CheckInfo {
            id: "print_spooler",
            name: "Print Spooler",
            description: "Checks if the Print Spooler service is running and restarts it if stuck.",
        },
        CheckInfo {
            id: "winsock_reset",
            name: "Network Stack Reset",
            description: "Resets Winsock catalog to fix network connectivity issues. May require a restart.",
        },
        CheckInfo {
            id: "windows_update_cache",
            name: "Windows Update Cache",
            description: "Checks for a bloated or corrupt Windows Update download cache and clears it.",
        },
        CheckInfo {
            id: "defender_scan",
            name: "Antivirus Quick Scan",
            description: "Checks Windows Defender status and runs a quick scan for malware.",
        },
        CheckInfo {
            id: "startup_programs",
            name: "Startup Programs",
            description: "Lists apps launching at startup that may be slowing down boot time. Opens Task Manager for you to review — nothing is disabled automatically.",
        },
        CheckInfo {
            id: "steam_cache",
            name: "Steam Download Cache",
            description: "Checks the size of Steam's cached web and download data and clears it if large. Fixes many stalled downloads and store-page glitches. Steam rebuilds these automatically.",
        },
        CheckInfo {
            id: "steam_reset",
            name: "Steam Won't Open or Log In",
            description: "Closes Steam and resets its local client registry — the most common fix for Steam failing to launch, freezing on login, or showing a blank window. You'll need to log back in afterward.",
        },
        CheckInfo {
            id: "epic_cache",
            name: "Epic Games Launcher Cache",
            description: "Checks the size of the Epic Games Launcher's web cache and clears it if large. Fixes a blank/white launcher window, a missing game library, or stuck downloads.",
        },
        CheckInfo {
            id: "epic_launcher_reset",
            name: "Epic Games Launcher Frozen",
            description: "Closes stuck Epic Games processes (launcher, web helper, overlay) and reopens the launcher. Fixes a frozen or unresponsive launcher.",
        },
        CheckInfo {
            id: "system_file_check",
            name: "Corrupted System Files",
            description: "Checks Windows' protected system files for corruption — a common cause of random crashes, missing DLL errors, and apps that won't start. The fix runs DISM and SFC repair, which can take 10-30 minutes and may need internet access.",
        },
        CheckInfo {
            id: "gpu_shader_cache",
            name: "Graphics Shader Cache",
            description: "Checks the size of the DirectX and graphics-card shader caches and clears them. A common fix for stuttering, texture glitches, or flickering, especially after a driver update. The cache rebuilds automatically.",
        },
        CheckInfo {
            id: "gpu_driver_health",
            name: "Graphics Driver Health",
            description: "Checks your graphics card for driver errors (like Code 43), a missing or generic display driver, or a very old driver. The fix opens the right place to get the correct driver.",
        },
        CheckInfo {
            id: "gpu_restart_driver",
            name: "Restart Graphics Driver",
            description: "Restarts the graphics driver to recover from a frozen screen, black screen, or visual artifacts. Your screen will go black for a second or two. Available as a manual fix.",
        },
        CheckInfo {
            id: "disk_health",
            name: "Disk Health (SMART)",
            description: "Reads each drive's built-in health status and warns you if a drive may be failing, so you can back up your files before you lose them.",
        },
        CheckInfo {
            id: "device_problems",
            name: "Hardware Device Problems",
            description: "Scans all connected components and built-in hardware for devices Windows can't use — usually a missing or broken driver — and helps you get them working.",
        },
        CheckInfo {
            id: "battery_health",
            name: "Battery Health",
            description: "On laptops, checks how much of the battery's original capacity has been lost to wear. Skipped automatically on desktops.",
        },
        CheckInfo {
            id: "memory_check",
            name: "Memory (RAM)",
            description: "Reports your installed memory and checks Windows' logs for recent hardware errors. Can launch the built-in Windows Memory Diagnostic to test for faulty RAM.",
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

/// Runs a scan script that prints a single `status|detail` line (status is
/// "issue" or "ok") and turns it into a ScanResult.
fn status_detail_scan(script: &str, timeout: Duration) -> ScanResult {
    let (ok, out) = run_ps_with_timeout(script, timeout);
    if !ok {
        return ScanResult { status: "error".into(), detail: out };
    }
    let trimmed = out.trim();
    let (status, detail) = trimmed.split_once('|').unwrap_or(("ok", trimmed));
    let status = if status.trim().eq_ignore_ascii_case("issue") { "issue" } else { "ok" };
    ScanResult { status: status.into(), detail: detail.trim().into() }
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

fn log_event_blocking(text: String) -> bool {
    let Some(path) = history_path() else {
        return false;
    };
    let (_, ts) = run_ps("Get-Date -Format \"yyyy-MM-dd HH:mm:ss\"");
    let line = format!("[{}] {}\n", ts.trim(), text);
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => f.write_all(line.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

#[tauri::command]
async fn log_event(text: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || log_event_blocking(text))
        .await
        .unwrap_or(false)
}

#[tauri::command]
fn get_history() -> String {
    match history_path() {
        Some(path) => fs::read_to_string(&path).unwrap_or_default(),
        None => String::new(),
    }
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
                return ScanResult { status: "error".into(), detail: out };
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
                $freePct = [math]::Round(($d.Free / ($d.Free + $d.Used)) * 100, 1)
                $freeGB = [math]::Round($d.Free / 1GB, 1)
                "$freePct,$freeGB"
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            let parts: Vec<&str> = out.trim().split(',').collect();
            let free_pct: f64 = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(100.0);
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
            status: "issue".into(),
            detail: "DNS cache can be flushed at any time to resolve connectivity issues.".into(),
        },
        "windows_updates" => {
            let script = r#"
                try {
                    if (-not (Get-Module -ListAvailable -Name PSWindowsUpdate)) {
                        Install-PackageProvider -Name NuGet -MinimumVersion 2.8.5.201 -Force -ErrorAction SilentlyContinue | Out-Null
                        Set-PSRepository -Name PSGallery -InstallationPolicy Trusted -ErrorAction SilentlyContinue
                        Install-Module -Name PSWindowsUpdate -Force -Confirm:$false -Scope AllUsers -ErrorAction Stop
                    }
                    Import-Module PSWindowsUpdate -ErrorAction Stop
                    $updates = Get-WindowsUpdate -ErrorAction Stop
                    ($updates | Measure-Object).Count
                } catch {
                    "ERROR: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps_with_timeout(script, LONG_TIMEOUT);
            let trimmed = out.trim();
            if !ok || trimmed.starts_with("ERROR:") {
                return ScanResult { status: "error".into(), detail: trimmed.to_string() };
            }
            let count: i32 = trimmed.parse().unwrap_or(0);
            if count > 0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{} update(s) available.", count),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: "No updates available. Windows is up to date.".into(),
                }
            }
        }
        "windows_update_service" => {
            let (ok, out) = run_ps("(Get-Service -Name wuauserv).Status");
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            if out.trim().eq_ignore_ascii_case("Running") {
                ScanResult {
                    status: "ok".into(),
                    detail: "Windows Update service is running.".into(),
                }
            } else {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("Windows Update service status: {}", out.trim()),
                }
            }
        }
        "print_spooler" => {
            let (ok, out) = run_ps("(Get-Service -Name Spooler).Status");
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            if out.trim().eq_ignore_ascii_case("Running") {
                ScanResult {
                    status: "ok".into(),
                    detail: "Print Spooler service is running.".into(),
                }
            } else {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("Print Spooler service status: {}", out.trim()),
                }
            }
        }
        "winsock_reset" => ScanResult {
            status: "issue".into(),
            detail: "Available as a manual fix for persistent network issues. Requires a restart afterward.".into(),
        },
        "restore_point" => ScanResult {
            status: "issue".into(),
            detail: "No restore point created yet this session. Recommended before running other fixes.".into(),
        },
        "windows_update_cache" => {
            // Two separate sources of "Windows Update stuff": the download cache
            // (SoftwareDistribution\Download) and superseded updates kept in the
            // component store (what Disk Cleanup calls "Windows Update Cleanup").
            // Measure the first directly; ask DISM whether the second is worth
            // cleaning (best-effort, English output).
            let script = r#"
                $path = "$env:windir\SoftwareDistribution\Download"
                $bytes = (Get-ChildItem -Path $path -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                if (-not $bytes) { $bytes = 0 }
                $mb = [math]::Round($bytes / 1MB, 1)
                $recommended = "no"
                try {
                    $a = & dism.exe /Online /Cleanup-Image /AnalyzeComponentStore 2>&1 | Out-String
                    if ($a -match "Component Store Cleanup Recommended\s*:\s*Yes") { $recommended = "yes" }
                } catch {}
                "$mb|$recommended"
            "#;
            let (ok, out) = run_ps_with_timeout(script, LONG_TIMEOUT);
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            let trimmed = out.trim();
            let (mb_str, rec) = trimmed.split_once('|').unwrap_or((trimmed, "no"));
            let mb: f64 = mb_str.trim().parse().unwrap_or(0.0);
            let store_cleanup = rec.trim().eq_ignore_ascii_case("yes");
            if mb > 500.0 && store_cleanup {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{:.1} MB in the download cache, plus reclaimable space in the component store.", mb),
                }
            } else if mb > 500.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{:.1} MB of old Windows Update files found.", mb),
                }
            } else if store_cleanup {
                ScanResult {
                    status: "issue".into(),
                    detail: "Superseded Windows Update files can be reclaimed from the component store.".into(),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("Only {:.1} MB in the download cache and nothing reclaimable in the component store.", mb),
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
                return ScanResult { status: "error".into(), detail: out };
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
                    detail: "Virus definitions are more than 7 days old. Running a quick scan.".into(),
                },
                _ => ScanResult {
                    status: "ok".into(),
                    detail: "Windows Defender status unavailable (another antivirus may be active). Quick scan still available.".into(),
                },
            }
        }
        "startup_programs" => {
            let (ok, out) = run_ps("(Get-CimInstance Win32_StartupCommand | Measure-Object).Count");
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            let count: i32 = out.trim().parse().unwrap_or(0);
            if count > 8 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{} apps launch at startup. Review them in Task Manager.", count),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("{} apps launch at startup. That's a reasonable number.", count),
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
                return ScanResult { status: "error".into(), detail: out };
            }
            let trimmed = out.trim();
            if trimmed == "NOTFOUND" {
                return ScanResult { status: "ok".into(), detail: "Steam isn't installed on this PC.".into() };
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
                return ScanResult { status: "error".into(), detail: out };
            }
            if out.trim() == "NOTFOUND" {
                ScanResult { status: "ok".into(), detail: "Steam isn't installed on this PC.".into() }
            } else {
                ScanResult {
                    status: "issue".into(),
                    detail: "Available as a manual fix if Steam won't launch, freezes while logging in, or shows a blank window.".into(),
                }
            }
        }
        "epic_cache" => {
            let script = r#"
                $paths = "$env:LOCALAPPDATA\EpicGamesLauncher\Saved\webcache", "$env:LOCALAPPDATA\EpicGamesLauncher\Saved\webcache_4147"
                $found = $false
                $bytes = 0
                foreach ($p in $paths) {
                    if (Test-Path $p) {
                        $found = $true
                        $sum = (Get-ChildItem -Path $p -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                        if ($sum) { $bytes += $sum }
                    }
                }
                if (-not $found) { "NOTFOUND" } else { [math]::Round($bytes / 1MB, 1) }
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            let trimmed = out.trim();
            if trimmed == "NOTFOUND" {
                return ScanResult { status: "ok".into(), detail: "No Epic Games Launcher cache found. Nothing to clean.".into() };
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
            let script = r#"
                $paths = @(
                    "${env:ProgramFiles(x86)}\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe",
                    "$env:ProgramFiles\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe",
                    "$env:LOCALAPPDATA\EpicGamesLauncher"
                )
                $found = $false
                foreach ($p in $paths) { if (Test-Path $p) { $found = $true; break } }
                if ($found) { "FOUND" } else { "NOTFOUND" }
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            if out.trim() == "NOTFOUND" {
                ScanResult { status: "ok".into(), detail: "Epic Games Launcher isn't installed on this PC.".into() }
            } else {
                ScanResult {
                    status: "issue".into(),
                    detail: "Available as a manual fix if the Epic Games Launcher is frozen, blank, or won't open.".into(),
                }
            }
        }
        "system_file_check" => {
            let (ok, out) = run_ps_with_timeout("dism /Online /Cleanup-Image /CheckHealth", LONG_TIMEOUT);
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            let lower = out.to_lowercase();
            if lower.contains("repairable") || lower.contains("corruption was detected") {
                ScanResult {
                    status: "issue".into(),
                    detail: "Component store corruption detected. Repair is available.".into(),
                }
            } else if lower.contains("no component store corruption detected") {
                ScanResult {
                    status: "ok".into(),
                    detail: "No system file corruption detected.".into(),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: "Quick check didn't find a flagged issue.".into(),
                }
            }
        }
        "gpu_shader_cache" => {
            let script = SHADER_CACHE_SIZE_SCRIPT;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            let mb: f64 = out.trim().parse().unwrap_or(0.0);
            if mb > 100.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{:.1} MB of shader cache. Clearing it can fix stuttering or visual glitches.", mb),
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
                $bad = Get-PnpDevice -Class Display -ErrorAction SilentlyContinue |
                    Where-Object { $_.ConfigManagerErrorCode -ne $null -and $_.ConfigManagerErrorCode -ne 0 } |
                    Select-Object -First 1
                $basic = Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue |
                    Where-Object { $_.Name -match "Basic Display|Standard VGA" } | Select-Object -First 1
                if ($bad) {
                    $status = "issue"
                    $detail = "Your graphics card reports a driver error (Code $($bad.ConfigManagerErrorCode)). Reinstalling the driver usually fixes it."
                } elseif ($basic) {
                    $status = "issue"
                    $detail = "Windows is using a basic display driver - your graphics card's full driver isn't installed. Installing it fixes display problems and unlocks full performance."
                } else {
                    $vc = Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue |
                        Where-Object { $_.DriverDate } | Sort-Object DriverDate | Select-Object -First 1
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
            status: "issue".into(),
            detail: "Available as a manual fix if your screen freezes, goes black, or shows visual artifacts.".into(),
        },
        "disk_health" => {
            let script = r#"
                $disks = Get-PhysicalDisk -ErrorAction SilentlyContinue
                if (-not $disks) { "ok|Couldn't read drive health on this system." }
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
                if (-not $b) { "ok|No battery detected (this looks like a desktop)." }
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

fn fix_check_blocking(id: String) -> FixResult {
    match id.as_str() {
        "temp_files" => {
            let script = r#"
                Get-ChildItem -Path $env:TEMP -Recurse -Force -ErrorAction SilentlyContinue |
                    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
                "Temp files cleared."
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
        }
        "disk_space" => {
            let script = r#"
                cleanmgr /sagerun:1 | Out-Null
                "Disk cleanup launched."
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
        }
        "dns_cache" => {
            let (ok, out) = run_ps("ipconfig /flushdns");
            FixResult { success: ok, output: out }
        }
        "windows_updates" => {
            let script = r#"
                try {
                    if (-not (Get-Module -ListAvailable -Name PSWindowsUpdate)) {
                        Install-PackageProvider -Name NuGet -MinimumVersion 2.8.5.201 -Force -ErrorAction SilentlyContinue | Out-Null
                        Set-PSRepository -Name PSGallery -InstallationPolicy Trusted -ErrorAction SilentlyContinue
                        Install-Module -Name PSWindowsUpdate -Force -Confirm:$false -Scope AllUsers -ErrorAction Stop
                    }
                    Import-Module PSWindowsUpdate -ErrorAction Stop
                    Get-WindowsUpdate -AcceptAll -Install -IgnoreReboot -ErrorAction Stop | Out-String
                } catch {
                    "Could not install updates: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps_with_timeout(script, LONG_TIMEOUT);
            FixResult { success: ok, output: out }
        }
        "windows_update_service" => {
            let (ok, out) = run_ps("Restart-Service -Name wuauserv -Force; (Get-Service -Name wuauserv).Status");
            FixResult { success: ok, output: out }
        }
        "print_spooler" => {
            let (ok, out) = run_ps("Restart-Service -Name Spooler -Force; (Get-Service -Name Spooler).Status");
            FixResult { success: ok, output: out }
        }
        "winsock_reset" => {
            let (ok, out) = run_ps("netsh winsock reset");
            FixResult { success: ok, output: out }
        }
        "restore_point" => {
            let script = r#"
                try {
                    Enable-ComputerRestore -Drive "$env:SystemDrive\" -ErrorAction SilentlyContinue
                    Checkpoint-Computer -Description "PC Doctor" -RestorePointType "MODIFY_SETTINGS" -ErrorAction Stop
                    "Restore point created."
                } catch {
                    "Could not create a restore point: $($_.Exception.Message) (Windows allows only one per 24 hours by default, this may already be fine.)"
                }
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
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
                    "Cleared $freedMB MB from the download cache and reclaimed superseded updates from the component store."
                } else {
                    "Cleared $freedMB MB from the download cache. Component store cleanup didn't finish - it can be retried after a restart."
                }
            "#;
            let (ok, out) = run_ps_with_timeout(script, REPAIR_TIMEOUT);
            FixResult { success: ok, output: out }
        }
        "defender_scan" => {
            let script = r#"
                try {
                    Start-MpScan -ScanType QuickScan -ErrorAction Stop
                    "Quick scan complete. Check Windows Security for results."
                } catch {
                    "Could not run a scan: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
        }
        "startup_programs" => {
            let (ok, out) = run_ps("Start-Process taskmgr; \"Task Manager opened. Go to the Startup tab to review and disable items.\"");
            FixResult { success: ok, output: out }
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
                    "Steam installation not found."
                } else {
                    $folders = "appcache\httpcache", "appcache\stats", "depotcache"
                    foreach ($f in $folders) {
                        $p = Join-Path $steamPath $f
                        if (Test-Path $p) {
                            Remove-Item -Path "$p\*" -Recurse -Force -ErrorAction SilentlyContinue
                        }
                    }
                    "Steam cache cleared. Restart Steam to rebuild it."
                }
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
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
                        "Steam installation not found."
                    } else {
                        $blob = Join-Path $steamPath "ClientRegistry.blob"
                        if (Test-Path $blob) { Remove-Item $blob -Force }
                        "Steam's local registry was reset. Open Steam and log back in."
                    }
                } catch {
                    "Could not reset Steam: $($_.Exception.Message)"
                }
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
        }
        "epic_cache" => {
            let script = r#"
                Stop-Process -Name "EpicGamesLauncher", "EpicWebHelper" -Force -ErrorAction SilentlyContinue
                Start-Sleep -Seconds 1
                Remove-Item -Path "$env:LOCALAPPDATA\EpicGamesLauncher\Saved\webcache" -Recurse -Force -ErrorAction SilentlyContinue
                Remove-Item -Path "$env:LOCALAPPDATA\EpicGamesLauncher\Saved\webcache_4147" -Recurse -Force -ErrorAction SilentlyContinue
                "Epic Games Launcher cache cleared. Restart the launcher to rebuild it."
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
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
                    "Closed stuck Epic Games processes and reopened the launcher."
                } else {
                    "Closed stuck Epic Games processes. Couldn't find the launcher to reopen it automatically — open it manually."
                }
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
        }
        "system_file_check" => {
            // Repairs the component store with DISM (which SFC repairs *from*),
            // then runs SFC. DISM's repair source is Windows Update, so we make
            // sure it's reachable: start the WU service and, if a WSUS policy is
            // redirecting updates (a common cause of error 0x800f0915), bypass it
            // for the duration of the repair and restore it afterward. Returns a
            // short verdict prefixed with a machine-readable STATUS line.
            let script = r#"
                $ProgressPreference = 'SilentlyContinue'
                $lines = New-Object System.Collections.ArrayList

                # DISM's repair source is Windows Update - make sure the service is up.
                try { Start-Service wuauserv -ErrorAction SilentlyContinue } catch {}

                # Temporarily bypass WSUS redirection so DISM can reach Windows Update.
                $auPath = "HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU"
                $useWU = $null
                try { $useWU = (Get-ItemProperty -Path $auPath -Name UseWUServer -ErrorAction Stop).UseWUServer } catch {}
                if ($useWU -eq 1) {
                    Set-ItemProperty -Path $auPath -Name UseWUServer -Value 0 -ErrorAction SilentlyContinue
                    Restart-Service wuauserv -Force -ErrorAction SilentlyContinue
                }

                # Repair the component store. Output is captured only to inspect it,
                # never shown, so the progress bars don't flood the result.
                $dismRaw = & dism.exe /Online /Cleanup-Image /RestoreHealth 2>&1 | Out-String
                $dismCode = $LASTEXITCODE
                $dismOk = ($dismCode -eq 0)

                # Restore the WSUS setting we changed.
                if ($useWU -eq 1) {
                    Set-ItemProperty -Path $auPath -Name UseWUServer -Value 1 -ErrorAction SilentlyContinue
                    Restart-Service wuauserv -Force -ErrorAction SilentlyContinue
                }

                if ($dismOk) {
                    [void]$lines.Add("DISM: Windows component store is healthy.")
                } elseif ($dismRaw -match "0x800f0915" -or $dismRaw -match "could not be found") {
                    [void]$lines.Add("DISM: couldn't download repair files (error 0x800f0915).")
                    [void]$lines.Add("Connect to the internet and turn off any VPN or work/metered network, then run this again. If it keeps failing, the repair may need a Windows installation ISO as a source.")
                } else {
                    [void]$lines.Add("DISM: repair didn't complete. Check your internet connection and try again.")
                }

                function Get-SfcResult {
                    $raw = & sfc.exe /scannow | Out-String
                    # SFC prints with embedded spacer characters; strip non-ASCII.
                    $clean = ($raw -replace "[^\x20-\x7E\r\n]", "")
                    if ($clean -match "did not find any integrity violations") { return "clean" }
                    if ($clean -match "successfully repaired")                 { return "repaired" }
                    if ($clean -match "unable to fix")                         { return "partial" }
                    if ($clean -match "could not perform")                     { return "error" }
                    return "unknown"
                }

                # SFC repairs from the store DISM just fixed. If the store was healthy
                # but a first pass couldn't fix everything, a second pass often clears it.
                $res = Get-SfcResult
                if ($res -eq "partial" -and $dismOk) { $res = Get-SfcResult }

                switch ($res) {
                    "clean"    { [void]$lines.Add("SFC: no integrity violations - your system files are intact.") }
                    "repaired" { [void]$lines.Add("SFC: found and repaired corrupted system files. Restart your PC to finish.") }
                    "partial"  { [void]$lines.Add("SFC: fixed what it could, but some files remain. Restart and run this again; if it persists, run it once in Safe Mode.") }
                    "error"    { [void]$lines.Add("SFC: couldn't run (another repair may be pending). Restart your PC and try again.") }
                    default    { [void]$lines.Add("SFC: finished. Details are in C:\Windows\Logs\CBS\CBS.log.") }
                }

                if ($dismOk -and ($res -eq "clean" -or $res -eq "repaired")) { $status = "ok" }
                elseif (-not $dismOk -and ($res -eq "partial" -or $res -eq "error")) { $status = "fail" }
                else { $status = "warn" }

                "STATUS:$status`n" + ($lines -join "`n")
            "#;
            let (ok, out) = run_ps_with_timeout(script, REPAIR_TIMEOUT);
            if !ok {
                return FixResult { success: false, output: out };
            }
            let (status_line, body) = out.split_once('\n').unwrap_or(("", out.as_str()));
            let success = status_line.trim() == "STATUS:ok";
            FixResult { success, output: body.trim().to_string() }
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
                "Cleared $mb MB of shader cache. It rebuilds automatically as you use apps and games."
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
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
                Start-Process $url
                if ($v -ne "") {
                    "Opened the $v driver download page in your browser. Download and run the latest driver for your card ($gpu), then restart your PC."
                } else {
                    "Opened Windows Update. Click 'Check for updates' - it can install the right graphics driver for you."
                }
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
        }
        "gpu_restart_driver" => {
            // Cycle the active display adapter. Always attempt to re-enable, even
            // on error, so the user is never left without a display.
            let script = r#"
                try {
                    $dev = Get-PnpDevice -Class Display -Status OK -ErrorAction Stop | Select-Object -First 1
                    if (-not $dev) {
                        "Couldn't find an active display adapter to restart."
                    } else {
                        Disable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction Stop
                        Start-Sleep -Seconds 2
                        Enable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction Stop
                        "Graphics driver restarted ($($dev.FriendlyName)). If the screen looked frozen, it should respond now."
                    }
                } catch {
                    # Make sure the adapter is back on no matter what went wrong.
                    Get-PnpDevice -Class Display -ErrorAction SilentlyContinue |
                        ForEach-Object { Enable-PnpDevice -InstanceId $_.InstanceId -Confirm:$false -ErrorAction SilentlyContinue }
                    "Couldn't fully restart the graphics driver: $($_.Exception.Message). If your screen is acting up, restart your PC."
                }
            "#;
            let (ok, out) = run_ps_with_timeout(script, LONG_TIMEOUT);
            FixResult { success: ok, output: out }
        }
        "disk_health" => {
            // No software fix for a failing drive - guide the user to back up.
            let script = r#"
                Start-Process "ms-settings:backup"
                "Opened Windows Backup settings. Back up your important files now. A SMART warning often appears before a drive fails - if it keeps reporting problems, replace the drive."
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
        }
        "device_problems" => {
            // Rescan for hardware (can install matching drivers from the driver
            // store) and open Device Manager so the user can act on the rest.
            let script = r#"
                & pnputil.exe /scan-devices 2>&1 | Out-Null
                Start-Process "devmgmt.msc"
                "Rescanned for hardware and opened Device Manager. Look for items marked with a yellow '!', right-click one, and choose 'Update driver'. Missing drivers usually come from Windows Update or your PC/motherboard maker's support page."
            "#;
            let (ok, out) = run_ps_with_timeout(script, LONG_TIMEOUT);
            FixResult { success: ok, output: out }
        }
        "battery_health" => {
            // Generate Windows' detailed battery report and open it.
            let script = r#"
                $out = "$env:USERPROFILE\battery-report.html"
                & powercfg.exe /batteryreport /output "$out" 2>&1 | Out-Null
                if (Test-Path $out) {
                    Start-Process $out
                    "Generated a detailed battery report and opened it ($out). The 'Battery capacity history' section shows how its capacity has dropped over time."
                } else {
                    "Couldn't generate a battery report on this device."
                }
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
        }
        "memory_check" => {
            // Launch Windows Memory Diagnostic (it offers restart-now or
            // on-next-reboot, so the user stays in control of the reboot).
            let script = r#"
                Start-Process "mdsched.exe"
                "Opened Windows Memory Diagnostic. Choose 'Restart now and check for problems' to test your RAM - your PC will reboot and run the scan. Save your work first."
            "#;
            let (ok, out) = run_ps(script);
            FixResult { success: ok, output: out }
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
            get_history
        ])
        .on_window_event(|_window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                kill_all_active_powershell();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
