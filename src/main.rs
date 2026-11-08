#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod game;
mod icons;
mod installer;
mod storage;
mod updates;

use api::{Kind, Post, Sort};
use updates::{Compat, MajorUpdate};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontFamily, FontId, Frame, Layout, Margin, Rect, RichText, Rounding,
    Sense, Stroke, TextStyle,
};
use icons::Icon;
use installer::{Installed, Manifest};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------------------------
// Theme

const BG: Color32 = Color32::from_rgb(0x11, 0x13, 0x17);
const PANEL: Color32 = Color32::from_rgb(0x18, 0x1b, 0x20);
const SIDEBAR: Color32 = Color32::from_rgb(0x14, 0x16, 0x1a);
const CARD: Color32 = Color32::from_rgb(0x1e, 0x22, 0x29);
const CARD_HOVER: Color32 = Color32::from_rgb(0x24, 0x29, 0x31);
const IMG_BG: Color32 = Color32::from_rgb(0x15, 0x18, 0x1d);
const BORDER: Color32 = Color32::from_rgb(0x2b, 0x31, 0x3a);
const BTN: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x38);
const TEXT: Color32 = Color32::from_rgb(0xdd, 0xe1, 0xe7);
const MUTED: Color32 = Color32::from_rgb(0x8b, 0x93, 0x9f);
const ACCENT: Color32 = Color32::from_rgb(0xf2, 0xa2, 0x2c);
const ON_ACCENT: Color32 = Color32::from_rgb(0x1c, 0x13, 0x04);
const DANGER: Color32 = Color32::from_rgb(0xe0, 0x5a, 0x50);
const DANGER_BG: Color32 = Color32::from_rgb(0x3a, 0x20, 0x20);
const SUCCESS: Color32 = Color32::from_rgb(0x5c, 0xc2, 0x78);

fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).size(12.5).color(MUTED)
}

const VEHICLE_TYPES: [(&str, &str); 5] = [
    ("", "All vehicles"),
    ("tank", "Ground"),
    ("aircraft", "Aircraft"),
    ("helicopter", "Helicopters"),
    ("ship", "Naval"),
];

const SETTINGS_FILE: &str = "settings.json";
/// file link -> upload date (unix seconds; 0 = the server sent none). Keyed by link, so a
/// re-uploaded file (new link) is looked up again.
const FILE_DATES_FILE: &str = "file_dates.json";
const DATE_WORKERS: usize = 4;
/// Shown in the app and used for the release tag (Cargo needs the semver form "0.1.0-alpha").
const APP_VERSION: &str = "0.1.1a";

// ---------------------------------------------------------------------------------------------
// App state

enum Msg {
    Feed { gen: u64, result: Result<Vec<Post>, String> },
    Progress { id: u64, done: u64, total: Option<u64> },
    Installing { id: u64 },
    InstallDone { id: u64, result: Result<Installed, String> },
    Detail(Result<Post, String>),
    Latest(Result<MajorUpdate, String>),
    FileDate { link: String, ts: Option<i64> },
}

#[derive(Default)]
struct Job {
    done: u64,
    total: Option<u64>,
    installing: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Browse(Kind),
    Installed,
}

enum Action {
    Install(Post),
    Uninstall(u64),
    Detail(Post),
    OpenPath(PathBuf),
    OpenUrl(String),
    Search(String),
    NeedDate(String),
    /// `None` = War Thunder's default hangar.
    SetHangar(Option<String>),
    HangarChoice(u64, HangarChoice),
}

#[derive(Clone, Copy)]
enum HangarChoice {
    Use,
    Keep,
    Remove,
}

struct Toast {
    text: String,
    error: bool,
    until: f64,
}

struct App {
    ctx: egui::Context,
    settings: storage::Settings,
    manifest: Manifest,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    view: View,
    sort: Sort,
    search: String,
    vehicle_type: &'static str,

    posts: Vec<Post>,
    page: u32,
    loading: bool,
    end: bool,
    feed_err: Option<String>,
    /// Bumped on every new query so answers to old queries are ignored.
    gen: u64,

    jobs: HashMap<u64, Job>,
    detail: Option<Post>,
    detail_image: usize,
    detail_loading: bool,
    toasts: Vec<Toast>,
    settings_open: bool,
    setup_error: Option<String>,

    /// Major updates, newest first (built-in list, plus the latest one read from the changelog).
    majors: Vec<MajorUpdate>,
    majors_live: bool,
    game_version: Option<String>,
    file_dates: HashMap<String, i64>,
    dates_pending: HashSet<String>,
    dates_dirty: bool,
    dates_saved_at: f64,
    date_tx: Sender<String>,
    only_current: bool,

    /// `hangarBlk` value from config.blk; `None` = default hangar.
    current_hangar: Option<String>,
    /// A just-installed hangar waiting for "use it / keep current / remove".
    hangar_prompt: Option<u64>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let mut settings: storage::Settings = storage::load(SETTINGS_FILE);
        let previous_root = settings.game_root.clone();
        settings.game_root = game::find_game_root(settings.game_root.as_deref());
        if settings.user_id.as_ref().map_or(true, |u| !game::user_ids().contains(u)) {
            settings.user_id = game::preferred_user_id();
        }
        let _ = storage::save(SETTINGS_FILE, &settings);

        let (tx, rx) = channel();

        // A small pool that looks up file upload dates (HEAD requests) without blocking the UI.
        let (date_tx, date_rx) = channel::<String>();
        let date_rx = Arc::new(Mutex::new(date_rx));
        for _ in 0..DATE_WORKERS {
            let (rx, tx, ctx) = (date_rx.clone(), tx.clone(), cc.egui_ctx.clone());
            thread::spawn(move || loop {
                let next = rx.lock().map(|r| r.recv());
                let Ok(Ok(link)) = next else { break };
                let ts = api::file_last_modified(&link);
                if tx.send(Msg::FileDate { link, ts }).is_err() {
                    break;
                }
                ctx.request_repaint();
            });
        }
        {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            thread::spawn(move || {
                let _ = tx.send(Msg::Latest(updates::fetch_latest()));
                ctx.request_repaint();
            });
        }

