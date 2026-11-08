//! The app's own files: settings, the installed-items manifest, download cache and backups.
//! Everything lives in %LOCALAPPDATA%\WT Armory Manager.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub fn app_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("WT Armory Manager")
}

pub fn cache_dir() -> PathBuf {
    app_dir().join("downloads")
}

pub fn backups_dir() -> PathBuf {
    app_dir().join("backups")
}

/// Control presets cannot be dropped into the game; they are kept here for "Import from file".
pub fn controls_dir() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(app_dir)
        .join("My Games")
        .join("WarThunder")
        .join("WT Armory Manager presets")
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    pub game_root: Option<PathBuf>,
    /// Gaijin user ID folder under Documents\My Games\WarThunder\Saves (used for sights).
    pub user_id: Option<String>,
}

pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    fs::read_to_string(app_dir().join(name))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Writes atomically (temp file + rename) so a crash never leaves a half-written manifest.
pub fn save<T: Serialize>(name: &str, value: &T) -> Result<(), String> {
    let dir = app_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{name}.tmp"));
    fs::write(&tmp, json).map_err(|e| e.to_string())?;
    fs::rename(&tmp, dir.join(name)).map_err(|e| e.to_string())
}

/// Makes a string safe as a single Windows file or folder name.
pub fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let s = s.trim().trim_end_matches('.').trim().to_string();
    if s.is_empty() {
        "file".into()
    } else {
        s
    }
}

pub fn open_in_explorer(path: &Path) {
    let mut cmd = std::process::Command::new("explorer");
    if path.is_file() {
        cmd.arg(format!("/select,{}", path.display()));
    } else {
        cmd.arg(path);
    }
    let _ = cmd.spawn();
}
