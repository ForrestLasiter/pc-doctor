//! Updating from GitHub releases: find the latest release, download its
//! installer, check it arrived intact.
//!
//! What is trusted: the release is fetched over HTTPS from this one repository
//! on api.github.com, only that release's own "PC.Doctor_x.y.z_x64-setup.exe"
//! under the repository's download address is accepted, and the downloaded
//! file must match the size (and the SHA-256 digest, when GitHub lists one)
//! given for it. The installer is not code-signed, so this is as far as the
//! check can go.

use crate::{run_ps, run_ps_with_timeout, LONG_TIMEOUT};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

pub const REPO: &str = "ForrestLasiter/pc-doctor";
const MIN_INSTALLER: u64 = 500 * 1024;
const MAX_INSTALLER: u64 = 200 * 1024 * 1024;
const MAX_NOTES_CHARS: usize = 6000;

#[derive(Clone, Debug, PartialEq)]
pub struct Installer {
    pub version: String,
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub notes: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct UpdateInfo {
    pub available: bool,
    pub version: String,
    pub current: String,
    pub size: u64,
    pub notes: String,
    pub can_install: bool,
}

/// "v1.2.3" or "1.2.3" -> [1, 2, 3], or None.
pub fn parse_version(text: &str) -> Option<[u32; 3]> {
    let t = text.trim();
    let t = t.strip_prefix('v').unwrap_or(t);
    let mut parts = t.split('.');
    let mut out = [0u32; 3];
    for slot in out.iter_mut() {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// True when version `a` is later than version `b`. Anything unreadable is never newer.
pub fn is_newer(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

/// Picks the installer out of a release as GitHub describes it, refusing
/// anything that is not exactly what a PC Doctor release looks like.
pub fn pick_installer(release_json: &str) -> Result<Installer, String> {
    let unreadable = || "The latest release could not be read.".to_string();
    let release: serde_json::Value =
        serde_json::from_str(release_json).map_err(|_| unreadable())?;
    let tag = release["tag_name"].as_str().map(str::trim).unwrap_or("");
    let version = parse_version(tag).ok_or_else(unreadable)?;
    if release["draft"].as_bool() != Some(false) || release["prerelease"].as_bool() != Some(false) {
        return Err(unreadable());
    }
    let version = format!("{}.{}.{}", version[0], version[1], version[2]);
    let wanted = format!("PC.Doctor_{}_x64-setup.exe", version);
    let asset = release["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"].as_str() == Some(&wanted)))
        .ok_or_else(|| format!("Version {} has no installer attached yet.", version))?;
    // `tag` passed parse_version, so it holds only digits, dots and an optional "v".
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        REPO, tag, wanted
    );
    if asset["browser_download_url"].as_str() != Some(&url) {
        return Err("The installer is not where it should be, so it was not downloaded.".into());
    }
    let size = asset["size"].as_u64().unwrap_or(0);
    if !(MIN_INSTALLER..=MAX_INSTALLER).contains(&size) {
        return Err("The installer is an unexpected size, so it was not downloaded.".into());
    }
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase);
    let notes: String = release["body"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(MAX_NOTES_CHARS)
        .collect();
    Ok(Installer {
        version,
        name: wanted,
        url,
        size,
        sha256,
        notes,
    })
}

fn fetch_latest_release_json() -> Result<String, String> {
    let script = format!(
        r#"
        $ProgressPreference = 'SilentlyContinue'
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        [Console]::OutputEncoding = [Text.Encoding]::UTF8
        try {{
            $r = Invoke-WebRequest -UseBasicParsing -Uri 'https://api.github.com/repos/{repo}/releases/latest' -Headers @{{ Accept = 'application/vnd.github+json' }} -UserAgent 'PC-Doctor' -TimeoutSec 20 -ErrorAction Stop
            'OK|' + $r.Content
        }} catch {{
            $code = 0
            if ($_.Exception.Response) {{ $code = [int]$_.Exception.Response.StatusCode }}
            "ERR|$code"
        }}
    "#,
        repo = REPO
    );
    let (ok, out) = run_ps(&script);
    let out = out.trim();
    if let Some(json) = out.strip_prefix("OK|") {
        return Ok(json.to_string());
    }
    if !ok {
        return Err("Could not check for updates. Try again in a little while.".into());
    }
    match out.strip_prefix("ERR|").map(str::trim) {
        Some("403") | Some("429") => Err(
            "GitHub is limiting requests from this network just now. Try again in a little while."
                .into(),
        ),
        Some("404") => Err("No release has been published yet.".into()),
        Some("0") | None => {
            Err("Could not reach GitHub. Check the internet connection and try again.".into())
        }
        Some(code) => Err(format!("GitHub answered with an error ({}).", code)),
    }
}

/// Asks GitHub for the latest release and compares it with `current`.
pub fn check_latest(current: &str) -> Result<(Installer, UpdateInfo), String> {
    let installer = pick_installer(&fetch_latest_release_json()?)?;
    let info = UpdateInfo {
        available: is_newer(&installer.version, current),
        version: installer.version.clone(),
        current: current.to_string(),
        size: installer.size,
        notes: installer.notes.clone(),
        can_install: !cfg!(debug_assertions),
    };
    Ok((installer, info))
}

/// Downloads the installer into the temp folder and checks it. A file that is
/// the wrong size or does not match its digest is deleted and refused.
pub fn download_installer(installer: &Installer) -> Result<PathBuf, String> {
    // Only values already validated by pick_installer (digits, fixed names)
    // are placed in the script, so nothing from the network is interpreted.
    if parse_version(&installer.version).is_none()
        || installer.name != format!("PC.Doctor_{}_x64-setup.exe", installer.version)
        || !installer
            .url
            .starts_with(&format!("https://github.com/{}/releases/download/", REPO))
        || installer.url.contains('\'')
    {
        return Err("The update details were not valid, so nothing was downloaded.".into());
    }
    let script = format!(
        r#"
        $ProgressPreference = 'SilentlyContinue'
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        try {{
            $dir = Join-Path $env:TEMP 'PCDoctor-update'
            New-Item -ItemType Directory -Path $dir -Force -ErrorAction Stop | Out-Null
            $part = Join-Path $dir '{name}.part'
            Remove-Item -LiteralPath $part -Force -ErrorAction SilentlyContinue
            Invoke-WebRequest -UseBasicParsing -Uri '{url}' -OutFile $part -UserAgent 'PC-Doctor' -ErrorAction Stop
            $len = (Get-Item -LiteralPath $part).Length
            $hash = (Get-FileHash -LiteralPath $part -Algorithm SHA256).Hash.ToLower()
            "OK|$len|$hash|$part"
        }} catch {{
            "ERR|$($_.Exception.Message)"
        }}
    "#,
        name = installer.name,
        url = installer.url
    );
    let (ok, out) = run_ps_with_timeout(&script, LONG_TIMEOUT);
    let line = out.trim();
    let Some(rest) = line.strip_prefix("OK|") else {
        return Err(if ok {
            "The download failed. Check the internet connection and try again.".into()
        } else {
            "The download did not finish. Try again.".to_string()
        });
    };
    let mut fields = rest.splitn(3, '|');
    let len: u64 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let hash = fields.next().unwrap_or("").to_string();
    let part = PathBuf::from(fields.next().unwrap_or(""));
    let digest_ok = installer.sha256.as_ref().is_none_or(|want| *want == hash);
    if len != installer.size || !digest_ok {
        let _ = fs::remove_file(&part);
        return Err(
            "The downloaded installer did not match what GitHub lists, so it was thrown away. Try again."
                .into(),
        );
    }
    let target = part.with_file_name(&installer.name);
    let _ = fs::remove_file(&target);
    fs::rename(&part, &target).map_err(|e| format!("Could not save the installer: {}", e))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, name: &str, url: &str, size: u64, digest: &str) -> String {
        serde_json::json!({
            "tag_name": tag, "draft": false, "prerelease": false, "body": "Notes",
            "assets": [{ "name": name, "browser_download_url": url, "size": size, "digest": digest }]
        })
        .to_string()
    }