        let game_version = settings.game_root.as_deref().and_then(game::installed_version);
        let mut app = App {
            ctx: cc.egui_ctx.clone(),
            settings,
            manifest: Manifest::load(),
            tx,
            rx,
            view: View::Browse(Kind::Camouflage),
            sort: Sort::PopularWeek,
            search: String::new(),
            vehicle_type: "",
            posts: Vec::new(),
            page: 0,
            loading: false,
            end: false,
            feed_err: None,
            gen: 0,
            jobs: HashMap::new(),
            detail: None,
            detail_image: 0,
            detail_loading: false,
            toasts: Vec::new(),
            settings_open: false,
            setup_error: None,
            majors: updates::known_updates(),
            majors_live: false,
            game_version,
            file_dates: storage::load(FILE_DATES_FILE),
            dates_pending: HashSet::new(),
            dates_dirty: false,
            dates_saved_at: 0.0,
            date_tx,
            only_current: false,
            current_hangar: None,
            hangar_prompt: None,
        };
        app.refresh_hangar();
        if let (Some(old), Some(new)) = (&previous_root, &app.settings.game_root) {
            if old != new {
                app.toast(
                    format!(
                        "Switched the War Thunder folder to {}. The old folder ({}) only contained the launcher, not the game.",
                        new.display(),
                        old.display()
                    ),
                    false,
                );
            }
        }
        if app.settings.game_root.is_some() {
            app.reload();
        }
        app
    }

    // ---- hangars -------------------------------------------------------------------------------

    fn refresh_hangar(&mut self) {
        self.current_hangar = self.settings.game_root.as_deref().and_then(|r| game::get_hangar(r).ok().flatten());
    }

    /// Installed hangars: (item, hangarBlk value).
    fn installed_hangars(&self) -> Vec<(&Installed, &str)> {
        self.manifest
            .items
            .iter()
            .filter_map(|i| match &i.config {
                Some(installer::ConfigChange::Hangar { value }) => Some((i, value.as_str())),
                _ => None,
            })
            .collect()
    }

    fn hangar_active(&self, value: &str) -> bool {
        game::hangar_matches(self.current_hangar.as_deref(), value)
    }

    fn hangar_display(&self) -> String {
        match &self.current_hangar {
            None => "War Thunder's default hangar".into(),
            Some(v) => self
                .installed_hangars()
                .into_iter()
                .find(|(_, value)| game::hangar_matches(Some(v), value))
                .map(|(i, _)| format!("\"{}\"", i.title))
                .unwrap_or_else(|| format!("a custom hangar ({v})")),
        }
    }

    fn set_hangar(&mut self, value: Option<String>) {
        let Some(root) = self.settings.game_root.clone() else { return };
        if game::is_game_running() {
            self.toast("Close War Thunder first: the game rewrites config.blk when it exits.", true);
            return;
        }
        match game::set_hangar(&root, value.as_deref()) {
            Ok(()) => {
                self.refresh_hangar();
                let msg = match value {
                    Some(_) => format!("Hangar switched to {}. Restart War Thunder to see it.", self.hangar_display()),
                    None => "Default hangar restored. Restart War Thunder to see it.".to_string(),
                };
                self.toast(msg, false);
            }
            Err(e) => self.toast(e, true),
        }
    }

    /// Hangar status, a switch per installed hangar and a "default hangar" button.
    fn hangar_panel(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let hangars = self.installed_hangars();
        if hangars.is_empty() && self.current_hangar.is_none() {
            return;
        }
        Frame::none()
            .fill(CARD)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .rounding(8.0)
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    icons::show(ui, Icon::Kind(Kind::Location), 18.0, ACCENT);
                    ui.label(RichText::new("Hangar").family(bold()).size(16.0).color(TEXT));
                    ui.add_space(8.0);
                    ui.label(muted("In use:"));
                    ui.label(RichText::new(self.hangar_display()).size(13.0).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let custom = self.current_hangar.is_some();
                        let r = button(ui, None, "Use default hangar", if custom { ACCENT } else { BTN }, if custom { ON_ACCENT } else { MUTED }, None, custom);
                        if r.on_hover_text("Remove the custom hangar line from config.blk").clicked() {
                            actions.push(Action::SetHangar(None));
                        }
                    });
                });
                if hangars.is_empty() {
                    ui.label(muted("This hangar was set outside WT Armory Manager."));
                }
                for (item, value) in hangars {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let on = self.hangar_active(value);
                        if toggle(ui, on).on_hover_text(if on { "Switch back to the default hangar" } else { "Use this hangar" }).clicked() {
                            actions.push(Action::SetHangar(if on { None } else { Some(value.to_string()) }));
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new(&item.title).size(14.0).color(if on { TEXT } else { TEXT.gamma_multiply(0.8) }));
                        ui.label(muted(value));
                    });
                }
                ui.add_space(2.0);
                ui.label(muted("Only one custom hangar can be active. Restart War Thunder after switching."));
            });
        ui.add_space(10.0);
    }

    fn hangar_prompt_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(id) = self.hangar_prompt else { return };
        let Some(item) = self.manifest.get(id) else {
            self.hangar_prompt = None;
            return;
        };
        let title = item.title.clone();
        // Dim the app behind the dialog.
        egui::Area::new(egui::Id::new("hangar_dim"))
            .order(egui::Order::Middle)
            .fixed_pos(pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                ui.allocate_rect(screen, Sense::click());
                ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(150));
            });
        egui::Window::new(RichText::new("Use your new hangar?").family(bold()).size(17.0).color(TEXT))
            .id(egui::Id::new("hangar_prompt"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.label(RichText::new(format!("\"{title}\" is installed.")).size(14.0).color(TEXT));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "Using it will replace your current hangar ({}). You can switch back to the default hangar at any time on the Installed page.",
                        self.hangar_display()
                    ))
                    .size(13.5)
                    .color(MUTED),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if button(ui, Some(Icon::Check), "Use new hangar", ACCENT, ON_ACCENT, None, true).clicked() {
                        actions.push(Action::HangarChoice(id, HangarChoice::Use));
                    }
                    if button(ui, None, "Keep current hangar", BTN, TEXT, None, true).clicked() {
                        actions.push(Action::HangarChoice(id, HangarChoice::Keep));
                    }
                    if button(ui, None, "Remove it", DANGER_BG, DANGER, None, true).on_hover_text("Uninstall the hangar again").clicked() {
                        actions.push(Action::HangarChoice(id, HangarChoice::Remove));
                    }
                });
            });
    }

    // ---- compatibility ------------------------------------------------------------------------

    /// When the post's file was last uploaded; `None` while the lookup is still running.
    fn updated_of(&self, p: &Post) -> Option<i64> {
        let Some(file) = &p.file else { return Some(p.created) };
        self.file_dates.get(&file.link).map(|&ts| ts.max(p.created))
    }

    fn compat_of(&self, p: &Post) -> Option<Compat> {
        match self.updated_of(p) {
            Some(ts) => Some(updates::check(ts, &self.majors)),
            // Posted after the latest major: can only be current, no need to wait for the lookup.
            None if updates::check(p.created, &self.majors) == Compat::Current => Some(Compat::Current),
            None => None,
        }
    }

    fn request_date(&mut self, link: String) {
        if !self.file_dates.contains_key(&link) && self.dates_pending.insert(link.clone()) {
            let _ = self.date_tx.send(link);
        }
    }

    fn save_file_dates_if_needed(&mut self) {
        let now = self.ctx.input(|i| i.time);
        if self.dates_dirty && now - self.dates_saved_at > 3.0 {
            let _ = storage::save(FILE_DATES_FILE, &self.file_dates);
            self.dates_dirty = false;
            self.dates_saved_at = now;
        }
    }

    fn save_settings(&self) {
        let _ = storage::save(SETTINGS_FILE, &self.settings);
    }

    fn save_manifest(&mut self) {
        if let Err(e) = self.manifest.save() {
            self.toast(format!("Could not save the list of installed items: {e}"), true);
        }
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        let now = self.ctx.input(|i| i.time);
        let until = now + 4.0 + text.len() as f64 / 22.0;
        self.toasts.push(Toast { text, error, until });
    }

    // ---- feed ---------------------------------------------------------------------------------

    fn reload(&mut self) {
        self.gen += 1;
        self.posts.clear();
        self.page = 0;
        self.end = false;
        self.loading = false;
        self.feed_err = None;
        self.fetch_next();
    }

    fn fetch_next(&mut self) {
        let View::Browse(kind) = self.view else { return };
        if self.loading || self.end {
            return;
        }
        self.loading = true;
        let (tx, ctx, gen, sort, page) = (self.tx.clone(), self.ctx.clone(), self.gen, self.sort, self.page);
        let search = self.search.clone();
        let vehicle_type = if kind == Kind::Camouflage { self.vehicle_type } else { "" };
        thread::spawn(move || {
            let result = api::fetch_feed(kind, sort, &search, vehicle_type, page);
            let _ = tx.send(Msg::Feed { gen, result });
            ctx.request_repaint();
        });
    }

    fn open_detail(&mut self, post: Post) {
        let id = post.lang_group;
        self.detail = Some(post);
        self.detail_image = 0;
        self.detail_loading = true;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(Msg::Detail(api::fetch_post(id)));
            ctx.request_repaint();
        });
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Feed { gen, result } if gen == self.gen => {
                    self.loading = false;
                    match result {
                        Ok(list) => {
                            if list.is_empty() {
                                self.end = true;
                            }
                            let seen: HashSet<u64> = self.posts.iter().map(|p| p.lang_group).collect();
                            self.posts.extend(list.into_iter().filter(|p| !seen.contains(&p.lang_group)));
                            self.page += 1;
                        }
                        Err(e) => self.feed_err = Some(e),
                    }
                }
                Msg::Feed { .. } => {}
                Msg::Progress { id, done, total } => {
                    if let Some(job) = self.jobs.get_mut(&id) {
                        job.done = done;
                        job.total = total;
                    }
                }
                Msg::Installing { id } => {
                    if let Some(job) = self.jobs.get_mut(&id) {
                        job.installing = true;
                    }
                }
                Msg::InstallDone { id, result } => {
                    self.jobs.remove(&id);
                    match result {
                        Ok(item) => {
                            let (title, note) = (item.title.clone(), item.note.clone());
                            let is_hangar = matches!(item.config, Some(installer::ConfigChange::Hangar { .. }));
                            self.manifest.items.retain(|i| i.lang_group != id);
                            self.manifest.items.push(item);
                            self.save_manifest();
                            self.toast(format!("Installed \"{title}\""), false);
                            if is_hangar {
                                self.hangar_prompt = Some(id); // ask before replacing the current hangar
                            } else if !note.is_empty() {
                                self.toast(note, false);
                            }
                        }
                        Err(e) => self.toast(e, true),
                    }
                }
                Msg::Latest(Ok(latest)) => {
                    self.majors = updates::merged(Some(&latest));
                    self.majors_live = true;
                }
                Msg::Latest(Err(_)) => {} // keep the built-in list
                Msg::FileDate { link, ts } => {
                    self.dates_pending.remove(&link);
                    self.file_dates.insert(link, ts.unwrap_or(0));
                    self.dates_dirty = true;
                }
                Msg::Detail(result) => {
                    self.detail_loading = false;
                    if let (Ok(full), Some(current)) = (result, self.detail.as_mut()) {
                        if current.lang_group == full.lang_group {
                            *current = full;
                        }
                    }
                }
            }
        }
    }

    // ---- install / uninstall ------------------------------------------------------------------

    fn start_install(&mut self, post: Post) {
        let Some(root) = self.settings.game_root.clone() else { return };
        let id = post.lang_group;
        if self.jobs.contains_key(&id) {
            return;
        }
        let target = installer::Target { root, user_id: self.settings.user_id.clone() };
        self.jobs.insert(id, Job::default());
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let result = (|| -> Result<Installed, String> {
                let file = post.file.clone().ok_or("This post has no file to download")?;
                let cache = storage::cache_dir().join(id.to_string());
                let dest = cache.join(storage::sanitize(&file.name));
                let file_date = api::download(&file.link, &dest, |done, total| {
                    let _ = tx.send(Msg::Progress { id, done, total });
                    ctx.request_repaint();
                })?;
                let _ = tx.send(Msg::Installing { id });
                ctx.request_repaint();
                let result = installer::install(&post, &target, &dest, file_date);
                let _ = std::fs::remove_dir_all(&cache);
                result
            })();
            let _ = tx.send(Msg::InstallDone { id, result });
            ctx.request_repaint();
        });
    }

    fn uninstall(&mut self, id: u64) {
        let Some(root) = self.settings.game_root.clone() else { return };
        let Some(pos) = self.manifest.items.iter().position(|i| i.lang_group == id) else { return };
        let item = self.manifest.items[pos].clone();
        match installer::uninstall(&item, &root, &self.manifest.items) {
            Ok(()) => {
                self.manifest.items.remove(pos);
                self.save_manifest();
                let was_active = self.current_hangar.is_some();
                self.refresh_hangar();
                let restored = if was_active && self.current_hangar.is_none() { " The default hangar is back." } else { "" };
                self.toast(format!("Removed \"{}\".{restored}", item.title), false);
            }
            Err(e) => self.toast(e, true),
        }
    }

    fn pick_game_folder(&mut self) -> bool {
        let Some(dir) = rfd::FileDialog::new().set_title("Choose your War Thunder folder").pick_folder() else {
            return false;
        };
        // Accept the parent folder too (e.g. "C:\Games" when the game is in "C:\Games\WarThunder").
        let found = [dir.clone(), dir.join("WarThunder"), dir.join("War Thunder")]
            .into_iter()
            .find(|p| game::is_game_root(p));
        if found.is_none() && game::is_launcher_only(&dir) {
            let msg = format!(
                "{} only contains the War Thunder launcher, not the game itself. Choose the folder that has the \
                 win64 folder in it (for Steam usually <library>\\steamapps\\common\\War Thunder).",
                dir.display()
            );
            self.setup_error = Some(msg.clone());
            self.toast(msg, true);
            return false;
        }
        match found {
            Some(root) => {
                self.game_version = game::installed_version(&root);
                self.settings.game_root = Some(root);
                self.save_settings();
                self.refresh_hangar();
                self.setup_error = None;
                true
            }
            None => {
                let msg = format!("No War Thunder installation in {} (win64\\aces.exe was not found).", dir.display());
                self.setup_error = Some(msg.clone());
                self.toast(msg, true);
                false
            }
        }
    }

    // ---- UI -----------------------------------------------------------------------------------

    fn top_bar(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::TopBottomPanel::top("top")
            .exact_height(54.0)
            .frame(Frame::none().fill(PANEL).inner_margin(Margin::symmetric(18.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    icons::show(ui, Icon::Kind(Kind::Sight), 24.0, ACCENT);
                    ui.add_space(4.0);
                    ui.label(RichText::new("WT ARMORY").family(bold()).size(18.0).color(TEXT));
                    ui.label(RichText::new("MANAGER").size(18.0).color(ACCENT));
                    ui.add_space(14.0);
                    if let Some(latest) = self.majors.first() {
                        let source = if self.majors_live { "from the official changelog" } else { "built-in list; changelog not reachable yet" };
                        ui.label(muted(format!("Latest update: {}  ·  {}", latest.label(), updates::fmt_date(latest.date))))
                            .on_hover_text(format!("Latest major War Thunder update ({source}). Mods uploaded before it may need an update."));
                    }
                    if let Some(v) = &self.game_version {
                        let latest_v = self.majors.first().map(|m| m.version.clone()).unwrap_or_default();
                        let outdated = !latest_v.is_empty() && !v.starts_with(&format!("{latest_v}."));
                        ui.label(muted("  ·  "));
                        let text = RichText::new(format!("Your game: {v}")).size(12.5).color(if outdated { ACCENT } else { MUTED });
                        ui.label(text).on_hover_text(if outdated {
                            "Your game is older than the latest major update. Update War Thunder in its launcher."
                        } else {
                            "Installed War Thunder version (content\\pkg_main.ver)"
                        });
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, Some(Icon::Gear), "Settings", BTN, TEXT, None, true).clicked() {
                            self.settings_open = !self.settings_open;
                        }
                        ui.add_space(6.0);
                        if let Some(root) = &self.settings.game_root {
                            let label = egui::Label::new(muted(api::truncate(&root.display().to_string(), 64)))
                                .sense(Sense::click());
                            if ui.add(label).on_hover_text("Open the game folder").clicked() {
                                actions.push(Action::OpenPath(root.clone()));
                            }
                            icons::show(ui, Icon::Folder, 14.0, MUTED);
                        }
                    });
                });
            });
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        let mut new_view = None;
        egui::SidePanel::left("nav")
            .exact_width(222.0)
            .resizable(false)
            .frame(Frame::none().fill(SIDEBAR).inner_margin(Margin::symmetric(10.0, 16.0)))
            .show(ctx, |ui| {
                section_label(ui, "BROWSE");
                for kind in Kind::ALL {
                    if nav_item(ui, Icon::Kind(kind), kind.label(), None, self.view == View::Browse(kind)).clicked() {
                        new_view = Some(View::Browse(kind));
                    }
                }
                ui.add_space(16.0);
                section_label(ui, "LIBRARY");
                let count = self.manifest.items.len() + self.jobs.len();
                if nav_item(ui, Icon::Library, "Installed", Some(count), self.view == View::Installed).clicked() {
                    new_view = Some(View::Installed);
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(RichText::new(format!("Version {APP_VERSION}")).size(11.0).color(MUTED.gamma_multiply(0.8)));
                    ui.label(RichText::new("Content is made by the War Thunder community. Not affiliated with Gaijin Entertainment.").size(11.0).color(MUTED.gamma_multiply(0.8)));
                });
            });
        if let Some(view) = new_view {
            if view != self.view {
                self.view = view;
                if let View::Browse(_) = view {
                    self.vehicle_type = "";
                    self.reload();
                }
            }
        }
    }

    fn browse(&mut self, ui: &mut egui::Ui, kind: Kind, actions: &mut Vec<Action>) {
        let mut reload = false;
        ui.horizontal(|ui| {
            icons::show(ui, Icon::Kind(kind), 24.0, ACCENT);
            ui.label(RichText::new(kind.label()).family(bold()).size(23.0).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if button(ui, Some(Icon::External), "Open on website", BTN, TEXT, None, true).clicked() {
                    actions.push(Action::OpenUrl(format!("{}{}", api::BASE, kind.site_path())));
                }
                if button(ui, Some(Icon::Refresh), "", BTN, TEXT, None, true).on_hover_text("Refresh").clicked() {
                    reload = true;
                }
            });
        });
        ui.label(muted(installer::describe_target(kind)));
        ui.add_space(10.0);
        if kind == Kind::Location {
            self.hangar_panel(ui, actions);
        }

        ui.horizontal_wrapped(|ui| {
            for sort in Sort::ALL {
                if chip(ui, sort.label(), self.sort == sort).clicked() && self.sort != sort {
                    self.sort = sort;
                    reload = true;
                }
            }
            ui.add_space(14.0);
            let latest = self.majors.first().map(|m| m.version.clone()).unwrap_or_default();
            let label = if latest.is_empty() { "Only up to date".to_string() } else { format!("Only made for {latest}+") };
            if chip(ui, &label, self.only_current)
                .on_hover_text("Hide mods uploaded before the latest major War Thunder update")
                .clicked()
            {
                self.only_current = !self.only_current;
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            icons::show(ui, Icon::Search, 16.0, MUTED);
            let edit = egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search hashtags, e.g. leopard or t_34")
                .desired_width(290.0)
                .margin(vec2(8.0, 6.0));
            let r = ui.add(edit);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                reload = true;
            }
            if button(ui, None, "Search", BTN, TEXT, None, true).clicked() {
                reload = true;
            }
            if !self.search.is_empty() && button(ui, None, "Clear", BTN, MUTED, None, true).clicked() {
                self.search.clear();
                reload = true;
            }
            if kind == Kind::Camouflage {
                ui.add_space(14.0);
                for (code, label) in VEHICLE_TYPES {
                    if chip(ui, label, self.vehicle_type == code).clicked() && self.vehicle_type != code {
                        self.vehicle_type = code;
                        reload = true;
                    }
                }
            }
        });
        ui.add_space(12.0);
        if reload {
            self.reload();
        }

        // With the filter on, every loaded post needs its date (hidden cards can't ask for it).
        if self.only_current {
            let missing: Vec<String> = self
                .posts
                .iter()
                .filter(|p| self.compat_of(p).is_none())
                .filter_map(|p| p.file.as_ref().map(|f| f.link.clone()))
                .collect();
            missing.into_iter().for_each(|l| self.request_date(l));
        }
        let shown: Vec<&Post> = self
            .posts
            .iter()
            .filter(|p| !self.only_current || self.compat_of(p) == Some(Compat::Current))
            .collect();
        let hidden = self.posts.len() - shown.len();

        let (mut want_more, mut retry) = (false, false);
        egui::ScrollArea::vertical()
            .id_salt(("feed", self.gen))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let gap = 14.0;
                let avail = ui.available_width() - 6.0;
                let cols = (((avail + gap) / (250.0 + gap)).floor() as usize).max(1);
                let card_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
                for row in shown.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for post in row {
                            self.card(ui, post, card_w, actions);
                        }
                    });
                    ui.add_space(gap);
                }
                ui.add_space(4.0);
                ui.vertical_centered(|ui| {
                    if hidden > 0 {
                        ui.label(muted(format!("{hidden} older or still-checking item(s) hidden by the filter")));
                    }
                    if self.loading {
                        ui.add(egui::Spinner::new().size(28.0).color(ACCENT));
                        ui.label(muted("Loading..."));
                    } else if let Some(e) = &self.feed_err {
                        ui.label(RichText::new(e).color(DANGER));
                        if button(ui, Some(Icon::Refresh), "Try again", BTN, TEXT, None, true).clicked() {
                            retry = true;
                        }
                    } else if self.end {
                        ui.label(muted(if self.posts.is_empty() {
                            "Nothing found. Try another search or sort order."
                        } else {
                            "That's everything."
                        }));
                    } else {
                        let r = button(ui, None, "Load more", BTN, TEXT, None, true);
                        // Infinite scroll: load the next page as soon as the button scrolls into view.
                        if r.clicked() || ui.is_rect_visible(r.rect) {
                            want_more = true;
                        }
                    }
                });
                ui.add_space(24.0);
            });
        if retry {
            self.feed_err = None;
            want_more = true;
        }
        if want_more && self.feed_err.is_none() {
            self.fetch_next();
        }
    }

    fn card(&self, ui: &mut egui::Ui, p: &Post, w: f32, actions: &mut Vec<Action>) {
        let img_h = (w * 9.0 / 16.0).round();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, img_h + 122.0), Sense::click());
        if !ui.is_rect_visible(rect) {
            return; // off screen: skip painting and image loading
        }
        let hovered = resp.contains_pointer();
        ui.painter().rect(
            rect,
            8.0,
            if hovered { CARD_HOVER } else { CARD },
            Stroke::new(1.0_f32, if hovered { ACCENT.gamma_multiply(0.55) } else { BORDER }),
        );
        let img_rect = Rect::from_min_size(rect.min, vec2(w, img_h)).shrink(1.0);
        let kind = p.kind().unwrap_or(Kind::Camouflage);
        paint_thumb(ui, p, kind, img_rect, Rounding { nw: 7.0, ne: 7.0, sw: 0.0, se: 0.0 });
        if let Some(f) = &p.file {
            badge(ui.painter(), img_rect.right_bottom() + vec2(-8.0, -8.0), Align2::RIGHT_BOTTOM, &fmt_size(f.size), Color32::from_black_alpha(190), TEXT);
        }
        if self.manifest.get(p.lang_group).is_some() {
            badge(ui.painter(), img_rect.left_top() + vec2(8.0, 8.0), Align2::LEFT_TOP, "INSTALLED", SUCCESS, Color32::from_rgb(0x08, 0x20, 0x10));
        }
        match self.compat_of(p) {
            Some(compat) => {
                let (text, bg, fg) = compat_style(compat, kind, &self.majors);
                let r = badge(ui.painter(), img_rect.right_top() + vec2(-8.0, 8.0), Align2::RIGHT_TOP, &text, bg, fg);
                ui.interact(r, ui.id().with(("compat", p.lang_group)), Sense::hover())
                    .on_hover_text(updates::explain(compat, kind, &self.majors));
            }
            None => {
                if let Some(f) = &p.file {
                    actions.push(Action::NeedDate(f.link.clone()));
                }
                badge(ui.painter(), img_rect.right_top() + vec2(-8.0, 8.0), Align2::RIGHT_TOP, "CHECKING...", Color32::from_black_alpha(170), MUTED);
            }
        }

        let body = Rect::from_min_max(pos2(rect.left() + 12.0, img_rect.bottom() + 10.0), rect.max - vec2(12.0, 12.0));
        // A detached child Ui: stacks top-down inside the card and takes no extra space in the row.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)));
        {
            let ui = &mut child;
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add(egui::Label::new(RichText::new(p.title()).family(bold()).size(15.0).color(TEXT)).truncate().selectable(false));
            let when = match self.updated_of(p) {
                Some(u) if u > p.created + 2 * 86_400 => format!("updated {}", ago(u)),
                _ => ago(p.created),
            };
            ui.add(egui::Label::new(muted(format!("by {}  ·  {when}", p.author_name()))).truncate().selectable(false));
            ui.horizontal(|ui| {
                stat(ui, Icon::Heart, p.likes, "likes");
                ui.add_space(8.0);
                stat(ui, Icon::Download, p.downloads, "downloads");
            });
            ui.add_space(4.0);
            let width = ui.available_width();
            self.install_controls(ui, p, width, actions);
        }
        if resp.clicked() {
            actions.push(Action::Detail(p.clone()));
        }
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    fn install_controls(&self, ui: &mut egui::Ui, p: &Post, width: f32, actions: &mut Vec<Action>) {
        let id = p.lang_group;
        if let Some(job) = self.jobs.get(&id) {
            let frac = job.total.filter(|t| *t > 0).map(|t| job.done as f32 / t as f32);
            let text = if job.installing {
                "Installing...".to_string()
            } else if let Some(f) = frac {
                format!("Downloading  {:.0}%", f * 100.0)
            } else {
                format!("Downloading  {}", fmt_size(job.done))
            };
            ui.add(
                egui::ProgressBar::new(if job.installing { 1.0 } else { frac.unwrap_or(0.0) })
                    .desired_width(width)
                    .desired_height(30.0)
                    .fill(ACCENT.gamma_multiply(0.7))
                    .rounding(6.0)
                    .animate(job.installing || frac.is_none())
                    .text(RichText::new(text).color(TEXT).size(13.0)),
            );
        } else if let Some(item) = self.manifest.get(id) {
            let in_use = matches!(&item.config, Some(installer::ConfigChange::Hangar { value }) if self.hangar_active(value));
            ui.allocate_ui_with_layout(vec2(width, 30.0), Layout::left_to_right(Align::Center), |ui| {
                icons::show(ui, Icon::Check, 16.0, SUCCESS);
                ui.label(RichText::new(if in_use { "In use" } else { "Installed" }).family(bold()).color(SUCCESS));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if button(ui, None, "Uninstall", DANGER_BG, DANGER, None, true).clicked() {
                        actions.push(Action::Uninstall(id));
                    }
                });
            });
        } else if p.file.is_none() {
            button(ui, None, "No file attached", BTN, MUTED, Some(width), false);
        } else if button(ui, Some(Icon::Download), "Install", ACCENT, ON_ACCENT, Some(width), true).clicked() {
            actions.push(Action::Install(p.clone()));
        }
    }

    fn installed_view(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            icons::show(ui, Icon::Library, 24.0, ACCENT);
            ui.label(RichText::new("Installed").family(bold()).size(23.0).color(TEXT));
        });
        ui.label(muted("Everything installed with WT Armory Manager. Uninstalling removes exactly the files that were added and restores anything that was replaced."));
        ui.add_space(12.0);
        self.hangar_panel(ui, actions);

        if !self.jobs.is_empty() {
            ui.label(muted(format!("{} download(s) in progress...", self.jobs.len())));
            ui.add_space(6.0);
        }
        if self.manifest.items.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Nothing installed yet").family(bold()).size(17.0).color(TEXT));
                ui.label(muted("Pick a category on the left and press Install on anything you like."));
            });
            return;
        }

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for item in self.manifest.items.iter().rev() {
                Frame::none()
                    .fill(CARD)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .rounding(8.0)
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(136.0, 76.0), Sense::hover());
                            ui.painter().rect_filled(r, 6.0, IMG_BG);
                            match &item.thumb {
                                Some(url) => {
                                    egui::Image::new(url.as_str()).rounding(6.0).paint_at(ui, r);
                                }
                                None => icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(30.0, 30.0)), Icon::Kind(item.kind), MUTED),
                            }
                            ui.add_space(6.0);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if button(ui, None, "Uninstall", DANGER_BG, DANGER, None, true).clicked() {
                                    actions.push(Action::Uninstall(item.lang_group));
                                }
                                if let Some(loc) = item.location() {
                                    if button(ui, Some(Icon::Folder), "Show files", BTN, TEXT, None, true).clicked() {
                                        actions.push(Action::OpenPath(loc));
                                    }
                                }
                                if button(ui, Some(Icon::External), "", BTN, TEXT, None, true).on_hover_text("Open the post on live.warthunder.com").clicked() {
                                    actions.push(Action::OpenUrl(format!("{}/post/{}/en/", api::BASE, item.lang_group)));
                                }
                                ui.add_space(8.0);
                                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                    ui.add(egui::Label::new(RichText::new(&item.title).family(bold()).size(15.0).color(TEXT)).truncate());
                                    ui.horizontal(|ui| {
                                        icons::show(ui, Icon::Kind(item.kind), 13.0, ACCENT);
                                        ui.add(egui::Label::new(muted(format!(
                                            "{}  ·  by {}  ·  installed {}  ·  {} file(s)",
                                            item.kind.singular(),
                                            item.author,
                                            ago(item.installed_at),
                                            item.files.len()
                                        ))).truncate());
                                    });
                                    if item.updated_at > 0 {
                                        if let Compat::Behind(n) = updates::check(item.updated_at, &self.majors) {
                                            let color = if updates::sensitive(item.kind) { ACCENT } else { MUTED };
                                            ui.label(RichText::new(format!(
                                                "Made before {} ({n} major update{} ago). Check the post for a newer version if it misbehaves.",
                                                self.majors.get(n - 1).map(|m| m.label()).unwrap_or_default(),
                                                if n == 1 { "" } else { "s" }
                                            )).size(12.0).color(color));
                                        }
                                    }
                                    if !item.note.is_empty() {
                                        ui.label(RichText::new(&item.note).size(12.0).color(MUTED));
                                    }
                                });
                            });
                        });
                    });
                ui.add_space(8.0);
            }
        });
    }

    fn detail_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(p) = self.detail.clone() else { return };
        let mut open = true;
        let screen = ctx.screen_rect();
        let max_h = (screen.height() - 90.0).max(300.0);
        egui::Window::new(RichText::new(p.title()).family(bold()).size(16.0).color(TEXT))
            .id(egui::Id::new("detail"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .default_width(780.0_f32.min(screen.width() - 60.0))
            .default_height(max_h.min(800.0))
            .max_height(max_h)
            .max_width((screen.width() - 40.0).max(400.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let w = ui.available_width();
                    let shown = p.images.get(self.detail_image).or(p.images.first());
                    if let Some(img) = shown {
                        if let Some(url) = img.large() {
                            let ratio = img.ratio();
                            let (iw, ih) = if w / ratio > 500.0 { (500.0 * ratio, 500.0) } else { (w, w / ratio) };
                            let (r, _) = ui.allocate_exact_size(vec2(w, ih), Sense::hover());
                            let r = Rect::from_center_size(r.center(), vec2(iw, ih));
                            ui.painter().rect_filled(r, 6.0, IMG_BG);
                            egui::Image::new(url).rounding(6.0).paint_at(ui, r);
                        }
                    }
                    if p.images.len() > 1 {
                        ui.add_space(6.0);
                        egui::ScrollArea::horizontal().id_salt("thumbs").show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for (i, img) in p.images.iter().enumerate() {
                                    let (r, resp) = ui.allocate_exact_size(vec2(112.0, 63.0), Sense::click());
                                    ui.painter().rect_filled(r, 4.0, IMG_BG);
                                    if let Some(url) = img.thumb() {
                                        egui::Image::new(url).rounding(4.0).paint_at(ui, r);
                                    }
                                    let selected = i == self.detail_image;
                                    if selected || resp.hovered() {
                                        ui.painter().rect_stroke(r, 4.0, Stroke::new(2.0_f32, if selected { ACCENT } else { MUTED }));
                                    }
                                    if resp.clicked() {
                                        self.detail_image = i;
                                    }
                                }
                            });
                        });
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(p.author_name()).family(bold()).color(TEXT));
                        ui.label(muted(ago(p.created)));
                        ui.add_space(10.0);
                        stat(ui, Icon::Heart, p.likes, "likes");
                        ui.add_space(6.0);
                        stat(ui, Icon::Download, p.downloads, "downloads");
                        ui.add_space(6.0);
                        ui.label(muted(format!("{} views", compact(p.views))));
                    });
                    if let Some(f) = &p.file {
                        ui.label(muted(format!("{}   ({})", f.name, fmt_size(f.size))));
                    }
                    let kind = p.kind().unwrap_or(Kind::Camouflage);
                    let dates = match self.updated_of(&p) {
                        Some(u) if u > p.created + 2 * 86_400 => format!(
                            "Posted {}  ·  file updated {}",
                            updates::fmt_date(p.created),
                            updates::fmt_date(u)
                        ),
                        _ => format!("Posted {}", updates::fmt_date(p.created)),
                    };
                    ui.label(muted(dates));
                    match self.compat_of(&p) {
                        Some(compat) => {
                            let (text, bg, fg) = compat_style(compat, kind, &self.majors);
                            ui.horizontal_wrapped(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(0.0, 20.0), Sense::hover());
                                let br = badge(ui.painter(), r.left_center(), Align2::LEFT_CENTER, &text, bg, fg);
                                ui.add_space(br.width() + 6.0);
                                ui.label(RichText::new(updates::explain(compat, kind, &self.majors)).size(12.5).color(TEXT));
                            });
                        }
                        None => {
                            if let Some(f) = &p.file {
                                actions.push(Action::NeedDate(f.link.clone()));
                            }
                            ui.label(muted("Checking when the file was last updated..."));
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.allocate_ui(vec2(240.0, 32.0), |ui| self.install_controls(ui, &p, 240.0, actions));
                        if button(ui, Some(Icon::External), "Open on website", BTN, TEXT, None, true).clicked() {
                            actions.push(Action::OpenUrl(p.url()));
                        }
                    });
                    if let Some(kind) = p.kind() {
                        ui.label(muted(installer::describe_target(kind)));
                    }
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);
                    if self.detail_loading {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                            ui.label(muted("Loading full description..."));
                        });
                    }
                    ui.set_max_width(w);
                    ui.add(egui::Label::new(RichText::new(p.text()).size(14.0).color(TEXT)).wrap());
                    let tags = p.hashtags();
                    if !tags.is_empty() {
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            for tag in tags {
                                if chip(ui, &tag, false).on_hover_text("Search this hashtag").clicked() {
                                    actions.push(Action::Search(tag.trim_start_matches('#').to_string()));
                                }
                            }
                        });
                    }
                });
            });
        if !open {
            self.detail = None;
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut pick = false;
        egui::Window::new(RichText::new("Settings").family(bold()).size(16.0).color(TEXT))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(540.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("War Thunder folder").family(bold()).color(TEXT));
                let root = self.settings.game_root.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                ui.label(muted(root));
                if button(ui, Some(Icon::Folder), "Change folder...", BTN, TEXT, None, true).clicked() {
                    pick = true;
                }
                ui.add_space(14.0);
                ui.label(RichText::new("Account used for sights").family(bold()).color(TEXT));
                ui.label(muted("Custom sights are stored per account in Documents\\My Games\\WarThunder\\Saves\\<user ID>."));
                let ids = game::user_ids();
                if ids.is_empty() {
                    ui.label(RichText::new("No account folders found. Start War Thunder and log in once.").color(DANGER));
                }
                for id in ids {
                    let selected = self.settings.user_id.as_deref() == Some(id.as_str());
                    if ui.radio(selected, RichText::new(&id).color(TEXT)).clicked() && !selected {
                        self.settings.user_id = Some(id);
                        self.save_settings();
                    }
                }
                ui.add_space(14.0);
                ui.label(RichText::new("Backups").family(bold()).color(TEXT));
                ui.label(muted("A copy of config.blk is saved before every change (the last 15 are kept). Files that an install replaces are kept until you uninstall it."));
                ui.horizontal(|ui| {
                    if button(ui, Some(Icon::Folder), "Backups", BTN, TEXT, None, true).clicked() {
                        let dir = storage::backups_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        storage::open_in_explorer(&dir);
                    }
                    if button(ui, Some(Icon::Folder), "Control presets", BTN, TEXT, None, true).clicked() {
                        let dir = storage::controls_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        storage::open_in_explorer(&dir);
                    }
                });
            });
        if pick && self.pick_game_folder() {
            self.toast("War Thunder folder changed", false);
        }
        if !open {
            self.settings_open = false;
        }
    }

    fn setup_screen(&mut self, ctx: &egui::Context) {
        let mut pick = false;
        egui::CentralPanel::default().frame(Frame::none().fill(BG)).show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.16).max(24.0));
                icons::show(ui, Icon::Kind(Kind::Sight), 60.0, ACCENT);
                ui.add_space(14.0);
                ui.label(RichText::new("Welcome to WT Armory Manager").family(bold()).size(27.0).color(TEXT));
                ui.add_space(8.0);
                ui.label(RichText::new("War Thunder was not found in the standard install folders.").size(15.0).color(TEXT));
                ui.label(RichText::new("Choose the folder War Thunder is installed in (the one that contains the win64 folder).").size(14.0).color(MUTED));
                ui.add_space(18.0);
                if button(ui, Some(Icon::Folder), "Choose War Thunder folder...", ACCENT, ON_ACCENT, None, true).clicked() {
                    pick = true;
                }
                if let Some(e) = &self.setup_error {
                    ui.add_space(10.0);
                    ui.label(RichText::new(e).color(DANGER));
                }
                ui.add_space(28.0);
                ui.label(RichText::new("Folders that were checked").family(bold()).size(13.0).color(MUTED));
                for c in game::candidate_roots().iter().take(10) {
                    ui.label(muted(c.display().to_string()));
                }
            });
        });
        if pick && self.pick_game_folder() {
            self.reload();
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        let mut dismiss = None;
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-18.0, -18.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for (i, t) in self.toasts.iter().enumerate() {
                    let r = Frame::none()
                        .fill(PANEL)
                        .stroke(Stroke::new(1.0_f32, if t.error { DANGER } else { SUCCESS.gamma_multiply(0.7) }))
                        .rounding(8.0)
                        .inner_margin(Margin::symmetric(14.0, 10.0))
                        .shadow(egui::epaint::Shadow { offset: vec2(0.0, 4.0), blur: 14.0, spread: 0.0, color: Color32::from_black_alpha(120) })
                        .show(ui, |ui| {
                            ui.set_width(360.0);
                            ui.label(RichText::new(&t.text).size(13.0).color(if t.error { DANGER } else { TEXT }));
                        })
                        .response;
                    if r.interact(Sense::click()).on_hover_text("Click to dismiss").clicked() {
                        dismiss = Some(i);
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some(i) = dismiss {
            self.toasts.remove(i);
        }
        ctx.request_repaint_after(Duration::from_millis(400));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();
        if self.settings.game_root.is_none() {
            self.setup_screen(ctx);
            self.draw_toasts(ctx);
            return;
        }
        let mut actions = Vec::new();
        self.top_bar(ctx, &mut actions);
        self.sidebar(ctx);
        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG).inner_margin(Margin { left: 22.0, right: 16.0, top: 16.0, bottom: 0.0 }))
            .show(ctx, |ui| match self.view {
                View::Browse(kind) => self.browse(ui, kind, &mut actions),
                View::Installed => self.installed_view(ui, &mut actions),
            });
        self.detail_window(ctx, &mut actions);
        self.settings_window(ctx);
        self.hangar_prompt_window(ctx, &mut actions);

        for action in actions {
            match action {
                Action::Install(p) => self.start_install(p),
                Action::Uninstall(id) => self.uninstall(id),
                Action::Detail(p) => self.open_detail(p),
                Action::OpenPath(p) => storage::open_in_explorer(&p),
                Action::OpenUrl(u) => ctx.open_url(egui::OpenUrl::new_tab(u)),
                Action::Search(tag) => {
                    self.search = tag;
                    self.detail = None;
                    self.reload();
                }
                Action::NeedDate(link) => self.request_date(link),
                Action::SetHangar(value) => self.set_hangar(value),
                Action::HangarChoice(id, choice) => {
                    self.hangar_prompt = None;
                    match choice {
                        HangarChoice::Use => {
                            let value = self.manifest.get(id).and_then(|i| match &i.config {
                                Some(installer::ConfigChange::Hangar { value }) => Some(value.clone()),
                                _ => None,
                            });
                            if value.is_some() {
                                self.set_hangar(value);
                            }
                        }
                        HangarChoice::Keep => self.toast("Kept your current hangar. You can switch to the new one on the Installed page.", false),
                        HangarChoice::Remove => self.uninstall(id),
                    }
                }
            }
        }
        self.save_file_dates_if_needed();
        self.draw_toasts(ctx);
    }
}

