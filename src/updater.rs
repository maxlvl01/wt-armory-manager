//! Self-update from GitHub releases.
//!
//! Windows can't overwrite a running exe, but it can *rename* one. So an update downloads the new
//! exe next to the current one, renames the running file to `<name>.old`, moves the new file into
//! its place and asks for a restart. The leftover `.old` file is deleted on the next start.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "maxlvl01/wt-armory-manager";

#[derive(Deserialize, Clone, Debug)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    /// "sha256:<hex>" (GitHub adds this to every asset).
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    html_url: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Clone, Debug)]
pub struct Release {
    /// Display version, e.g. "0.2.0a".
    pub version: String,
    pub page: String,
    pub exe_url: String,
    pub size: u64,
    pub sha256: Option<String>,
}

/// Parses "v0.1.1a", "0.2.0b", "1.0.0" into a sortable key. Final releases sort above alpha/beta.
pub fn version_key(v: &str) -> Option<(Vec<u32>, u8, u32)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    let (nums, suffix) = v.split_at(split);
    let nums: Vec<u32> = nums.split('.').filter(|s| !s.is_empty()).map(|s| s.parse().ok()).collect::<Option<_>>()?;
    if nums.is_empty() {
        return None;
    }
    let suffix = suffix.trim_start_matches(['-', '.']).to_ascii_lowercase();
    let letters: String = suffix.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let extra: u32 = suffix[letters.len()..].trim_start_matches('.').parse().unwrap_or(0);
    let stage = match letters.as_str() {
        "" => 3,
        "rc" => 2,
        "b" | "beta" => 1,
        _ => 0, // "a", "alpha"
    };
    Some((nums, stage, extra))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_key(candidate), version_key(current)) {
        (Some(a), Some(b)) => {
            let pad = |mut v: Vec<u32>| {
                v.resize(4, 0);
                v
            };
            (pad(a.0), a.1, a.2) > (pad(b.0), b.1, b.2)
        }
        _ => false,
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(crate::api::USER_AGENT)
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(|e| e.to_string())
}

/// The newest release (pre-releases included, drafts skipped) if it is newer than `current`.
pub fn check(current: &str) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=15");
    let text = client()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| format!("Could not reach GitHub: {e}"))?;
    let list: Vec<GhRelease> = serde_json::from_str(&text).map_err(|e| format!("Unexpected answer from GitHub: {e}"))?;
    let newest = list
        .into_iter()
        .filter(|r| !r.draft && version_key(&r.tag_name).is_some())
        .max_by(|a, b| {
            if is_newer(&a.tag_name, &b.tag_name) {
                std::cmp::Ordering::Greater
            } else if is_newer(&b.tag_name, &a.tag_name) {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        });
    let Some(r) = newest else { return Ok(None) };
    if !is_newer(&r.tag_name, current) {
        return Ok(None);
    }
    let Some(asset) = r.assets.iter().find(|a| a.name.to_ascii_lowercase().ends_with(".exe")) else {
        return Ok(None); // a release without a Windows build yet
    };
    let version = r.tag_name.trim_start_matches(['v', 'V']).to_string();
    Ok(Some(Release {
        version,
        page: r.html_url.clone(),
        exe_url: asset.browser_download_url.clone(),
        size: asset.size,
        sha256: asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).map(|s| s.to_ascii_lowercase()),
    }))
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Deletes the previous version left behind by an update. Call once at start-up.
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(old_path(&exe));
        let _ = fs::remove_file(exe.with_extension("exe.download"));
    }
}

/// Downloads the release, checks its size and SHA-256 and swaps it in for the running exe.
/// Returns the path of the (new) exe to restart.
pub fn install(release: &Release, mut progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Could not find the running program: {e}"))?;
    let tmp = exe.with_extension("exe.download");
    let mut resp = reqwest::blocking::Client::builder()
        .user_agent(crate::api::USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?
        .get(&release.exe_url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Could not download the update: {e}"))?;
    let total = resp.content_length().or(Some(release.size).filter(|s| *s > 0));
    let mut data = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = resp.read(&mut buf).map_err(|e| format!("Update download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        progress(data.len() as u64, total);
    }
    if release.size > 0 && data.len() as u64 != release.size {
        return Err(format!("The update download is incomplete ({} of {} bytes).", data.len(), release.size));
    }
    if let Some(expected) = &release.sha256 {
        let got = hex(&Sha256::digest(&data));
        if &got != expected {
            return Err("The update download is damaged (SHA-256 does not match). Nothing was changed.".into());
        }
    }
    if data.len() < 2 || &data[..2] != b"MZ" {
        return Err("The update download is not a Windows program. Nothing was changed.".into());
    }
    fs::write(&tmp, &data).map_err(|e| no_write(&exe, e))?;
    let old = old_path(&exe);
    let _ = fs::remove_file(&old);
    if let Err(e) = fs::rename(&exe, &old) {
        let _ = fs::remove_file(&tmp);
        return Err(no_write(&exe, e));
    }
    if let Err(e) = fs::rename(&tmp, &exe) {
        let _ = fs::rename(&old, &exe); // put the current version back
        let _ = fs::remove_file(&tmp);
        return Err(no_write(&exe, e));
    }
    Ok(exe)
}

fn no_write(exe: &Path, e: std::io::Error) -> String {
    let dir = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    format!("Could not replace the program in {dir} ({e}). Download the update from the Releases page instead.")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Starts the given exe and lets the caller exit.
pub fn restart(exe: &Path) -> Result<(), String> {
    std::process::Command::new(exe).spawn().map(|_| ()).map_err(|e| format!("Could not restart: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert!(is_newer("v0.1.2a", "0.1.1a"));
        assert!(is_newer("v0.2.0a", "0.1.9b"));
        assert!(is_newer("v0.1.1b", "0.1.1a"));
        assert!(is_newer("v0.1.1", "0.1.1b"));
        assert!(is_newer("v0.1.10a", "0.1.9a"));
        assert!(!is_newer("v0.1.1a", "0.1.1a"));
        assert!(!is_newer("v0.1.0a", "0.1.1a"));
        assert!(!is_newer("nightly", "0.1.1a"));
    }
}
