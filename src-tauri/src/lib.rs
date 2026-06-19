use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x08000000;

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
            description: "Checks for available Windows updates and installs them (installs the PSWindowsUpdate module first if needed).",
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
    ]
}

fn run_ps(script: &str) -> (bool, String) {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let combined = if stderr.is_empty() {
                stdout
            } else {
                format!("{}\n{}", stdout, stderr)
            };
            (out.status.success(), combined)
        }
        Err(e) => (false, format!("Failed to launch powershell: {}", e)),
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

#[tauri::command]
fn log_event(text: String) -> bool {
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
fn get_history() -> String {
    match history_path() {
        Some(path) => fs::read_to_string(&path).unwrap_or_default(),
        None => String::new(),
    }
}

#[tauri::command]
fn scan_check(id: String) -> ScanResult {
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
            let (ok, out) = run_ps(script);
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
        _ => ScanResult {
            status: "error".into(),
            detail: "Unknown check.".into(),
        },
    }
}

#[tauri::command]
fn fix_check(id: String) -> FixResult {
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
            let (ok, out) = run_ps(script);
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