    const GOOD_URL: &str =
        "https://github.com/ForrestLasiter/pc-doctor/releases/download/v0.9.0/PC.Doctor_0.9.0_x64-setup.exe";
    const HASH: &str = "sha256:507e73182d8f47da599820647ca0e73fbb452d88467a665a2e73193da8982a6e";

    #[test]
    fn parses_versions_strictly() {
        assert_eq!(parse_version("v1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse_version(" 0.8.0 "), Some([0, 8, 0]));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.2.x"), None);
        assert_eq!(parse_version("1.2.3-beta"), None);
        assert_eq!(parse_version("v1.2.3'; calc"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.7.0", "0.8.0"));
        assert!(!is_newer("garbage", "0.8.0"));
        assert!(!is_newer("0.9.0", "garbage"));
    }

    #[test]
    fn picks_a_well_formed_installer() {
        let i = pick_installer(&release(
            "v0.9.0",
            "PC.Doctor_0.9.0_x64-setup.exe",
            GOOD_URL,
            1_400_000,
            HASH,
        ))
        .unwrap();
        assert_eq!(i.version, "0.9.0");
        assert_eq!(i.url, GOOD_URL);
        assert_eq!(i.size, 1_400_000);
        assert_eq!(i.sha256.as_deref(), Some(&HASH[7..]));
        assert_eq!(i.notes, "Notes");
    }

    #[test]
    fn refuses_an_installer_hosted_elsewhere() {
        let r = release(
            "v0.9.0",
            "PC.Doctor_0.9.0_x64-setup.exe",
            "https://evil.example/PC.Doctor_0.9.0_x64-setup.exe",
            1_400_000,
            HASH,
        );
        assert!(pick_installer(&r).is_err());
    }

    #[test]
    fn refuses_wrong_name_size_or_state() {
        let wrong_name = release("v0.9.0", "setup.exe", GOOD_URL, 1_400_000, HASH);
        assert!(pick_installer(&wrong_name).is_err());
        let tiny = release(
            "v0.9.0",
            "PC.Doctor_0.9.0_x64-setup.exe",
            GOOD_URL,
            10,
            HASH,
        );
        assert!(pick_installer(&tiny).is_err());
        let bad_tag = release(
            "latest",
            "PC.Doctor_0.9.0_x64-setup.exe",
            GOOD_URL,
            1_400_000,
            HASH,
        );
        assert!(pick_installer(&bad_tag).is_err());
        let draft = r#"{"tag_name":"v0.9.0","draft":true,"prerelease":false,"assets":[]}"#;
        assert!(pick_installer(draft).is_err());
        assert!(pick_installer("not json").is_err());
    }

    #[test]
    fn a_malformed_digest_is_ignored_not_trusted() {
        let i = pick_installer(&release(
            "v0.9.0",
            "PC.Doctor_0.9.0_x64-setup.exe",
            GOOD_URL,
            1_400_000,
            "md5:abc",
        ))
        .unwrap();
        assert_eq!(i.sha256, None);
    }

    #[test]
    fn download_refuses_tampered_details() {
        let i = Installer {
            version: "0.9.0".into(),
            name: "PC.Doctor_0.9.0_x64-setup.exe".into(),
            url: "https://evil.example/x.exe".into(),
            size: 1_400_000,
            sha256: None,
            notes: String::new(),
        };
        assert!(download_installer(&i).is_err());
    }

    // Live checks against GitHub: `cargo test --lib -- --ignored`.
    #[test]
    #[ignore]
    fn live_check_finds_the_published_release() {
        let (installer, info) = check_latest("0.0.1").expect("check");
        assert!(info.available);
        assert!(installer.sha256.is_some());
        let (_, same) = check_latest(&installer.version).expect("check");
        assert!(!same.available);
    }

    #[test]
    #[ignore]
    fn live_download_verifies_and_rejects_a_wrong_digest() {
        let (installer, _) = check_latest("0.0.1").expect("check");
        let path = download_installer(&installer).expect("download");
        assert_eq!(fs::metadata(&path).unwrap().len(), installer.size);
        let _ = fs::remove_file(&path);

        let mut wrong = installer.clone();
        wrong.sha256 = Some("0".repeat(64));
        assert!(download_installer(&wrong).is_err());
        assert!(!path.exists());
    }
}
