//! Installing and removing downloads, per content type.
//!
//! | Type        | Goes to                                                            | Extra step                     |
//! |-------------|--------------------------------------------------------------------|--------------------------------|
//! | Camouflage  | `<game>\UserSkins\<vehicle folder>\`                               | –                              |
//! | Sound mod   | `<game>\sound\mod\`                                                | `sound{ enable_mod:b=yes }`    |
//! | Sight       | `Documents\My Games\WarThunder\Saves\<uid>\production\UserSights\` | –                              |
//! | Mission     | `<game>\UserMissions\` (or merged into the game root)              | –                              |
//! | Model       | merged into the game root (`content\`, `UserMissions\`, ...)       | –                              |
//! | Hangar      | merged into the game root                                          | top-level `hangarBlk:t="..."`  |
//! | Controls    | `Documents\My Games\WarThunder\WT Armory Manager presets\`         | import in game                 |
//!
//! Every written file, every overwritten file (backed up first) and every created folder is
//! recorded, so uninstalling restores the game folder exactly.

use crate::api::{Kind, Post};
use crate::{game, storage};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ConfigChange {
    SoundMod,
    Hangar { value: String, previous: Option<String> },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Installed {
    pub lang_group: u64,
    pub kind: Kind,
    pub title: String,
    pub author: String,
    pub thumb: Option<String>,
    pub file_name: String,
    pub installed_at: i64,
    /// When the mod was last uploaded or updated (unix seconds), for the compatibility check.
    #[serde(default)]
    pub updated_at: i64,
    pub files: Vec<PathBuf>,
    /// (game file that was overwritten, our copy of the original)
    pub backups: Vec<(PathBuf, PathBuf)>,
    pub created_dirs: Vec<PathBuf>,
    pub config: Option<ConfigChange>,
    /// What the player still has to do in game.
    pub note: String,
}

impl Installed {
    /// The most useful folder to show in Explorer.
    pub fn location(&self) -> Option<PathBuf> {
        if self.kind == Kind::Controls {
            return self.files.first().cloned();
        }
        let first = self.files.first()?;
        self.created_dirs
            .iter()
            .filter(|d| first.starts_with(d))
            .min_by_key(|d| d.components().count())
            .cloned()
            .or_else(|| first.parent().map(Path::to_path_buf))
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Manifest {
    pub items: Vec<Installed>,
}

impl Manifest {
    const FILE: &'static str = "installed.json";

    pub fn load() -> Manifest {
        storage::load(Self::FILE)
    }

    pub fn save(&self) -> Result<(), String> {
        storage::save(Self::FILE, self)
    }

    pub fn get(&self, lang_group: u64) -> Option<&Installed> {
        self.items.iter().find(|i| i.lang_group == lang_group)
    }
}

pub struct Target {
    pub root: PathBuf,
    pub user_id: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Archive layout

const JUNK: &[&str] = &["__macosx", ".ds_store", "thumbs.db", "desktop.ini"];
/// Folders that belong directly in the game root.
const ROOT_DIRS: &[&str] = &["content", "usermissions", "levels", "userskins", "sound", "ui", "config"];

#[derive(Clone, Copy)]
enum Src {
    Zip(usize),
    /// The downloaded file itself (a bare .blk).
    Whole,
}

struct Entry {
    src: Src,
    rel: Vec<String>,
}

fn is_zip(path: &Path) -> bool {
    use io::Read;
    let mut magic = [0u8; 4];
    fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"PK\x03\x04"
}

fn read_zip(path: &Path) -> Result<(zip::ZipArchive<fs::File>, Vec<Entry>), String> {
    let file = fs::File::open(path).map_err(|e| format!("Could not open the download: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("The archive is damaged: {e}"))?;
    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let f = archive.by_index(i).map_err(|e| format!("The archive is damaged: {e}"))?;
        if f.is_dir() {
            continue;
        }
        // enclosed_name rejects absolute paths and `..`, so nothing escapes the target folder.
        let Some(path) = f.enclosed_name() else { continue };
        let rel: Vec<String> = path
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        if rel.is_empty() || rel.iter().any(|c| JUNK.contains(&c.to_lowercase().as_str())) {
            continue;
        }
        entries.push(Entry { src: Src::Zip(i), rel });
    }
    if entries.is_empty() {
        return Err("The archive is empty".into());
    }
    Ok((archive, entries))
}

/// Strips known leading folders (e.g. `sound\mod\`) and single wrapper folders, as long as every
/// file keeps more than `min_len` path parts and the wrapper is not one of `keep`.
fn normalize(entries: &mut [Entry], prefixes: &[&[&str]], min_len: usize, keep: &[&str]) {
    loop {
        let prefix = prefixes.iter().find(|pre| {
            entries.iter().all(|e| {
                e.rel.len() > pre.len() && e.rel.iter().zip(pre.iter()).all(|(a, b)| a.eq_ignore_ascii_case(b))
            })
        });
        if let Some(pre) = prefix {
            entries.iter_mut().for_each(|e| drop(e.rel.drain(..pre.len())));
            continue;
        }
        let first = entries[0].rel[0].clone();
        let is_kept = keep.iter().any(|k| first.eq_ignore_ascii_case(k));
        if !is_kept && entries.iter().all(|e| e.rel.len() > min_len && e.rel[0].eq_ignore_ascii_case(&first)) {
            entries.iter_mut().for_each(|e| drop(e.rel.remove(0)));
            continue;
        }
        break;
    }
}

fn join(base: &Path, rel: &[String]) -> PathBuf {
    rel.iter().fold(base.to_path_buf(), |p, c| p.join(c))
}

fn file_stem(name: &str) -> String {
    storage::sanitize(&Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
}

fn has_ext(rel: &[String], ext: &str) -> bool {
    rel.last().is_some_and(|n| n.to_lowercase().ends_with(ext))
}

/// The `hangarBlk:t="..."` line authors put in the post text.
fn hangar_from_description(post: &Post) -> Option<String> {
    let text = post.text();
    let at = text.find("hangarBlk")?;
    let rest = &text[at..];
    let rest = rest[rest.find('=')? + 1..].trim_start();
    let quotes: &[char] = &['"', '\u{201c}', '\u{201d}', '\''];
    let rest = rest.trim_start_matches(quotes);
    let value: String = rest.chars().take_while(|c| !quotes.contains(c) && *c != '\n').collect();
    let value = value.trim().to_string();
    (!value.is_empty() && value.len() < 200).then_some(value)
}

// ---------------------------------------------------------------------------------------------
// Install

pub fn install(post: &Post, target: &Target, download: &Path, file_date: Option<i64>) -> Result<Installed, String> {
    let kind = post.kind().ok_or("This kind of post cannot be installed")?;
    let file_name = post.file.as_ref().map(|f| f.name.clone()).unwrap_or_default();
    let root = &target.root;

    if matches!(kind, Kind::Sound | Kind::Location) && game::is_game_running() {
        return Err("Close War Thunder first: the game rewrites config.blk when it exits.".into());
    }

    let mut inst = Installed {
        lang_group: post.lang_group,
        kind,
        title: post.title(),
        author: post.author_name().to_string(),
        thumb: post.images.first().and_then(|i| i.thumb()).map(String::from),
        file_name: file_name.clone(),
        installed_at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
        updated_at: file_date.unwrap_or(0).max(post.created),
        files: Vec::new(),
        backups: Vec::new(),
        created_dirs: Vec::new(),
        config: None,
        note: String::new(),
    };

    let (mut archive, mut entries) = if is_zip(download) {
        let (a, e) = read_zip(download)?;
        (Some(a), e)
    } else {
        let lower = file_name.to_lowercase();
        if lower.ends_with(".rar") || lower.ends_with(".7z") {
            return Err(format!(
                "\"{file_name}\" is a {} archive. Only .zip archives and single files can be installed automatically.",
                if lower.ends_with(".rar") { "RAR" } else { "7-Zip" }
            ));
        }
        (None, vec![Entry { src: Src::Whole, rel: vec![storage::sanitize(&file_name)] }])
    };

    let stem = file_stem(&file_name);
    let mut plan: Vec<(Src, PathBuf)> = Vec::new();

    match kind {
        Kind::Camouflage => {
            // Wanted layout: UserSkins\<vehicle folder>\<textures + .blk>
            normalize(&mut entries, &[&["userskins"]], 2, &[]);
            let base = root.join("UserSkins");
            let flat_blks: Vec<&Entry> = entries.iter().filter(|e| e.rel.len() == 1 && has_ext(&e.rel, ".blk")).collect();
            let flat_folder = match flat_blks.as_slice() {
                [only] => file_stem(&only.rel[0]),
                _ => stem.clone(),
            };
            for e in &entries {
                let dest = if e.rel.len() == 1 { base.join(&flat_folder).join(&e.rel[0]) } else { join(&base, &e.rel) };
                plan.push((e.src, dest));
            }
            inst.note = "In game open Customization, press \"Update custom camouflage list\" and pick the skin from \
                         the custom camouflages. If it is missing, use \"Create camouflage example\" once for that \
                         vehicle, then reinstall."
                .into();
        }
        Kind::Sound => {
            normalize(&mut entries, &[&["sound", "mod"], &["mod"]], 1, &[]);
            let base = root.join("sound").join("mod");
            plan.extend(entries.iter().map(|e| (e.src, join(&base, &e.rel))));
            inst.config = Some(ConfigChange::SoundMod);
            inst.note = "Sound mods are switched on in config.blk (enable_mod:b=yes). Restart War Thunder for the \
                         sounds to load."
                .into();
        }
        Kind::Sight => {
            let uid = target.user_id.as_deref().ok_or(
                "Could not find your War Thunder Saves folder (Documents\\My Games\\WarThunder\\Saves). Start the \
                 game and log in once, or pick your user ID in Settings.",
            )?;
            let base = game::sights_dir(uid).ok_or("Could not find your Documents folder")?;
            // Wanted layout: UserSights\<vehicle id or all_tanks>\<sight>.blk
            normalize(&mut entries, &[&["production", "usersights"], &["usersights"]], 2, &[]);
            for e in &entries {
                let dest = if e.rel.len() == 1 { base.join("all_tanks").join(&e.rel[0]) } else { join(&base, &e.rel) };
                plan.push((e.src, dest));
            }
            inst.note = "Restart the game, then open Options > Main Parameters > Ground Vehicle Battle Settings > \
                         Sight Settings and choose the reticle."
                .into();
        }
        Kind::Mission | Kind::Model | Kind::Location => {
            normalize(&mut entries, &[], 1, ROOT_DIRS);
            let merges_into_root = entries.iter().any(|e| {
                e.rel.len() > 1 && ROOT_DIRS.contains(&e.rel[0].to_lowercase().as_str())
            });
            if kind == Kind::Mission && !merges_into_root {
                // Mission files only show up when they sit directly in UserMissions.
                let base = root.join("UserMissions");
                plan.extend(entries.iter().map(|e| (e.src, base.join(e.rel.last().unwrap()))));
            } else {
                plan.extend(entries.iter().map(|e| (e.src, join(root, &e.rel))));
            }
            inst.note = match kind {
                Kind::Mission => "Find it in the hangar under Custom Battles > User Missions.".into(),
                Kind::Model => "Start the mission that came with the model from Custom Battles > User Missions.".into(),
                _ => String::new(),
            };
        }
        Kind::Controls => {
            let base = storage::controls_dir();
            for e in &entries {
                let dest = if entries.len() == 1 {
                    base.join(e.rel.last().unwrap())
                } else {
                    base.join(&stem).join(e.rel.last().unwrap())
                };
                plan.push((e.src, dest));
            }
        }
    }

    let backup_dir = storage::backups_dir().join(post.lang_group.to_string());
    if let Err(e) = place(&mut archive, download, &plan, &backup_dir, &mut inst) {
        let _ = undo_files(&inst);
        return Err(e);
    }

    // Config changes come last so a failed copy never leaves config.blk edited.
    let config_result = match kind {
        Kind::Sound => game::set_sound_mod(root, true),
        Kind::Location => {
            let root_blks: Vec<String> = plan
                .iter()
                .filter(|(_, d)| d.parent() == Some(root.as_path()) && d.extension().is_some_and(|x| x.eq_ignore_ascii_case("blk")))
                .filter_map(|(_, d)| d.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            let hangar = hangar_from_description(post).or_else(|| match root_blks.as_slice() {
                [only] => Some(only.clone()),
                _ => None,
            });
            match hangar {
                Some(value) => game::get_hangar(root).and_then(|previous| {
                    game::set_hangar(root, Some(&value))?;
                    inst.note = format!(
                        "Hangar set in config.blk (hangarBlk:t=\"{value}\"). Restart War Thunder to see it. Custom \
                         hangars do not load in the launcher's compatibility mode."
                    );
                    inst.config = Some(ConfigChange::Hangar { value, previous });
                    Ok(())
                }),
                None => {
                    inst.note = "Files were added to the game folder. Follow the author's notes if anything else is needed.".into();
                    Ok(())
                }
            }
        }
        Kind::Controls => {
            let file = inst.files.first().map(|f| f.display().to_string()).unwrap_or_default();
            inst.note = format!(
                "War Thunder can only load control presets from inside the game: open Controls, click \"Import from \
                 file\" (bottom left) and choose {file}. Export your own controls first if you want to keep them."
            );
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(e) = config_result {
        let _ = undo_files(&inst);
        return Err(e);
    }
    Ok(inst)
}

fn place(
    archive: &mut Option<zip::ZipArchive<fs::File>>,
    whole: &Path,
    plan: &[(Src, PathBuf)],
    backup_dir: &Path,
    inst: &mut Installed,
) -> Result<(), String> {
    let mut written: HashSet<PathBuf> = HashSet::new();
    for (n, (src, dest)) in plan.iter().enumerate() {
        if let Some(parent) = dest.parent() {
            create_dirs(parent, &mut inst.created_dirs).map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
        }
        if dest.is_file() && !written.contains(dest) {
            fs::create_dir_all(backup_dir).map_err(|e| e.to_string())?;
            let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let copy = backup_dir.join(format!("{n}_{name}"));
            fs::copy(dest, &copy).map_err(|e| format!("Could not back up {}: {e}", dest.display()))?;
            inst.backups.push((dest.clone(), copy));
        }
        let result: io::Result<()> = match src {
            Src::Whole => fs::copy(whole, dest).map(|_| ()),
            Src::Zip(i) => {
                let archive = archive.as_mut().expect("zip entries come from an archive");
                match archive.by_index(*i) {
                    Ok(mut entry) => fs::File::create(dest).and_then(|mut out| io::copy(&mut entry, &mut out)).map(|_| ()),
                    Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
                }
            }
        };
        result.map_err(|e| format!("Could not write {}: {e}", dest.display()))?;
        if written.insert(dest.clone()) {
            inst.files.push(dest.clone());
        }
    }
    Ok(())
}

/// Creates `dir` and records every folder that did not exist before.
fn create_dirs(dir: &Path, created: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.exists() {
            break;
        }
        missing.push(d.to_path_buf());
        cur = d.parent();
    }
    fs::create_dir_all(dir)?;
    for d in missing.into_iter().rev() {
        if !created.contains(&d) {
            created.push(d);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Uninstall

/// Removes our files, puts back the originals and deletes folders we created (if empty).
fn undo_files(inst: &Installed) -> Result<(), String> {
    let mut errors = Vec::new();
    for f in &inst.files {
        if let Err(e) = fs::remove_file(f) {
            if e.kind() != io::ErrorKind::NotFound {
                errors.push(format!("{}: {e}", f.display()));
            }
        }
    }
    for (dest, copy) in &inst.backups {
        if !copy.exists() {
            continue; // already restored by an earlier, partly failed uninstall
        }
        match fs::copy(copy, dest) {
            Ok(_) => {
                let _ = fs::remove_file(copy);
            }
            Err(e) => errors.push(format!("could not restore {}: {e}", dest.display())),
        }
    }
    let mut dirs = inst.created_dirs.clone();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        let _ = fs::remove_dir(d); // only succeeds when empty
    }
    let _ = fs::remove_dir_all(storage::backups_dir().join(inst.lang_group.to_string()));
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("Some files could not be removed (is the game running?): {}", errors.join("; ")))
    }
}

/// `others` are the remaining installed items (used to keep sound mods on while one is left).
pub fn uninstall(inst: &Installed, root: &Path, others: &[Installed]) -> Result<(), String> {
    if inst.config.is_some() && game::is_game_running() {
        return Err("Close War Thunder first: the game rewrites config.blk when it exits.".into());
    }
    undo_files(inst)?;
    match &inst.config {
        Some(ConfigChange::SoundMod) => {
            let sound_left = others.iter().any(|o| o.kind == Kind::Sound && o.lang_group != inst.lang_group);
            if !sound_left {
                game::set_sound_mod(root, false)?;
            }
        }
        Some(ConfigChange::Hangar { value, previous }) => {
            // Only revert when the player has not picked another hangar since.
            if game::get_hangar(root)?.as_deref() == Some(value.as_str()) {
                game::set_hangar(root, previous.as_deref())?;
            }
        }
        None => {}
    }
    Ok(())
}

pub fn describe_target(kind: Kind) -> &'static str {
    match kind {
        Kind::Camouflage => "Installs into UserSkins in your game folder.",
        Kind::Sound => "Installs into sound\\mod and switches on sound mods in config.blk.",
        Kind::Sight => "Installs into UserSights in your Documents\\My Games\\WarThunder\\Saves folder.",
        Kind::Mission => "Installs into UserMissions in your game folder.",
        Kind::Model => "Merges into your game folder (content, UserMissions).",
        Kind::Location => "Merges into your game folder and sets the hangar in config.blk.",
        Kind::Controls => "Saved to Documents\\My Games\\WarThunder\\WT Armory Manager presets; import it in game under Controls.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(paths: &[&str]) -> Vec<Entry> {
        paths
            .iter()
            .map(|p| Entry { src: Src::Whole, rel: p.split('/').map(String::from).collect() })
            .collect()
    }

    fn rels(e: &[Entry]) -> Vec<String> {
        e.iter().map(|e| e.rel.join("/")).collect()
    }

    #[test]
    fn skin_wrapper_is_stripped_down_to_vehicle_folder() {
        let mut e = entries(&["My Skin v2/UserSkins/t_34_85_d_5t_cool/a.dds", "My Skin v2/UserSkins/t_34_85_d_5t_cool/t.blk"]);
        normalize(&mut e, &[&["userskins"]], 2, &[]);
        assert_eq!(rels(&e), ["t_34_85_d_5t_cool/a.dds", "t_34_85_d_5t_cool/t.blk"]);
    }

    #[test]
    fn sound_mod_prefix_is_stripped() {
        let mut e = entries(&["pack/sound/mod/tanks.bank", "pack/sound/mod/guns.bank"]);
        normalize(&mut e, &[&["sound", "mod"], &["mod"]], 1, &[]);
        assert_eq!(rels(&e), ["tanks.bank", "guns.bank"]);
    }

    #[test]
    fn root_folders_are_kept() {
        let mut e = entries(&["Model pack/content/pkg_user/x.dxp.bin", "Model pack/UserMissions/m.blk"]);
        normalize(&mut e, &[], 1, ROOT_DIRS);
        assert_eq!(rels(&e), ["content/pkg_user/x.dxp.bin", "UserMissions/m.blk"]);
    }

    #[test]
    fn hangar_line_is_read_from_post() {
        let post = Post { description: "<p>Add this line:</p><p>hangarBlk:t=&quot;Saux.blk&quot;</p>".into(), ..Default::default() };
        assert_eq!(hangar_from_description(&post).as_deref(), Some("Saux.blk"));
    }
}
