//! Finding the War Thunder install and the Saves folder, and editing config.blk.

use crate::storage;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Serialises config.blk edits from parallel installs.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// A War Thunder root holds the actual game, not just the launcher: a leftover launcher-only folder
/// (common after moving a Steam library) has `launcher.exe` and its own `config.blk` that the game
/// never reads.
pub fn is_game_root(p: &Path) -> bool {
    p.join("win64").join("aces.exe").is_file() || p.join("content").join("pkg_main.ver").is_file()
}

/// Only the launcher, no game files.
pub fn is_launcher_only(p: &Path) -> bool {
    p.join("launcher.exe").is_file() && !is_game_root(p)
}

/// The saved folder if it is still valid, else the first standard location that holds the game.
pub fn find_game_root(saved: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = saved.filter(|p| is_game_root(p)) {
        return Some(p.to_path_buf());
    }
    candidate_roots().into_iter().find(|p| is_game_root(p))
}

/// Installed client version from `content\pkg_main.ver`, e.g. "2.59.0.100".
pub fn installed_version(root: &Path) -> Option<String> {
    let v = fs::read_to_string(root.join("content").join("pkg_main.ver")).ok()?;
    let v = v.trim().to_string();
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then_some(v)
}

/// Standard install locations: the Gaijin launcher default, Steam (all libraries) and common folders.
pub fn candidate_roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(local) = dirs::data_local_dir() {
        v.push(local.join("WarThunder"));
    }
    for lib in steam_libraries() {
        v.push(lib.join("steamapps").join("common").join("War Thunder"));
    }
    for drive in ['C', 'D', 'E', 'F', 'G'] {
        let d = format!("{drive}:\\");
        if !Path::new(&d).exists() {
            continue;
        }
        for sub in [
            r"Program Files (x86)\WarThunder",
            r"Program Files\WarThunder",
            r"Games\WarThunder",
            r"Games\War Thunder",
            r"WarThunder",
            r"SteamLibrary\steamapps\common\War Thunder",
        ] {
            v.push(PathBuf::from(&d).join(sub));
        }
    }
    let mut seen = Vec::new();
    v.retain(|p| {
        let key = p.to_string_lossy().to_lowercase();
        let new = !seen.contains(&key);
        seen.push(key);
        new
    });
    v
}

/// Steam install folders plus every library listed in libraryfolders.vdf.
fn steam_libraries() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for steam in [r"C:\Program Files (x86)\Steam", r"C:\Program Files\Steam"] {
        let steam = PathBuf::from(steam);
        if !steam.is_dir() {
            continue;
        }
        out.push(steam.clone());
        if let Ok(vdf) = fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf")) {
            for line in vdf.lines() {
                // 		"path"		"D:\\SteamLibrary"
                let parts: Vec<&str> = line.split('"').collect();
                if parts.len() >= 4 && parts[1] == "path" {
                    out.push(PathBuf::from(parts[3].replace("\\\\", "\\")));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Saves folder (sights live here since update 2.53)

pub fn saves_dir() -> Option<PathBuf> {
    dirs::document_dir().map(|d| d.join("My Games").join("WarThunder").join("Saves"))
}

/// Numeric Gaijin user ID folders under Saves.
pub fn user_ids() -> Vec<String> {
    let Some(dir) = saves_dir() else { return Vec::new() };
    let mut ids: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .collect();
    ids.sort();
    ids
}

/// The account that last logged in (Saves\last\lastUid), else the first one found.
pub fn preferred_user_id() -> Option<String> {
    let ids = user_ids();
    let last = saves_dir()
        .and_then(|d| fs::read_to_string(d.join("last").join("lastUid")).ok())
        .map(|s| s.chars().filter(char::is_ascii_digit).collect::<String>());
    match last {
        Some(uid) if ids.contains(&uid) => Some(uid),
        _ => ids.into_iter().next(),
    }
}

pub fn sights_dir(user_id: &str) -> Option<PathBuf> {
    saves_dir().map(|d| d.join(user_id).join("production").join("UserSights"))
}

#[cfg(windows)]
pub fn is_game_running() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq aces.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("aces.exe"))
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn is_game_running() -> bool {
    false
}

// ---------------------------------------------------------------------------------------------
// config.blk
//
// BLK is a simple text format: `name:type=value` lines and `name{ ... }` blocks. The edits below
// work line by line and track brace depth, so the rest of the file is kept byte for byte.

struct Config {
    path: PathBuf,
    lines: Vec<String>,
    newline: &'static str,
}

impl Config {
    fn open(root: &Path) -> Result<Config, String> {
        let path = root.join("config.blk");
        let text = fs::read_to_string(&path).map_err(|_| {
            "config.blk was not found. Start War Thunder once so it creates the file, then try again.".to_string()
        })?;
        Ok(Config {
            newline: if text.contains("\r\n") { "\r\n" } else { "\n" },
            lines: text.lines().map(String::from).collect(),
            path,
        })
    }

    fn save(self) -> Result<(), String> {
        backup(&self.path);
        let mut text = self.lines.join(self.newline);
        text.push_str(self.newline);
        fs::write(&self.path, text).map_err(|e| format!("Could not write config.blk: {e}"))
    }

    /// (opening line, closing line) of the top-level block `name{ ... }`.
    fn block(&self, name: &str) -> Option<(usize, usize)> {
        let mut depth = 0;
        let mut start = None;
        for (i, line) in self.lines.iter().enumerate() {
            if depth == 0 && start.is_none() {
                let t = line.trim();
                if t.strip_prefix(name).is_some_and(|rest| rest.trim_start().starts_with('{')) {
                    start = Some(i);
                }
            }
            depth += brace_delta(line);
            if let Some(s) = start {
                if depth <= 0 {
                    return Some((s, i));
                }
            }
        }
        None
    }

    /// Line index of a top-level `key...` entry (outside every block).
    fn top_level(&self, key: &str) -> Option<usize> {
        let mut depth = 0;
        for (i, line) in self.lines.iter().enumerate() {
            if depth == 0 && line.trim_start().starts_with(key) {
                return Some(i);
            }
            depth += brace_delta(line);
        }
        None
    }
}

fn brace_delta(line: &str) -> i32 {
    let mut in_quotes = false;
    let mut d = 0;
    let mut prev = ' ';
    for c in line.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            '/' if prev == '/' && !in_quotes => break,
            '{' if !in_quotes => d += 1,
            '}' if !in_quotes => d -= 1,
            _ => {}
        }
        prev = c;
    }
    d
}

