# PC Doctor

A lightweight Windows desktop app that scans your PC for common performance and reliability issues, then lets you fix them with one click.

## What it does

PC Doctor runs a set of checks against your system and reports whether each one is healthy or needs attention:

- **System Restore Point** — creates a restore point so changes made here can be undone
- **Temporary Files** — measures and clears space used by temp files
- **Low Disk Space (C:)** — checks free space and runs disk cleanup if low
- **DNS Cache** — flushes the DNS resolver cache for connectivity issues
- **Windows Updates** — checks for and installs available updates
- **Windows Update Service** — verifies the update service is running, restarts it if not
- **Print Spooler** — verifies the print spooler is running, restarts it if stuck
- **Network Stack Reset** — resets Winsock to fix network connectivity issues
- **Windows Update Cache** — clears a bloated or corrupt update download cache
- **Antivirus Quick Scan** — checks Windows Defender status and runs a quick scan
- **Startup Programs** — flags apps slowing down boot and opens Task Manager to review them (nothing is disabled automatically)
- **Corrupted System Files** — flags Windows component-store corruption and repairs it with DISM + SFC
- **Steam Download Cache** — clears bloated Steam cache that causes stalled downloads or store glitches
- **Steam Won't Open or Log In** — resets Steam's local client registry, the standard fix for Steam failing to launch or freezing on login
- **Epic Games Launcher Cache** — clears the launcher's web cache, fixing a blank window or missing library
- **Epic Games Launcher Frozen** — force-closes stuck Epic processes and reopens the launcher

Checks that detect Steam or Epic Games aren't installed report healthy automatically rather than showing a false issue.

Each check can be scanned individually or fixed with "Fix All." Scans run several checks concurrently to finish faster. A run history is kept locally so you can see what's been done over time.

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

Early (v0.3.2). Built and tested on Windows only. See [Releases](../../releases) for the latest installer and changelog.
