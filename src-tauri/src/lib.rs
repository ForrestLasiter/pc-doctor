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
const REPAIR_TIMEOUT: Duration = Duration::from_secs(1800);

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
            let script = r#"
                $path = "$env:windir\SoftwareDistribution\Download"
                $bytes = (Get-ChildItem -Path $path -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                if (-not $bytes) { $bytes = 0 }
                [math]::Round($bytes / 1MB, 1)
            "#;
            let (ok, out) = run_ps(script);
            if !ok {
                return ScanResult { status: "error".into(), detail: out };
            }
            let mb: f64 = out.trim().parse().unwrap_or(0.0);
            if mb > 500.0 {
                ScanResult {
                    status: "issue".into(),
                    detail: format!("{:.1} MB of old Windows Update files found.", mb),
                }
            } else {
                ScanResult {
                    status: "ok".into(),
                    detail: format!("Only {:.1} MB in the Windows Update cache.", mb),
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
        "steam_reset" => ScanResult {
            status: "issue".into(),
            detail: "Available as a manual fix if Steam won't launch, freezes while logging in, or shows a blank window.".into(),
        },
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
                return ScanResult { status: "ok".into(), detail: "Epic Games Launcher isn't installed on this PC.".into() };
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
        "epic_launcher_reset" => ScanResult {
            status: "issue".into(),
            detail: "Available as a manual fix if the Epic Games Launcher is frozen, blank, or won't open.".into(),
        },
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
            let script = r#"
                Stop-Service -Name wuauserv -Force -ErrorAction SilentlyContinue
                Stop-Service -Name bits -Force -ErrorAction SilentlyContinue
                Remove-Item -Path "$env:windir\SoftwareDistribution\Download\*" -Recurse -Force -ErrorAction SilentlyContinue
                Start-Service -Name bits -ErrorAction SilentlyContinue
                Start-Service -Name wuauserv -ErrorAction SilentlyContinue
                "Windows Update cache cleared."
            "#;
            let (ok, out) = run_ps(script);
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
            let script = r#"
                $dismOut = dism /Online /Cleanup-Image /RestoreHealth 2>&1 | Out-String
                $sfcOut = sfc /scannow 2>&1 | Out-String
                "DISM RestoreHealth:`n$dismOut`n`nSFC /scannow:`n$sfcOut"
            "#;
            let (ok, out) = run_ps_with_timeout(script, REPAIR_TIMEOUT);
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