/// Keeps the last 15 copies of config.blk before each edit.
fn backup(path: &Path) {
    let dir = storage::backups_dir().join("config");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let _ = fs::copy(path, dir.join(format!("config_{ts}.blk")));
    let mut old: Vec<PathBuf> = fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    old.sort();
    while old.len() > 15 {
        let _ = fs::remove_file(old.remove(0));
    }
}

/// Sets `enable_mod:b=yes|no` inside the `sound{}` block (creating the block if needed).
pub fn set_sound_mod(root: &Path, on: bool) -> Result<(), String> {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut cfg = Config::open(root)?;
    let entry = format!("  enable_mod:b={}", if on { "yes" } else { "no" });
    match cfg.block("sound") {
        Some((s, e)) if e > s => {
            match (s + 1..e).find(|&j| cfg.lines[j].trim_start().starts_with("enable_mod:")) {
                Some(j) => cfg.lines[j] = entry,
                None => cfg.lines.insert(s + 1, entry),
            }
        }
        Some((s, _)) => {
            // `sound{}` on one line
            cfg.lines[s] = "sound{".into();
            cfg.lines.insert(s + 1, entry);
            cfg.lines.insert(s + 2, "}".into());
        }
        None if on => {
            cfg.lines.extend([String::new(), "sound{".into(), entry, "}".into()]);
        }
        None => return Ok(()),
    }
    cfg.save()
}

pub fn get_hangar(root: &Path) -> Result<Option<String>, String> {
    let cfg = Config::open(root)?;
    Ok(cfg.top_level("hangarBlk:").map(|i| {
        let line = &cfg.lines[i];
        let value = line.split_once('=').map(|(_, v)| v).unwrap_or("");
        value.trim().trim_matches('"').to_string()
    }))
}

/// Sets (or with `None` removes) the top-level `hangarBlk:t="..."` line.
pub fn set_hangar(root: &Path, value: Option<&str>) -> Result<(), String> {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut cfg = Config::open(root)?;
    let idx = cfg.top_level("hangarBlk:");
    match (idx, value) {
        (Some(i), Some(v)) => cfg.lines[i] = format!("hangarBlk:t=\"{v}\""),
        (None, Some(v)) => cfg.lines.insert(0, format!("hangarBlk:t=\"{v}\"")),
        (Some(i), None) => {
            cfg.lines.remove(i);
        }
        (None, None) => return Ok(()),
    }
    cfg.save()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(content: &str) -> PathBuf {
        // Tests run in parallel: a counter keeps folders unique even within one clock tick.
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wtam_test_{}_{ts}_{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.blk"), content).unwrap();
        dir
    }

    #[test]
    fn sound_mod_toggle() {
        let root = temp_root("video{\r\n  vsync:b=no\r\n}\r\n\r\nsound{\r\n  fxVolume:r=1\r\n  nested{\r\n    a:i=1\r\n  }\r\n}\r\n");
        set_sound_mod(&root, true).unwrap();
        let t = fs::read_to_string(root.join("config.blk")).unwrap();
        assert!(t.contains("sound{\r\n  enable_mod:b=yes\r\n  fxVolume:r=1"));
        set_sound_mod(&root, false).unwrap();
        let t = fs::read_to_string(root.join("config.blk")).unwrap();
        assert!(t.contains("enable_mod:b=no") && !t.contains("enable_mod:b=yes"));
        assert_eq!(t.matches("enable_mod").count(), 1);
    }

    #[test]
    fn sound_block_created() {
        let root = temp_root("video{\n  vsync:b=no\n}\n");
        set_sound_mod(&root, true).unwrap();
        let t = fs::read_to_string(root.join("config.blk")).unwrap();
        assert!(t.ends_with("sound{\n  enable_mod:b=yes\n}\n"));
    }

    #[test]
    fn hangar_set_and_restore() {
        let root = temp_root("graphicsQuality:t=\"movie\"\nvideo{\n  hangarBlk:t=\"inner.blk\"\n}\n");
        assert_eq!(get_hangar(&root).unwrap(), None, "nested keys are not top level");
        set_hangar(&root, Some("my_hangar.blk")).unwrap();
        assert_eq!(get_hangar(&root).unwrap().as_deref(), Some("my_hangar.blk"));
        set_hangar(&root, None).unwrap();
        let t = fs::read_to_string(root.join("config.blk")).unwrap();
        assert_eq!(t, "graphicsQuality:t=\"movie\"\nvideo{\n  hangarBlk:t=\"inner.blk\"\n}\n");
    }
}
