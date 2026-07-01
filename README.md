# PC Doctor

A lightweight Windows desktop app that scans your PC for common performance and reliability issues, then lets you fix them with one click.

## What it does

PC Doctor runs a set of checks against your system and reports whether each one is healthy or needs attention:

- **System Restore Point** — creates a restore point so changes made here can be undone
- **Temporary Files** — measures and clears space used by temp files
- **Low Disk Space (C:)** — checks free space and runs disk cleanup if low
- **DNS Cache** — flushes the DNS resolver cache for connectivity issues
- **Windows Update Service** — verifies the update service is running, restarts it if not
- **Print Spooler** — verifies the print spooler is running, restarts it if stuck
- **Network Stack Reset** — resets Winsock to fix network connectivity issues
- **Windows Update Cache** — clears a bloated or corrupt update download cache
- **Antivirus Quick Scan** — checks Windows Defender status and runs a quick scan
- **Startup Programs** — flags apps slowing down boot and opens Task Manager to review them (nothing is disabled automatically)
- **Corrupted System Files** — flags Windows component-store corruption and repairs it with DISM + SFC, running the repair in the background and reporting the result in History
- **Steam Download Cache** — clears bloated Steam cache that causes stalled downloads or store glitches
- **Steam Won't Open or Log In** — resets Steam's local client registry, the standard fix for Steam failing to launch or freezing on login
- **Epic Games Launcher Cache** — clears the launcher's web cache, fixing a blank window or missing library
- **Epic Games Launcher Frozen** — force-closes stuck Epic processes and reopens the launcher
- **Graphics Shader Cache** — clears DirectX and GPU-vendor shader caches, a common fix for stuttering, texture glitches, or flickering
- **Graphics Driver Health** — flags driver errors (e.g. Code 43), a generic/missing display driver, or a very old driver, and opens the right place to get the correct one
- **Restart Graphics Driver** — restarts the graphics driver to recover from a frozen/black screen or visual artifacts
- **Disk Health (SMART)** — reads each drive's health status and warns you if a drive may be failing, so you can back up in time
- **Hardware Device Problems** — flags connected components and built-in hardware that Windows can't use (usually a missing driver) and helps get them working
- **Battery Health** — on laptops, reports how much battery capacity has been lost to wear; skipped on desktops
- **Memory (RAM)** — reports installed memory and recent hardware errors, and can launch Windows Memory Diagnostic to test for faulty RAM

Checks that detect Steam or Epic Games aren't installed report healthy automatically rather than showing a false issue. Hardware checks that can't be fixed in software (a failing drive, worn battery) instead point you to the right next step.

Each check can be scanned individually or fixed with "Fix All." Scans run several checks concurrently to finish faster. A run history is kept locally so you can see what's been done over time.

Starting a scan also quietly nudges Windows Update to look for updates in the background — Windows downloads and installs them on its own schedule, so PC Doctor never makes you wait on it.

## Installing on Windows

Download the latest `PC Doctor_x.y.z_x64-setup.exe` from the [Releases page](../../releases) and run it.

**You'll probably see a blue "Windows protected your PC" warning.** That's Windows SmartScreen, and it shows up for any app that isn't signed with a paid certificate — not a sign that anything is wrong. PC Doctor is free and open-source, so you can read exactly what it does in [`src-tauri/src/lib.rs`](src-tauri/src/lib.rs) before installing. To continue:

1. Click **More info** on the warning.
2. Click **Run anyway**.
3. Follow the setup wizard (Next → Install → Finish).

You only have to do this once per download. PC Doctor needs administrator rights to repair Windows components, so you'll also get a "Do you want to allow this app to make changes?" prompt — click **Yes**.

### Verifying your download (optional)

Each release includes a `SHA256SUMS.txt` file. To confirm your installer wasn't tampered with, open PowerShell where you downloaded it and run:

```powershell
Get-FileHash ".\PC Doctor_0.7.0_x64-setup.exe" -Algorithm SHA256
```

The hash it prints should match the one in `SHA256SUMS.txt` for that file.

## Why

Most "PC cleaner" tools are bloated, ad-laden, or push you toward paid upsells for things Windows can already do for free via PowerShell. PC Doctor is meant to be a small, transparent alternative: every scan and fix is a short, readable PowerShell command (visible in [`src-tauri/src/lib.rs`](src-tauri/src/lib.rs)), there's no telemetry or bundled third-party software, and nothing is changed on your system without you clicking a button.

## Tech stack

Built with [Tauri 2](https://tauri.app/) — a Rust backend (system checks and fixes) with a vanilla HTML/CSS/JS frontend. This keeps the installer small and the app fast to start, unlike Electron-based alternatives.

## Development

```bash
npm install
npm run tauri dev    # run in development
npm run tauri build  # produce a Windows installer
```

## Status

Early (v0.7.0). Built and tested on Windows only. See [Releases](../../releases) for the latest installer and changelog.