// ---------------------------------------------------------------------------------------------
// Widgets

/// A button with an optional drawn icon. `width` makes it fixed width with centred content.
fn button(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    text: &str,
    fill: Color32,
    fg: Color32,
    width: Option<f32>,
    enabled: bool,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::new(14.0, bold()), fg);
    let (isz, gap, pad) = (15.0, 7.0, vec2(12.0, 7.0));
    let content_w = icon.map_or(0.0, |_| isz) + if icon.is_some() && !text.is_empty() { gap } else { 0.0 } + galley.size().x;
    let size = vec2(width.unwrap_or(content_w + pad.x * 2.0), 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        let bg = if !enabled {
            fill.gamma_multiply(0.6)
        } else if resp.is_pointer_button_down_on() {
            shade(fill, -14)
        } else if resp.hovered() {
            shade(fill, 18)
        } else {
            fill
        };
        ui.painter().rect_filled(rect, 6.0, bg);
        let mut x = rect.center().x - content_w / 2.0;
        if let Some(icon) = icon {
            icons::paint(ui.painter(), Rect::from_min_size(pos2(x, rect.center().y - isz / 2.0), vec2(isz, isz)), icon, fg);
            x += isz + gap;
        }
        if !text.is_empty() {
            ui.painter().galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
        }
    }
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// An on/off switch with an animated knob.
fn toggle(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let size = vec2(38.0, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, on);
        let track = if on { ACCENT } else { shade(BTN, 10) };
        let track = if resp.hovered() { shade(track, 14) } else { track };
        ui.painter().rect_filled(rect, rect.height() / 2.0, track);
        let r = rect.height() / 2.0 - 3.0;
        let x = egui::lerp((rect.left() + r + 3.0)..=(rect.right() - r - 3.0), t);
        ui.painter().circle_filled(pos2(x, rect.center().y), r, if on { ON_ACCENT } else { TEXT });
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn shade(c: Color32, d: i16) -> Color32 {
    let f = |v: u8| (v as i16 + d).clamp(0, 255) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

fn chip(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let b = egui::Button::new(RichText::new(text).size(13.0).color(if selected { ON_ACCENT } else { TEXT }))
        .fill(if selected { ACCENT } else { BTN })
        .stroke(Stroke::NONE)
        .rounding(14.0)
        .min_size(vec2(0.0, 28.0));
    let r = ui.add(b);
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    r
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(text).size(11.0).family(bold()).color(MUTED.gamma_multiply(0.85)));
    ui.add_space(4.0);
}

fn nav_item(ui: &mut egui::Ui, icon: Icon, text: &str, count: Option<usize>, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 6.0, ACCENT.gamma_multiply(0.13));
        p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 7.0), vec2(3.0, 22.0)), 2.0, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, 6.0, Color32::from_white_alpha(8));
    }
    let fg = if selected { ACCENT } else if resp.hovered() { TEXT } else { TEXT.gamma_multiply(0.85) };
    icons::paint(p, Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(17.0, 17.0)), icon, fg);
    let font = if selected { FontId::new(14.0, bold()) } else { FontId::proportional(14.0) };
    p.text(pos2(rect.left() + 42.0, rect.center().y), Align2::LEFT_CENTER, text, font, fg);
    if let Some(n) = count.filter(|n| *n > 0) {
        badge(p, pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, &n.to_string(), BTN, TEXT);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn badge(p: &egui::Painter, at: egui::Pos2, align: Align2, text: &str, bg: Color32, fg: Color32) -> Rect {
    let galley = p.layout_no_wrap(text.to_string(), FontId::new(11.0, bold()), fg);
    let rect = align.anchor_size(at, galley.size() + vec2(12.0, 5.0));
    p.rect_filled(rect, 4.0, bg);
    p.galley(rect.center() - galley.size() / 2.0, galley, fg);
    rect
}

/// Badge text and colours for a compatibility result. Types that rarely break (skins, controls)
/// stay neutral grey instead of warning colours.
fn compat_style(compat: Compat, kind: Kind, majors: &[MajorUpdate]) -> (String, Color32, Color32) {
    match compat {
        Compat::Current => {
            let v = majors.first().map(|m| m.version.as_str()).filter(|v| !v.is_empty());
            (v.map_or("UP TO DATE".into(), |v| format!("FOR {v}")), SUCCESS, Color32::from_rgb(0x08, 0x20, 0x10))
        }
        Compat::Behind(n) => {
            let text = format!("{n} UPDATE{} OLD", if n == 1 { "" } else { "S" });
            if !updates::sensitive(kind) {
                (text, Color32::from_black_alpha(190), TEXT)
            } else if n == 1 {
                (text, ACCENT, ON_ACCENT)
            } else {
                (text, DANGER, Color32::from_rgb(0x24, 0x06, 0x04))
            }
        }
    }
}

fn stat(ui: &mut egui::Ui, icon: Icon, n: u64, what: &str) {
    ui.spacing_mut().item_spacing.x = 4.0;
    icons::show(ui, icon, 13.0, MUTED).on_hover_text(format!("{n} {what}"));
    ui.label(RichText::new(compact(n)).size(12.5).color(MUTED));
}

/// Draws a post's thumbnail cropped to fill `rect` (like CSS object-fit: cover).
fn paint_thumb(ui: &mut egui::Ui, p: &Post, kind: Kind, rect: Rect, rounding: Rounding) {
    ui.painter().rect_filled(rect, rounding, IMG_BG);
    match p.images.first().and_then(|i| i.thumb().map(|u| (u, i.ratio()))) {
        Some((url, ratio)) => {
            let target = rect.width() / rect.height();
            let uv = if ratio > target {
                let w = target / ratio;
                Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
            } else {
                let h = ratio / target;
                Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
            };
            egui::Image::new(url).uv(uv).rounding(rounding).paint_at(ui, rect);
        }
        None => icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(46.0, 46.0)), Icon::Kind(kind), MUTED.gamma_multiply(0.5)),
    }
}

// ---------------------------------------------------------------------------------------------
// Formatting

fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1000..=999_999 => format!("{:.1}k", n as f64 / 1000.0).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0).replace(".0M", "M"),
    }
}

fn ago(ts: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let s = (now - ts).max(0);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match s {
        _ if ts <= 0 => String::new(),
        0..=59 => "just now".into(),
        60..=3599 => plural(s / 60, "minute"),
        3600..=86_399 => plural(s / 3600, "hour"),
        86_400..=2_591_999 => plural(s / 86_400, "day"),
        2_592_000..=31_535_999 => plural(s / 2_592_000, "month"),
        _ => plural(s / 31_536_000, "year"),
    }
}

// ---------------------------------------------------------------------------------------------
// Setup

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let read = |name: &str| std::fs::read(format!(r"C:\Windows\Fonts\{name}")).ok();
    if let Some(data) = read("segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), egui::FontData::from_owned(data));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "segoe".into());
    }
    let mut bold_family = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(data) = read("seguisb.ttf").or_else(|| read("segoeuib.ttf")) {
        fonts.font_data.insert("segoe_bold".into(), egui::FontData::from_owned(data));
        bold_family.insert(0, "segoe_bold".into());
    }
    // Fallbacks so Chinese, Japanese and Korean titles don't render as empty boxes.
    for (key, file) in [("cjk_sc", "msyh.ttc"), ("cjk_jp", "YuGothM.ttc"), ("cjk_kr", "malgun.ttf")] {
        if let Some(data) = read(file) {
            fonts.font_data.insert(key.into(), egui::FontData::from_owned(data));
            fonts.families.entry(FontFamily::Proportional).or_default().push(key.into());
            bold_family.push(key.into());
        }
    }
    fonts.families.insert(bold(), bold_family);
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_rounding = Rounding::same(10.0);
    v.extreme_bg_color = Color32::from_rgb(0x0d, 0x0f, 0x12);
    v.faint_bg_color = CARD;
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.override_text_color = None;
    for w in [&mut v.widgets.inactive, &mut v.widgets.noninteractive] {
        w.bg_stroke = Stroke::new(1.0_f32, BORDER);
    }
    v.widgets.inactive.weak_bg_fill = BTN;
    v.widgets.inactive.bg_fill = BTN;
    v.widgets.hovered.weak_bg_fill = shade(BTN, 18);
    v.widgets.hovered.bg_fill = shade(BTN, 18);
    v.widgets.active.weak_bg_fill = shade(BTN, -10);
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.6));
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 5.0);
        s.spacing.scroll.bar_width = 8.0;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        s.text_styles.insert(TextStyle::Heading, FontId::new(20.0, bold()));
    });
}

/// Window icon: an amber gunsight on a dark rounded square, drawn pixel by pixel.
fn app_icon() -> egui::IconData {
    const N: u32 = 64;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let d = (dx * dx + dy * dy).sqrt();
            let corner = (dx.abs() - 22.0).max(0.0).hypot((dy.abs() - 22.0).max(0.0));
            let inside = corner <= 9.5;
            let ring = (d - 19.0).abs() < 2.4;
            let cross = (dx.abs() < 1.6 && (dy.abs() > 8.0 && dy.abs() < 27.0)) || (dy.abs() < 1.6 && (dx.abs() > 8.0 && dx.abs() < 27.0));
            let dot = d < 3.0;
            let px = if !inside {
                [0, 0, 0, 0]
            } else if ring || cross || dot {
                [0xf2, 0xa2, 0x2c, 0xff]
            } else {
                [0x18, 0x1b, 0x20, 0xff]
            };
            rgba.extend_from_slice(&px);
        }
    }
    egui::IconData { rgba, width: N, height: N }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("WT Armory Manager")
            .with_inner_size([1320.0, 860.0])
            .with_min_inner_size([900.0, 600.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native("WT Armory Manager", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
