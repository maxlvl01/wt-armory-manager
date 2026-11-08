#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod game;
mod icons;
mod installer;
mod login;
mod storage;
mod updater;
mod updates;

use api::{Kind, Post, Sort};
use updates::{Compat, MajorUpdate};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontFamily, FontId, Frame, Layout, Margin, Rect, RichText, Rounding,
    Sense, Stroke, TextStyle,
};
use icons::Icon;
use installer::{Installed, Manifest};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// Downloads that run at the same time; the rest wait in the queue.
const MAX_PARALLEL_DOWNLOADS: usize = 2;
/// Finished downloads kept in the Downloads window.
const HISTORY_LEN: usize = 40;
/// Shown in the app and used for the release tag (Cargo needs the semver form "0.1.0-alpha").
const APP_VERSION: &str = "0.2.0a";

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
    UpdateCheck { manual: bool, result: Result<Option<updater::Release>, String> },
    UpdateProgress { done: u64, total: Option<u64> },
    UpdateInstalled(Result<PathBuf, String>),
    Profile(u64, Result<api::Profile, String>),
    UserPosts { user: u64, page: u32, result: Result<(Vec<Post>, bool), String> },
    FollowingPosts { gen: u64, result: Result<(Vec<Post>, bool), String> },
    Comments(u64, Result<Vec<api::Comment>, String>),
    Liked(u64, Result<(u64, bool), String>),
    CommentPosted(u64, Result<(), String>),
    Login(Result<Option<login::Session>, String>),
    /// Background check of a saved session: (user id the site sees, their profile).
    SessionChecked(Result<(u64, Option<api::Profile>), String>),
}

/// A creator's page: profile header and their posts.
struct CreatorPage {
    id: u64,
    nickname: String,
    profile: Option<api::Profile>,
    posts: Vec<Post>,
    page: u32,
    loading: bool,
    end: bool,
    err: Option<String>,
}

/// Comments of the post open in the detail window.
#[derive(Default)]
struct CommentsState {
    lang_group: u64,
    list: Vec<api::Comment>,
    loading: bool,
    err: Option<String>,
    draft: String,
    reply_to: Option<u64>,
    sending: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum JobState {
    Queued,
    Downloading,
    Installing,
}

/// One entry of the download queue (waiting, downloading or installing).
struct Job {
    post: Post,
    state: JobState,
    done: u64,
    total: Option<u64>,
    /// When the download started (egui time), for the speed display.
    started: f64,
    cancel: Arc<AtomicBool>,
}

/// A finished download, shown under "Finished" in the Downloads window.
struct Finished {
    post: Post,
    /// `None` = installed, `Some(text)` = failed or cancelled.
    error: Option<String>,
    at: i64,
}

enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(updater::Release),
    Downloading { release: updater::Release, done: u64, total: Option<u64> },
    Ready { release: updater::Release, exe: PathBuf },
    Failed(String),
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Browse(Kind),
    Installed,
    /// Newest posts from followed creators.
    Following,
    Creator(u64),
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
    /// Cancel a queued or running download.
    Cancel(u64),
    ClearFinished,
    InstallUpdate(updater::Release),
    Restart(PathBuf),
    OpenCreator(u64, String),
    /// Follow / unfollow a creator (id, nickname, avatar).
    ToggleFollow(u64, String, Option<String>),
    Like(u64),
    SendComment,
    ReplyTo(Option<u64>),
    Login,
    Logout,
    CreatorMore,
    OpenFollowing,
    ToggleHidden(u64, String),
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
    /// Queue order of `jobs` (first in, first out).
    job_order: Vec<u64>,
    finished: VecDeque<Finished>,
    downloads_open: bool,

    update: UpdateState,
    update_banner_hidden: bool,

    bug_open: bool,
    bug_title: String,
    bug_text: String,
    bug_include_errors: bool,
    /// Recent error messages (for bug reports).
    error_log: VecDeque<String>,

    session: Option<login::Session>,
    login_busy: bool,
    /// Likes the user changed in this session: lang_group -> (likes, liked).
    likes: HashMap<u64, (u64, bool)>,
    like_busy: HashSet<u64>,
    comments: CommentsState,
    creator: Option<CreatorPage>,
    following_posts: Vec<Post>,
    following_loading: usize,
    following_gen: u64,

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
            job_order: Vec::new(),
            finished: VecDeque::new(),
            downloads_open: false,
            update: UpdateState::Idle,
            update_banner_hidden: false,
            bug_open: false,
            bug_title: String::new(),
            bug_text: String::new(),
            bug_include_errors: true,
            error_log: VecDeque::new(),
            session: None,
            login_busy: false,
            likes: HashMap::new(),
            like_busy: HashSet::new(),
            comments: CommentsState::default(),
            creator: None,
            following_posts: Vec::new(),
            following_loading: 0,
            following_gen: 0,
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
        updater::cleanup_old();
        app.check_updates(false);
        if let Some(s) = login::load() {
            app.use_session(s);
        }
        app.dev_start_screen();
        app
    }

    /// Development aid for screenshots: `WTAM_SCREEN=sound|following|creator:<id>|downloads|bug`.
    fn dev_start_screen(&mut self) {
        let Ok(screen) = std::env::var("WTAM_SCREEN") else { return };
        let (what, arg) = screen.split_once(':').unwrap_or((screen.as_str(), ""));
        match what {
            "following" => {
                self.view = View::Following;
                self.load_following();
            }
            "creator" => self.open_creator(arg.parse().unwrap_or(0), String::new()),
            "downloads" => self.downloads_open = true,
            // the full post arrives via Msg::Detail
            "detail" => self.open_detail(Post { lang_group: arg.parse().unwrap_or(0), ..Default::default() }),
            "bug" => self.bug_open = true,
            "installed" => self.view = View::Installed,
            code => {
                if let Some(kind) = Kind::from_code(code) {
                    self.view = View::Browse(kind);
                    self.reload();
                }
            }
        }
    }

    // ---- account ------------------------------------------------------------------------------

    /// Starts using a saved session and checks in the background that the site still accepts it.
    fn use_session(&mut self, s: login::Session) {
        api::set_session(Some(s.cookies.clone()));
        self.session = Some(s);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let result = api::whoami().map(|uid| (uid, if uid != 0 { api::fetch_profile(uid).ok() } else { None }));
            let _ = tx.send(Msg::SessionChecked(result));
            ctx.request_repaint();
        });
    }

    fn start_login(&mut self) {
        if self.login_busy {
            return;
        }
        self.login_busy = true;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(Msg::Login(login::run_child()));
            ctx.request_repaint();
        });
    }

    fn logout(&mut self) {
        login::logout();
        api::set_session(None);
        self.session = None;
        self.likes.clear();
        self.toast("Logged out of live.warthunder.com.", false);
    }

    fn logged_in(&self) -> bool {
        self.session.is_some()
    }

    // ---- creators -----------------------------------------------------------------------------

    fn is_following(&self, id: u64) -> bool {
        self.settings.favourite_authors.contains_key(&id)
    }

    fn toggle_follow(&mut self, id: u64, nickname: String, avatar: Option<String>) {
        if id == 0 {
            return;
        }
        if self.settings.favourite_authors.remove(&id).is_some() {
            self.toast(format!("Unfollowed {nickname}."), false);
        } else {
            self.settings.favourite_authors.insert(id, storage::FavAuthor { nickname: nickname.clone(), avatar });
            self.toast(format!("Following {nickname}. Their newest posts show up under Followed creators."), false);
        }
        self.save_settings();
    }

    fn open_creator(&mut self, id: u64, nickname: String) {
        if id == 0 {
            return;
        }
        self.detail = None;
        self.view = View::Creator(id);
        if self.creator.as_ref().is_some_and(|c| c.id == id) {
            return;
        }
        self.creator = Some(CreatorPage { id, nickname, profile: None, posts: Vec::new(), page: 0, loading: false, end: false, err: None });
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(Msg::Profile(id, api::fetch_profile(id)));
            ctx.request_repaint();
        });
        self.creator_more();
    }

    fn creator_more(&mut self) {
        let Some(c) = self.creator.as_mut() else { return };
        if c.loading || c.end {
            return;
        }
        c.loading = true;
        c.err = None;
        let (user, page) = (c.id, c.page);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(Msg::UserPosts { user, page, result: api::fetch_user_posts(user, page) });
            ctx.request_repaint();
        });
    }

    /// Loads the first page of every followed creator and merges them, newest first.
    fn load_following(&mut self) {
        self.following_gen += 1;
        self.following_posts.clear();
        self.following_loading = self.settings.favourite_authors.len();
        for &id in self.settings.favourite_authors.keys() {
            let (tx, ctx, gen) = (self.tx.clone(), self.ctx.clone(), self.following_gen);
            thread::spawn(move || {
                let _ = tx.send(Msg::FollowingPosts { gen, result: api::fetch_user_posts(id, 0) });
                ctx.request_repaint();
            });
        }
    }

    // ---- likes and comments -------------------------------------------------------------------

    fn likes_of(&self, p: &Post) -> (u64, Option<bool>) {
        match self.likes.get(&p.lang_group) {
            Some(&(n, liked)) => (n, Some(liked)),
            None => (p.likes, None),
        }
    }

    fn like(&mut self, lang_group: u64) {
        if !self.logged_in() {
            self.toast("Log in to live.warthunder.com to like posts (sidebar, bottom left).", true);
            return;
        }
        if !self.like_busy.insert(lang_group) {
            return;
        }
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(Msg::Liked(lang_group, api::toggle_like(lang_group)));
            ctx.request_repaint();
        });
    }

    fn load_comments(&mut self, p: &Post) {
        let keep_draft = std::mem::take(&mut self.comments.draft);
        let same = self.comments.lang_group == p.lang_group;
        self.comments = CommentsState { lang_group: p.lang_group, loading: true, ..Default::default() };
        if same {
            self.comments.draft = keep_draft;
        }
        if p.id == 0 {
            // Feed entries sometimes lack the language id; the full post (Msg::Detail) brings it.
            return;
        }
        let (tx, ctx, id, lg) = (self.tx.clone(), self.ctx.clone(), p.id, p.lang_group);
        thread::spawn(move || {
            let _ = tx.send(Msg::Comments(lg, api::fetch_comments(id, lg)));
            ctx.request_repaint();
        });
    }

    fn send_comment(&mut self) {
        let Some(p) = self.detail.clone() else { return };
        let text = self.comments.draft.trim().to_string();
        if text.is_empty() || self.comments.sending || p.id == 0 {
            return;
        }
        if !self.logged_in() {
            self.toast("Log in to live.warthunder.com to comment.", true);
            return;
        }
        self.comments.sending = true;
        let reply = self.comments.reply_to;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(Msg::CommentPosted(p.lang_group, api::post_comment(p.id, p.lang_group, &text, reply)));
            ctx.request_repaint();
        });
    }

    fn hidden_by_filter(&self, p: &Post) -> bool {
        let author = p.author.as_ref().map_or(0, |a| a.id);
        // On a creator's own page their posts stay visible.
        let on_their_page = self.view == View::Creator(author);
        (!on_their_page && self.settings.hidden_authors.contains_key(&author)) || (self.settings.hide_anime && p.looks_anime())
    }

    fn toggle_hidden_author(&mut self, id: u64, nickname: String) {
        if id == 0 {
            return;
        }
        if self.settings.hidden_authors.remove(&id).is_some() {
            self.toast(format!("Posts by {nickname} are shown again."), false);
        } else {
            self.settings.hidden_authors.insert(id, nickname.clone());
            self.detail = None;
            self.toast(format!("Posts by {nickname} are hidden. Undo this in Settings."), false);
        }
        self.save_settings();
    }

    // ---- app updates --------------------------------------------------------------------------

    fn check_updates(&mut self, manual: bool) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready { .. }) {
            return;
        }
        self.update = UpdateState::Checking;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let result = updater::check(APP_VERSION);
            let _ = tx.send(Msg::UpdateCheck { manual, result });
            ctx.request_repaint();
        });
    }

    fn install_update(&mut self, release: updater::Release) {
        if matches!(self.update, UpdateState::Downloading { .. } | UpdateState::Ready { .. }) {
            return;
        }
        self.update = UpdateState::Downloading { release: release.clone(), done: 0, total: None };
        self.update_banner_hidden = false;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let result = updater::install(&release, |done, total| {
                let _ = tx.send(Msg::UpdateProgress { done, total });
                ctx.request_repaint();
            });
            let _ = tx.send(Msg::UpdateInstalled(result));
            ctx.request_repaint();
        });
    }

    /// Banner above the page while an app update is available, downloading or ready.
    fn update_banner(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) -> bool {
        let mut hide = false;
        let (text, color) = match &self.update {
            UpdateState::Available(r) => (format!("WT Armory Manager {} is available (you have {APP_VERSION}).", r.version), ACCENT),
            UpdateState::Downloading { release, done, total } => {
                let pct = total.filter(|t| *t > 0).map(|t| format!(" {:.0}%", *done as f64 * 100.0 / t as f64)).unwrap_or_default();
                (format!("Downloading WT Armory Manager {}...{pct}", release.version), ACCENT)
            }
            UpdateState::Ready { release, .. } => (format!("WT Armory Manager {} is installed. Restart the app to use it.", release.version), SUCCESS),
            _ => return false,
        };
        if self.update_banner_hidden {
            return false;
        }
        Frame::none()
            .fill(color.gamma_multiply(0.12))
            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.6)))
            .rounding(8.0)
            .inner_margin(Margin::symmetric(12.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    icons::show(ui, Icon::Update, 18.0, color);
                    ui.label(RichText::new(text).size(14.0).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, Some(Icon::Close), "", Color32::TRANSPARENT, MUTED, None, true).on_hover_text("Hide until next start").clicked() {
                            hide = true;
                        }
                        match &self.update {
                            UpdateState::Available(r) => {
                                if button(ui, Some(Icon::External), "What's new", BTN, TEXT, None, true).clicked() {
                                    actions.push(Action::OpenUrl(r.page.clone()));
                                }
                                if button(ui, Some(Icon::Download), "Install update", ACCENT, ON_ACCENT, None, true).clicked() {
                                    actions.push(Action::InstallUpdate(r.clone()));
                                }
                            }
                            UpdateState::Downloading { .. } => {
                                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                            }
                            UpdateState::Ready { release, exe } => {
                                if button(ui, Some(Icon::External), "What's new", BTN, TEXT, None, true).clicked() {
                                    actions.push(Action::OpenUrl(release.page.clone()));
                                }
                                let busy = self.jobs.values().any(|j| j.state != JobState::Queued);
                                let r = button(ui, Some(Icon::Refresh), "Restart now", SUCCESS, Color32::from_rgb(0x08, 0x20, 0x10), None, !busy);
                                if busy {
                                    r.on_hover_text("Wait until the running downloads are finished");
                                } else if r.clicked() {
                                    actions.push(Action::Restart(exe.clone()));
                                }
                            }
                            _ => {}
                        }
                    });
                });
            });
        ui.add_space(10.0);
        hide
    }

    // ---- bug reports --------------------------------------------------------------------------

    fn log_error(&mut self, text: &str) {
        let line = format!("[{}] {}", updates::fmt_date(now_ts()), redact(text));
        self.error_log.push_back(line);
        while self.error_log.len() > 15 {
            self.error_log.pop_front();
        }
    }

    /// The prefilled GitHub "new issue" page for the bug report form. Nothing is sent by the
    /// app itself: the user reviews the text on GitHub and submits it there with their account.
    fn bug_report_url(&self) -> String {
        let game = self.game_version.clone().unwrap_or_else(|| "unknown".into());
        let source = match self.settings.game_root.as_ref().map(|p| p.display().to_string().to_lowercase()) {
            Some(p) if p.contains("steamapps") => "Steam",
            Some(_) => "Gaijin launcher / other",
            None => "not set",
        };
        let mut body = format!(
            "### What happened?\n{}\n\n### Steps to reproduce\n1. \n\n### System\n- WT Armory Manager: {APP_VERSION}\n- Windows: {}\n- War Thunder: {game} ({source})\n- Installed items: {}\n",
            if self.bug_text.trim().is_empty() { "<!-- Describe the problem -->" } else { self.bug_text.trim() },
            windows_version(),
            self.manifest.items.len(),
        );
        if self.bug_include_errors && !self.error_log.is_empty() {
            body.push_str("\n### Recent errors\n```\n");
            for line in &self.error_log {
                body.push_str(&api::truncate(line, 300));
                body.push('\n');
            }
            body.push_str("```\n");
        }
        let title = if self.bug_title.trim().is_empty() { "Bug: " } else { self.bug_title.trim() };
        // Browsers and GitHub reject very long URLs; the body is what gets cut.
        let mut body_enc = url_encode(&body);
        while body_enc.len() > 6000 {
            let keep = body.chars().count().saturating_sub(200);
            body = body.chars().take(keep).collect();
            body_enc = url_encode(&format!("{body}\n...(cut)"));
        }
        format!("https://github.com/{}/issues/new?labels=bug&title={}&body={}", updater::REPO, url_encode(title), body_enc)
    }

    fn bug_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.bug_open {
            return;
        }
        let mut open = true;
        let mut submit = false;
        egui::Window::new(RichText::new("Report a bug").family(bold()).size(16.0).color(TEXT))
            .id(egui::Id::new("bug_report"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.label(muted("This opens a new issue on GitHub with your text and some system details filled in. You can check and change everything there before you submit it (you need a free GitHub account)."));
                ui.add_space(8.0);
                ui.label(RichText::new("Title").family(bold()).color(TEXT));
                ui.add(egui::TextEdit::singleline(&mut self.bug_title).hint_text("e.g. Sound mod does not turn on").desired_width(f32::INFINITY).margin(vec2(8.0, 6.0)));
                ui.add_space(6.0);
                ui.label(RichText::new("What happened?").family(bold()).color(TEXT));
                ui.add(
                    egui::TextEdit::multiline(&mut self.bug_text)
                        .hint_text("What did you do, what did you expect, and what happened instead?")
                        .desired_width(f32::INFINITY)
                        .desired_rows(6)
                        .margin(vec2(8.0, 6.0)),
                );
                ui.add_space(6.0);
                let n = self.error_log.len();
                ui.add_enabled(n > 0, egui::Checkbox::new(&mut self.bug_include_errors, RichText::new(format!("Include the last {n} error message(s) from this session")).color(TEXT)));
                ui.label(muted(format!(
                    "Always included: app version {APP_VERSION}, Windows version, War Thunder version and the number of installed items. Your user folder is replaced by %USERPROFILE%."
                )));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if button(ui, Some(Icon::External), "Continue on GitHub", ACCENT, ON_ACCENT, None, true).clicked() {
                        submit = true;
                    }
                    if button(ui, None, "See existing issues", BTN, TEXT, None, true).clicked() {
                        actions.push(Action::OpenUrl(format!("https://github.com/{}/issues", updater::REPO)));
                    }
                });
            });
        if submit {
            actions.push(Action::OpenUrl(self.bug_report_url()));
            self.bug_open = false;
            self.bug_title.clear();
            self.bug_text.clear();
            self.toast("The bug report opened in your browser. Submit it there.", false);
        } else if !open {
            self.bug_open = false;
        }
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
        if error {
            self.log_error(&text);
        }
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
        self.load_comments(&post);
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
                        job.state = JobState::Installing;
                    }
                }
                Msg::UpdateCheck { manual, result } => match result {
                    Ok(Some(release)) => {
                        // Debug builds never replace themselves (that would overwrite target\debug).
                        let auto = self.settings.auto_update && !cfg!(debug_assertions);
                        self.update = UpdateState::Available(release.clone());
                        if auto {
                            self.install_update(release);
                        }
                    }
                    Ok(None) => {
                        self.update = UpdateState::UpToDate;
                        if manual {
                            self.toast(format!("You have the latest version ({APP_VERSION})."), false);
                        }
                    }
                    Err(e) => {
                        if manual {
                            self.toast(format!("Update check failed: {e}"), true);
                        }
                        self.update = UpdateState::Failed(e);
                    }
                },
                Msg::UpdateProgress { done, total } => {
                    if let UpdateState::Downloading { done: d, total: t, .. } = &mut self.update {
                        *d = done;
                        *t = total;
                    }
                }
                Msg::UpdateInstalled(result) => {
                    let release = match std::mem::replace(&mut self.update, UpdateState::Idle) {
                        UpdateState::Downloading { release, .. } => Some(release),
                        _ => None,
                    };
                    match (result, release) {
                        (Ok(exe), Some(release)) => self.update = UpdateState::Ready { release, exe },
                        (Err(e), release) => {
                            self.toast(format!("Update failed: {e}"), true);
                            self.update = match release {
                                Some(r) => UpdateState::Available(r),
                                None => UpdateState::Failed(e),
                            };
                        }
                        (Ok(_), None) => {}
                    }
                }
                Msg::InstallDone { id, result } => {
                    let post = self.jobs.remove(&id).map(|j| j.post);
                    self.job_order.retain(|j| *j != id);
                    if let Some(post) = post {
                        let error = result.as_ref().err().cloned();
                        self.finished.push_front(Finished { post, error, at: now_ts() });
                        self.finished.truncate(HISTORY_LEN);
                    }
                    self.pump_queue();
                    match result {
                        Err(e) if e == api::CANCELLED => {}
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
                    let mut reload_comments = None;
                    if let (Ok(full), Some(current)) = (result, self.detail.as_mut()) {
                        if current.lang_group == full.lang_group {
                            if current.id == 0 && full.id != 0 {
                                reload_comments = Some(full.clone());
                            }
                            *current = full;
                        }
                    }
                    if let Some(p) = reload_comments {
                        self.load_comments(&p);
                    }
                }
                Msg::Profile(id, result) => {
                    if let Some(c) = self.creator.as_mut().filter(|c| c.id == id) {
                        match result {
                            Ok(p) => {
                                c.nickname = p.nickname.clone();
                                // keep the followed entry's name and picture current
                                if let Some(f) = self.settings.favourite_authors.get_mut(&id) {
                                    f.nickname = p.nickname.clone();
                                    f.avatar = p.avatar.clone();
                                }
                                c.profile = Some(p);
                            }
                            Err(e) => c.err = Some(e),
                        }
                    }
                }
                Msg::UserPosts { user, page, result } => {
                    if let Some(c) = self.creator.as_mut().filter(|c| c.id == user && c.page == page) {
                        c.loading = false;
                        match result {
                            Ok((posts, more)) => {
                                let seen: HashSet<u64> = c.posts.iter().map(|p| p.lang_group).collect();
                                c.posts.extend(posts.into_iter().filter(|p| !seen.contains(&p.lang_group)));
                                c.end = !more;
                                c.page += 1;
                            }
                            Err(e) => c.err = Some(e),
                        }
                    }
                }
                Msg::FollowingPosts { gen, result } if gen == self.following_gen => {
                    self.following_loading = self.following_loading.saturating_sub(1);
                    if let Ok((posts, _)) = result {
                        let seen: HashSet<u64> = self.following_posts.iter().map(|p| p.lang_group).collect();
                        self.following_posts.extend(posts.into_iter().filter(|p| !seen.contains(&p.lang_group)));
                        self.following_posts.sort_by(|a, b| b.created.cmp(&a.created));
                    }
                }
                Msg::FollowingPosts { .. } => {}
                Msg::Comments(lg, result) if lg == self.comments.lang_group => {
                    self.comments.loading = false;
                    match result {
                        Ok(list) => self.comments.list = list,
                        Err(e) => self.comments.err = Some(e),
                    }
                }
                Msg::Comments(..) => {}
                Msg::Liked(lg, result) => {
                    self.like_busy.remove(&lg);
                    match result {
                        Ok(state) => {
                            self.likes.insert(lg, state);
                        }
                        Err(e) => self.toast(format!("Could not like the post: {e}"), true),
                    }
                }
                Msg::CommentPosted(lg, result) => {
                    self.comments.sending = false;
                    match result {
                        Ok(()) => {
                            self.toast("Comment posted.", false);
                            if self.comments.lang_group == lg {
                                self.comments.draft.clear();
                                self.comments.reply_to = None;
                                if let Some(p) = self.detail.clone() {
                                    self.load_comments(&p);
                                }
                            }
                        }
                        Err(e) => self.toast(format!("Could not post the comment: {e}"), true),
                    }
                }
                Msg::Login(result) => {
                    self.login_busy = false;
                    match result {
                        Ok(Some(s)) => {
                            self.toast("Logged in to live.warthunder.com.", false);
                            self.use_session(s);
                        }
                        Ok(None) => {}
                        Err(e) => self.toast(e, true),
                    }
                }
                Msg::SessionChecked(result) => match result {
                    Ok((0, _)) => {
                        // The site no longer accepts the cookies.
                        login::logout();
                        api::set_session(None);
                        self.session = None;
                        self.toast("Your live.warthunder.com login has expired. Log in again to like and comment.", false);
                    }
                    Ok((uid, profile)) => {
                        if let Some(s) = self.session.as_mut() {
                            s.user_id = uid;
                            if let Some(p) = profile {
                                s.nickname = p.nickname;
                                s.avatar = p.avatar;
                            }
                            let _ = login::save(s);
                        }
                    }
                    Err(_) => {} // offline: keep the session and try again next start
                },
            }
        }
    }

    // ---- install / uninstall ------------------------------------------------------------------

    /// Adds a post to the download queue. At most `MAX_PARALLEL_DOWNLOADS` run at once, so a
    /// dozen clicks don't split the bandwidth a dozen ways.
    fn start_install(&mut self, post: Post) {
        let id = post.lang_group;
        if self.settings.game_root.is_none() || self.jobs.contains_key(&id) {
            return;
        }
        let job = Job { post, state: JobState::Queued, done: 0, total: None, started: 0.0, cancel: Arc::new(AtomicBool::new(false)) };
        self.jobs.insert(id, job);
        self.job_order.push(id);
        self.pump_queue();
    }

    fn running_jobs(&self) -> usize {
        self.jobs.values().filter(|j| j.state != JobState::Queued).count()
    }

    /// Starts queued downloads while there is a free slot.
    fn pump_queue(&mut self) {
        while self.running_jobs() < MAX_PARALLEL_DOWNLOADS {
            let Some(id) = self.job_order.iter().copied().find(|id| self.jobs.get(id).is_some_and(|j| j.state == JobState::Queued)) else {
                break;
            };
            self.run_job(id);
        }
    }

    fn cancel_job(&mut self, id: u64) {
        let Some(job) = self.jobs.get(&id) else { return };
        match job.state {
            JobState::Queued => {
                let job = self.jobs.remove(&id).expect("checked above");
                self.job_order.retain(|j| *j != id);
                self.finished.push_front(Finished { post: job.post, error: Some(api::CANCELLED.into()), at: now_ts() });
                self.finished.truncate(HISTORY_LEN);
            }
            // The worker notices the flag at its next progress report and answers with CANCELLED.
            JobState::Downloading => job.cancel.store(true, Ordering::Relaxed),
            JobState::Installing => {} // too late: files are being written
        }
    }

    fn run_job(&mut self, id: u64) {
        let Some(root) = self.settings.game_root.clone() else { return };
        let now = self.ctx.input(|i| i.time);
        let Some(job) = self.jobs.get_mut(&id) else { return };
        job.state = JobState::Downloading;
        job.started = now;
        let (post, cancel) = (job.post.clone(), job.cancel.clone());
        let target = installer::Target { root, user_id: self.settings.user_id.clone() };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let result = (|| -> Result<Installed, String> {
                let file = post.file.clone().ok_or("This post has no file to download")?;
                let cache = storage::cache_dir().join(id.to_string());
                let dest = cache.join(storage::sanitize(&file.name));
                let file_date = api::download(&file.link, &dest, |done, total| {
                    let _ = tx.send(Msg::Progress { id, done, total });
                    ctx.request_repaint();
                    !cancel.load(Ordering::Relaxed)
                });
                let file_date = match file_date {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&cache);
                        return Err(e);
                    }
                };
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
                        let active = self.jobs.len();
                        let r = button(ui, Some(Icon::Download), "Downloads", if active > 0 { ACCENT.gamma_multiply(0.25) } else { BTN }, TEXT, None, true);
                        if active > 0 {
                            badge(ui.painter(), r.rect.right_top() + vec2(4.0, -4.0), Align2::RIGHT_TOP, &active.to_string(), ACCENT, ON_ACCENT);
                        }
                        if r.on_hover_text("Running, waiting and finished downloads").clicked() {
                            self.downloads_open = !self.downloads_open;
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

    fn sidebar(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let mut new_view = None;
        let mut account = None;
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
                let following = self.settings.favourite_authors.len();
                let on_creators = matches!(self.view, View::Following | View::Creator(_));
                if nav_item(ui, Icon::Star(false), "Followed creators", Some(following), on_creators).clicked() {
                    new_view = Some(View::Following);
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(RichText::new(format!("Version {APP_VERSION}")).size(11.0).color(MUTED.gamma_multiply(0.8)));
                    ui.add_space(4.0);
                    if nav_item(ui, Icon::Bug, "Report a bug", None, self.bug_open).clicked() {
                        self.bug_open = !self.bug_open;
                    }
                    match &self.session {
                        Some(s) => {
                            let r = ui.add(egui::Label::new(RichText::new("Log out").size(12.0).color(MUTED)).sense(Sense::click()));
                            if r.on_hover_text("Forget the live.warthunder.com login on this PC").clicked() {
                                account = Some(Action::Logout);
                            }
                            let name = if s.nickname.is_empty() { "Logged in".to_string() } else { s.nickname.clone() };
                            let me = View::Creator(s.user_id);
                            if nav_item(ui, Icon::User, &name, None, self.view == me).on_hover_text("Your profile").clicked() {
                                account = Some(Action::OpenCreator(s.user_id, name));
                            }
                        }
                        None => {
                            let text = if self.login_busy { "Logging in..." } else { "Log in" };
                            if nav_item(ui, Icon::Login, text, None, self.login_busy)
                                .on_hover_text("Log in to live.warthunder.com to like posts and write comments")
                                .clicked()
                            {
                                account = Some(Action::Login);
                            }
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new("Content is made by the War Thunder community. Not affiliated with Gaijin Entertainment.").size(11.0).color(MUTED.gamma_multiply(0.8)));
                });
            });
        actions.extend(account);
        if let Some(view) = new_view {
            if view != self.view {
                self.view = view;
                match view {
                    View::Browse(_) => {
                        self.vehicle_type = "";
                        self.reload();
                    }
                    View::Following => self.load_following(),
                    _ => {}
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
            if chip(ui, "Hide anime", self.settings.hide_anime)
                .on_hover_text("Hide anime and itasha skins (detected from hashtags like #anime, #itasha, #girls_und_panzer and franchise names)")
                .clicked()
            {
                self.settings.hide_anime = !self.settings.hide_anime;
                self.save_settings();
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
            .filter(|p| !self.hidden_by_filter(p))
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
                        ui.label(muted(format!("{hidden} item(s) hidden by the filters")));
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
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add(egui::Label::new(muted("by")).selectable(false));
                author_link(ui, p, actions);
                ui.add(egui::Label::new(muted(format!("·  {when}"))).truncate().selectable(false));
            });
            ui.horizontal(|ui| {
                let (likes, liked) = self.likes_of(p);
                if liked == Some(true) {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    icons::show(ui, Icon::Heart, 13.0, DANGER).on_hover_text("You like this");
                    ui.label(RichText::new(compact(likes)).size(12.5).color(DANGER));
                } else {
                    stat(ui, Icon::Heart, likes, "likes");
                }
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
            ui.allocate_ui_with_layout(vec2(width, 30.0), Layout::right_to_left(Align::Center), |ui| {
                let can_cancel = job.state != JobState::Installing;
                if can_cancel && button(ui, Some(Icon::Close), "", BTN, TEXT, Some(32.0), true).on_hover_text("Cancel").clicked() {
                    actions.push(Action::Cancel(id));
                }
                let w = ui.available_width();
                self.job_progress(ui, job, w, 30.0);
            });
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

    fn queue_position(&self, id: u64) -> usize {
        self.job_order.iter().filter(|j| self.jobs.get(j).is_some_and(|j| j.state == JobState::Queued)).position(|j| *j == id).map_or(0, |p| p + 1)
    }

    fn job_progress(&self, ui: &mut egui::Ui, job: &Job, width: f32, height: f32) {
        let frac = job.total.filter(|t| *t > 0).map(|t| job.done as f32 / t as f32);
        let now = ui.input(|i| i.time);
        let text = match job.state {
            JobState::Queued => format!("Waiting in queue  #{}", self.queue_position(job.post.lang_group)),
            JobState::Installing => "Installing...".to_string(),
            JobState::Downloading => {
                let secs = (now - job.started).max(0.1);
                let speed = if secs > 1.0 { format!("  ·  {}/s", fmt_size((job.done as f64 / secs) as u64)) } else { String::new() };
                match frac {
                    Some(f) => format!("{:.0}%{speed}", f * 100.0),
                    None => format!("{}{speed}", fmt_size(job.done)),
                }
            }
        };
        let (value, animate, fill) = match job.state {
            JobState::Queued => (0.0, false, BTN),
            JobState::Installing => (1.0, true, ACCENT.gamma_multiply(0.7)),
            JobState::Downloading => (frac.unwrap_or(0.0), frac.is_none(), ACCENT.gamma_multiply(0.7)),
        };
        ui.add(
            egui::ProgressBar::new(value)
                .desired_width(width)
                .desired_height(height)
                .fill(fill)
                .rounding(6.0)
                .animate(animate)
                .text(RichText::new(text).color(TEXT).size(13.0)),
        );
    }

    fn downloads_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.downloads_open {
            return;
        }
        let mut open = true;
        let screen = ctx.screen_rect();
        egui::Window::new(RichText::new("Downloads").family(bold()).size(16.0).color(TEXT))
            .id(egui::Id::new("downloads"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .default_height(520.0_f32.min(screen.height() - 120.0))
            .max_height((screen.height() - 100.0).max(250.0))
            .anchor(Align2::RIGHT_TOP, vec2(-18.0, 64.0))
            .show(ctx, |ui| {
                let running: Vec<&Job> = self.job_order.iter().filter_map(|id| self.jobs.get(id)).filter(|j| j.state != JobState::Queued).collect();
                let queued: Vec<&Job> = self.job_order.iter().filter_map(|id| self.jobs.get(id)).filter(|j| j.state == JobState::Queued).collect();
                ui.label(muted(format!("Up to {MAX_PARALLEL_DOWNLOADS} downloads run at the same time; the rest wait their turn.")));
                ui.add_space(6.0);
                egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                    section_label(ui, &format!("ACTIVE  ({})", running.len()));
                    if running.is_empty() {
                        ui.label(muted("Nothing is downloading."));
                    }
                    for job in &running {
                        self.download_row(ui, &job.post, actions, |app, ui, w, actions| {
                            let can_cancel = job.state != JobState::Installing;
                            ui.horizontal(|ui| {
                                app.job_progress(ui, job, w - if can_cancel { 40.0 } else { 0.0 }, 24.0);
                                if can_cancel && button(ui, Some(Icon::Close), "", BTN, TEXT, Some(32.0), true).on_hover_text("Cancel").clicked() {
                                    actions.push(Action::Cancel(job.post.lang_group));
                                }
                            });
                        });
                    }
                    if !queued.is_empty() {
                        ui.add_space(8.0);
                        section_label(ui, &format!("WAITING  ({})", queued.len()));
                        for job in &queued {
                            self.download_row(ui, &job.post, actions, |app, ui, w, actions| {
                                ui.horizontal(|ui| {
                                    app.job_progress(ui, job, w - 40.0, 24.0);
                                    if button(ui, Some(Icon::Close), "", BTN, TEXT, Some(32.0), true).on_hover_text("Remove from the queue").clicked() {
                                        actions.push(Action::Cancel(job.post.lang_group));
                                    }
                                });
                            });
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        section_label(ui, &format!("FINISHED  ({})", self.finished.len()));
                        if !self.finished.is_empty() {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(egui::Label::new(RichText::new("Clear").size(12.5).color(ACCENT)).sense(Sense::click())).clicked() {
                                    actions.push(Action::ClearFinished);
                                }
                            });
                        }
                    });
                    if self.finished.is_empty() {
                        ui.label(muted("Finished downloads show up here."));
                    }
                    for f in &self.finished {
                        self.download_row(ui, &f.post, actions, |app, ui, _w, actions| {
                            ui.horizontal(|ui| {
                                match &f.error {
                                    None => {
                                        icons::show(ui, Icon::Check, 14.0, SUCCESS);
                                        ui.label(RichText::new("Installed").size(13.0).color(SUCCESS));
                                    }
                                    Some(e) if e == api::CANCELLED => {
                                        ui.label(RichText::new("Cancelled").size(13.0).color(MUTED));
                                    }
                                    Some(e) => {
                                        icons::show(ui, Icon::Close, 14.0, DANGER);
                                        ui.add(egui::Label::new(RichText::new(e).size(12.5).color(DANGER)).truncate()).on_hover_text(e);
                                    }
                                }
                                ui.label(muted(format!("·  {}", ago(f.at))));
                                let installed = app.manifest.get(f.post.lang_group).is_some();
                                if f.error.is_some() && !installed && !app.jobs.contains_key(&f.post.lang_group) {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if button(ui, Some(Icon::Refresh), "Retry", BTN, TEXT, None, true).clicked() {
                                            actions.push(Action::Install(f.post.clone()));
                                        }
                                    });
                                }
                            });
                        });
                    }
                });
            });
        if !open {
            self.downloads_open = false;
        }
    }

    /// One row of the Downloads window: thumbnail, title and a status line drawn by `status`.
    fn download_row(&self, ui: &mut egui::Ui, p: &Post, actions: &mut Vec<Action>, status: impl FnOnce(&Self, &mut egui::Ui, f32, &mut Vec<Action>)) {
        ui.horizontal(|ui| {
            let (r, resp) = ui.allocate_exact_size(vec2(84.0, 47.0), Sense::click());
            let kind = p.kind().unwrap_or(Kind::Camouflage);
            paint_thumb(ui, p, kind, r, Rounding::same(4.0));
            if resp.on_hover_text("Show details").clicked() {
                actions.push(Action::Detail(p.clone()));
            }
            ui.vertical(|ui| {
                let w = ui.available_width();
                ui.horizontal(|ui| {
                    icons::show(ui, Icon::Kind(kind), 13.0, ACCENT);
                    ui.add(egui::Label::new(RichText::new(p.title()).family(bold()).size(14.0).color(TEXT)).truncate());
                });
                status(self, ui, w, actions);
            });
        });
        ui.add_space(6.0);
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
            let text = RichText::new(format!("{} download(s) running or waiting. Open Downloads", self.jobs.len())).size(12.5).color(ACCENT);
            if ui.add(egui::Label::new(text).sense(Sense::click())).clicked() {
                self.downloads_open = true;
            }
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
                        let author = p.author.clone().unwrap_or_default();
                        let pic = author.avatar_url();
                        if avatar(ui, pic.as_deref(), 28.0).clicked() && author.id != 0 {
                            actions.push(Action::OpenCreator(author.id, author.nickname.clone()));
                        }
                        author_link(ui, &p, actions);
                        if author.id != 0 {
                            let following = self.is_following(author.id);
                            let r = button(ui, Some(Icon::Star(following)), if following { "Following" } else { "Follow" }, BTN, if following { ACCENT } else { TEXT }, None, true);
                            if r.on_hover_text(if following { "Stop following this creator" } else { "Follow this creator (kept in the app)" }).clicked() {
                                actions.push(Action::ToggleFollow(author.id, author.nickname.clone(), pic.clone()));
                            }
                        }
                        ui.label(muted(ago(p.created)));
                        ui.add_space(10.0);
                        let (likes, liked) = self.likes_of(&p);
                        let liked = liked == Some(true);
                        let busy = self.like_busy.contains(&p.lang_group);
                        let r = button(
                            ui,
                            Some(Icon::Heart),
                            &compact(likes),
                            if liked { DANGER_BG } else { BTN },
                            if liked { DANGER } else { TEXT },
                            None,
                            !busy,
                        );
                        let tip = match (self.logged_in(), liked) {
                            (false, _) => "Log in (bottom left) to like posts",
                            (true, true) => "You like this. Click to take the like back",
                            (true, false) => "Like this post",
                        };
                        if r.on_hover_text(tip).clicked() {
                            actions.push(Action::Like(p.lang_group));
                        }
                        if author.id != 0 && !self.is_following(author.id) {
                            let r = ui.add(egui::Label::new(RichText::new("Hide creator").size(12.5).color(MUTED)).sense(Sense::click()));
                            if r.on_hover_text("Never show posts by this creator in the feeds (e.g. anime skins the filter misses)").clicked() {
                                actions.push(Action::ToggleHidden(author.id, author.nickname.clone()));
                            }
                        }
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
                    ui.add_space(12.0);
                    ui.separator();
                    self.comments_section(ui, &p, actions);
                });
            });
        if !open {
            self.detail = None;
        }
    }

    fn comments_section(&mut self, ui: &mut egui::Ui, p: &Post, actions: &mut Vec<Action>) {
        let c = &self.comments;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            icons::show(ui, Icon::Comment, 16.0, ACCENT);
            let n = if c.loading { p.comments as usize } else { c.list.len() };
            ui.label(RichText::new(format!("Comments ({n})")).family(bold()).size(15.0).color(TEXT));
            if c.loading {
                ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
            }
        });
        ui.add_space(4.0);

        // Write box (or the way to log in).
        if self.logged_in() {
            if let Some(rid) = self.comments.reply_to {
                let who = self.comments.list.iter().find(|x| x.id == rid).and_then(|x| x.author.as_ref()).map(|a| a.nickname.clone()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(muted(format!("Replying to {who}")));
                    if ui.add(egui::Label::new(RichText::new("Cancel").size(12.5).color(ACCENT)).sense(Sense::click())).clicked() {
                        actions.push(Action::ReplyTo(None));
                    }
                });
            }
            ui.add(
                egui::TextEdit::multiline(&mut self.comments.draft)
                    .hint_text("Write a comment...")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .margin(vec2(8.0, 6.0)),
            );
            ui.horizontal(|ui| {
                let can = !self.comments.draft.trim().is_empty() && !self.comments.sending && p.id != 0;
                let label = if self.comments.sending { "Sending..." } else { "Post comment" };
                if button(ui, Some(Icon::Comment), label, ACCENT, ON_ACCENT, None, can).clicked() {
                    actions.push(Action::SendComment);
                }
                ui.label(muted("Posted on live.warthunder.com under your account."));
            });
        } else {
            ui.horizontal(|ui| {
                if button(ui, Some(Icon::Login), "Log in to comment", BTN, TEXT, None, !self.login_busy).clicked() {
                    actions.push(Action::Login);
                }
            });
        }
        ui.add_space(8.0);

        let c = &self.comments;
        if let Some(e) = &c.err {
            ui.label(RichText::new(e).color(DANGER));
        } else if !c.loading && c.list.is_empty() {
            ui.label(muted("No comments yet."));
        }
        // Top-level comments, oldest first, each followed by its replies.
        let ids: HashSet<u64> = c.list.iter().map(|x| x.id).collect();
        let mut top: Vec<&api::Comment> = c.list.iter().filter(|x| x.parent == 0 || !ids.contains(&x.parent)).collect();
        top.sort_by_key(|x| x.created);
        let logged_in = self.logged_in();
        for t in top {
            comment_row(ui, t, 0.0, logged_in, actions);
            let mut replies: Vec<&api::Comment> = c.list.iter().filter(|x| x.parent == t.id).collect();
            replies.sort_by_key(|x| x.created);
            for r in replies {
                comment_row(ui, r, 34.0, logged_in, actions);
            }
        }
    }

    fn creator_view(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(c) = &self.creator else { return };
        let profile = c.profile.clone();
        let (id, nickname) = (c.id, c.nickname.clone());
        let pic = profile.as_ref().and_then(|p| p.avatar.clone());
        ui.horizontal(|ui| {
            if button(ui, None, "Back", BTN, TEXT, None, true).on_hover_text("Followed creators").clicked() {
                actions.push(Action::OpenFollowing);
            }
            ui.add_space(6.0);
            avatar(ui, pic.as_deref(), 52.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(&nickname).family(bold()).size(23.0).color(TEXT));
                match &profile {
                    Some(p) => {
                        ui.label(muted(format!(
                            "{} posts  ·  {} comments  ·  {} subscribers  ·  reputation {}",
                            compact(p.posts),
                            compact(p.comments),
                            compact(p.subscribers),
                            compact(p.reputation)
                        )));
                    }
                    None => {
                        ui.label(muted("Loading profile..."));
                    }
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if button(ui, Some(Icon::External), "Open profile on website", BTN, TEXT, None, true).clicked() {
                    actions.push(Action::OpenUrl(api::Profile::url(id)));
                }
                let following = self.is_following(id);
                if button(ui, Some(Icon::Star(following)), if following { "Following" } else { "Follow" }, if following { BTN } else { ACCENT }, if following { ACCENT } else { ON_ACCENT }, None, true)
                    .on_hover_text(if following { "Stop following" } else { "Show this creator's newest posts under Followed creators" })
                    .clicked()
                {
                    actions.push(Action::ToggleFollow(id, nickname.clone(), pic.clone()));
                }
            });
        });
        ui.add_space(12.0);
        let c = self.creator.as_ref().expect("checked above");
        let posts: Vec<Post> = c.posts.iter().filter(|p| !self.hidden_by_filter(p)).cloned().collect();
        let hidden = c.posts.len() - posts.len();
        let (loading, end, err) = (c.loading, c.end, c.err.clone());
        let more = self.post_grid(ui, ("creator", id), &posts, actions, |ui| {
            if hidden > 0 {
                ui.label(muted(format!("{hidden} item(s) hidden by the anime filter")));
            }
            if let Some(e) = &err {
                ui.label(RichText::new(e).color(DANGER));
            }
            if loading {
                ui.add(egui::Spinner::new().size(28.0).color(ACCENT));
                false
            } else if end || err.is_some() {
                ui.label(muted(if posts.is_empty() && hidden == 0 { "This creator has no installable posts." } else { "That's everything." }));
                false
            } else {
                let r = button(ui, None, "Load more", BTN, TEXT, None, true);
                r.clicked() || ui.is_rect_visible(r.rect)
            }
        });
        if more {
            actions.push(Action::CreatorMore);
        }
    }

    fn following_view(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            icons::show(ui, Icon::Star(true), 24.0, ACCENT);
            ui.label(RichText::new("Followed creators").family(bold()).size(23.0).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if button(ui, Some(Icon::Refresh), "", BTN, TEXT, None, true).on_hover_text("Refresh").clicked() {
                    actions.push(Action::OpenFollowing);
                }
            });
        });
        ui.label(muted("Creators you follow and their newest posts. Click a creator's name anywhere in the app to see their profile and follow them."));
        ui.add_space(10.0);
        if self.settings.favourite_authors.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("You don't follow anyone yet").family(bold()).size(17.0).color(TEXT));
                ui.label(muted("Open a post, then press Follow next to the creator's name."));
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for (&id, fav) in &self.settings.favourite_authors {
                let r = Frame::none()
                    .fill(CARD)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .rounding(20.0)
                    .inner_margin(Margin { left: 4.0, right: 12.0, top: 4.0, bottom: 4.0 })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            avatar(ui, fav.avatar.as_deref(), 28.0);
                            ui.label(RichText::new(&fav.nickname).size(13.5).color(TEXT));
                        });
                    })
                    .response
                    .interact(Sense::click());
                if r.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if r.on_hover_text("Open profile").clicked() {
                    actions.push(Action::OpenCreator(id, fav.nickname.clone()));
                }
            }
        });
        ui.add_space(12.0);
        section_label(ui, "NEWEST POSTS");
        let posts: Vec<Post> = self.following_posts.iter().filter(|p| !self.hidden_by_filter(p)).cloned().collect();
        let loading = self.following_loading > 0;
        let gen = self.following_gen;
        self.post_grid(ui, ("following", gen), &posts, actions, |ui| {
            if loading {
                ui.add(egui::Spinner::new().size(28.0).color(ACCENT));
            } else if posts.is_empty() {
                ui.label(muted("No installable posts from these creators."));
            }
            false
        });
    }

    /// A scrolling grid of post cards. `footer` draws below the cards and returns true to ask
    /// for more posts (infinite scroll).
    fn post_grid(&self, ui: &mut egui::Ui, id: impl std::hash::Hash, posts: &[Post], actions: &mut Vec<Action>, footer: impl FnOnce(&mut egui::Ui) -> bool) -> bool {
        let mut more = false;
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
            let gap = 14.0;
            let avail = ui.available_width() - 6.0;
            let cols = (((avail + gap) / (250.0 + gap)).floor() as usize).max(1);
            let card_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
            for row in posts.chunks(cols) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for post in row {
                        self.card(ui, post, card_w, actions);
                    }
                });
                ui.add_space(gap);
            }
            ui.vertical_centered(|ui| more = footer(ui));
            ui.add_space(24.0);
        });
        more
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut pick = false;
        let mut check = false;
        let mut install = None;
        egui::Window::new(RichText::new("Settings").family(bold()).size(16.0).color(TEXT))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(540.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("App updates").family(bold()).color(TEXT));
                ui.horizontal(|ui| {
                    ui.label(muted(format!("You have version {APP_VERSION}.")));
                    let status = match &self.update {
                        UpdateState::Idle => String::new(),
                        UpdateState::Checking => "Checking...".into(),
                        UpdateState::UpToDate => "This is the latest version.".into(),
                        UpdateState::Available(r) => format!("Version {} is available.", r.version),
                        UpdateState::Downloading { release, .. } => format!("Downloading {}...", release.version),
                        UpdateState::Ready { release, .. } => format!("{} is installed; restart to use it.", release.version),
                        UpdateState::Failed(e) => format!("Last check failed: {}", api::truncate(e, 70)),
                    };
                    let color = if matches!(self.update, UpdateState::Failed(_)) { DANGER } else { TEXT };
                    ui.label(RichText::new(status).size(12.5).color(color));
                });
                ui.horizontal(|ui| {
                    let busy = matches!(self.update, UpdateState::Checking | UpdateState::Downloading { .. });
                    if button(ui, Some(Icon::Refresh), "Check for updates", BTN, TEXT, None, !busy).clicked() {
                        check = true;
                    }
                    if let UpdateState::Available(r) = &self.update {
                        if button(ui, Some(Icon::Download), "Install update", ACCENT, ON_ACCENT, None, true).clicked() {
                            install = Some(r.clone());
                        }
                    }
                });
                if ui
                    .checkbox(&mut self.settings.auto_update, RichText::new("Install updates automatically (checked at every start)").color(TEXT))
                    .changed()
                {
                    self.save_settings();
                }
                ui.add_space(14.0);
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
                ui.label(RichText::new("Content filter").family(bold()).color(TEXT));
                if ui
                    .checkbox(&mut self.settings.hide_anime, RichText::new("Hide anime and itasha skins").color(TEXT))
                    .on_hover_text("Detected from hashtags and franchise names in the description, so posts without them can slip through")
                    .changed()
                {
                    self.save_settings();
                }
                if self.settings.hidden_authors.is_empty() {
                    ui.label(muted("Hidden creators: none. Use \"Hide creator\" in a post to add one."));
                } else {
                    ui.label(muted("Hidden creators (click to show their posts again):"));
                    let mut unhide = None;
                    ui.horizontal_wrapped(|ui| {
                        for (&id, name) in &self.settings.hidden_authors {
                            if chip(ui, name, false).on_hover_text("Show this creator again").clicked() {
                                unhide = Some((id, name.clone()));
                            }
                        }
                    });
                    if let Some((id, name)) = unhide {
                        self.settings.hidden_authors.remove(&id);
                        self.save_settings();
                        self.toast(format!("Posts by {name} are shown again."), false);
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
        if check {
            self.check_updates(true);
        }
        if let Some(r) = install {
            self.install_update(r);
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
        self.sidebar(ctx, &mut actions);
        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG).inner_margin(Margin { left: 22.0, right: 16.0, top: 16.0, bottom: 0.0 }))
            .show(ctx, |ui| {
                if self.update_banner(ui, &mut actions) {
                    self.update_banner_hidden = true;
                }
                match self.view {
                    View::Browse(kind) => self.browse(ui, kind, &mut actions),
                    View::Installed => self.installed_view(ui, &mut actions),
                    View::Following => self.following_view(ui, &mut actions),
                    View::Creator(_) => self.creator_view(ui, &mut actions),
                }
            });
        self.detail_window(ctx, &mut actions);
        self.settings_window(ctx);
        self.downloads_window(ctx, &mut actions);
        self.bug_window(ctx, &mut actions);
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
                Action::OpenCreator(id, name) => self.open_creator(id, name),
                Action::ToggleFollow(id, name, avatar) => self.toggle_follow(id, name, avatar),
                Action::Like(lg) => self.like(lg),
                Action::SendComment => self.send_comment(),
                Action::ReplyTo(r) => self.comments.reply_to = r,
                Action::Login => self.start_login(),
                Action::Logout => self.logout(),
                Action::CreatorMore => self.creator_more(),
                Action::ToggleHidden(id, name) => self.toggle_hidden_author(id, name),
                Action::OpenFollowing => {
                    self.view = View::Following;
                    self.load_following();
                }
                Action::Cancel(id) => self.cancel_job(id),
                Action::ClearFinished => self.finished.clear(),
                Action::InstallUpdate(r) => self.install_update(r),
                Action::Restart(exe) => match updater::restart(&exe) {
                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(e) => self.toast(e, true),
                },
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

fn comment_row(ui: &mut egui::Ui, c: &api::Comment, indent: f32, logged_in: bool, actions: &mut Vec<Action>) {
    ui.horizontal_top(|ui| {
        ui.add_space(indent);
        let a = c.author.clone().unwrap_or_default();
        if avatar(ui, a.avatar_url().as_deref(), 28.0).clicked() && a.id != 0 {
            actions.push(Action::OpenCreator(a.id, a.nickname.clone()));
        }
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let name = if a.nickname.is_empty() { "unknown" } else { a.nickname.as_str() };
                let r = ui.add(egui::Label::new(RichText::new(name).family(bold()).size(13.0).color(TEXT)).sense(Sense::click()));
                if r.clicked() && a.id != 0 {
                    actions.push(Action::OpenCreator(a.id, a.nickname.clone()));
                }
                ui.label(muted(ago(c.created)));
                if c.rating != 0 {
                    ui.label(muted(format!("{:+}", c.rating)));
                }
                if logged_in && ui.add(egui::Label::new(RichText::new("Reply").size(12.5).color(ACCENT)).sense(Sense::click())).clicked() {
                    actions.push(Action::ReplyTo(Some(if c.parent == 0 { c.id } else { c.parent })));
                }
            });
            ui.add(egui::Label::new(RichText::new(api::html_to_text(&c.text).trim()).size(13.5).color(TEXT)).wrap());
        });
    });
    ui.add_space(6.0);
}

/// The post's author as a link to their creator page.
fn author_link(ui: &mut egui::Ui, p: &Post, actions: &mut Vec<Action>) {
    let Some(a) = p.author.as_ref().filter(|a| a.id != 0) else {
        ui.add(egui::Label::new(muted(p.author_name())).selectable(false));
        return;
    };
    let r = ui.add(egui::Label::new(RichText::new(p.author_name()).size(12.5).color(TEXT.gamma_multiply(0.9))).sense(Sense::click()).truncate().selectable(false));
    let r = if r.hovered() {
        ui.painter().hline(r.rect.x_range(), r.rect.bottom(), Stroke::new(1.0_f32, ACCENT));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        r.on_hover_text("Show this creator's profile and other mods")
    } else {
        r
    };
    if r.clicked() {
        actions.push(Action::OpenCreator(a.id, a.nickname.clone()));
    }
}

/// A round avatar picture (or a drawn placeholder).
fn avatar(ui: &mut egui::Ui, url: Option<&str>, size: f32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    ui.painter().circle_filled(r.center(), size / 2.0, BTN);
    match url {
        Some(u) => {
            egui::Image::new(u).rounding(size / 2.0).paint_at(ui, r);
        }
        None => icons::paint(ui.painter(), r.shrink(size * 0.22), Icon::User, MUTED),
    }
    resp
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

fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Percent-encodes everything except unreserved characters (RFC 3986), for URL query values.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Replaces the user's home folder (it contains their Windows user name) in text meant for others.
fn redact(text: &str) -> String {
    let Some(home) = dirs::home_dir() else { return text.to_string() };
    let home = home.display().to_string();
    if home.len() < 4 {
        return text.to_string();
    }
    // Case-insensitive, like Windows paths.
    let lower = text.to_lowercase();
    let needle = home.to_lowercase();
    let mut out = String::new();
    let mut last = 0;
    for (i, _) in lower.match_indices(&needle) {
        if text.is_char_boundary(i) && text.is_char_boundary(i + needle.len()) {
            out.push_str(&text[last..i]);
            out.push_str("%USERPROFILE%");
            last = i + needle.len();
        }
    }
    out.push_str(&text[last..]);
    out
}

/// "Windows 11 (build 26200)" from the registry, read once.
fn windows_version() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let query = |name: &str| -> Option<String> {
                let mut cmd = std::process::Command::new("reg");
                cmd.args(["query", r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "/v", name]);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                }
                let out = cmd.output().ok()?;
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                text.lines().find(|l| l.contains(name))?.split("REG_SZ").nth(1).map(|s| s.trim().to_string())
            };
            let build: u32 = query("CurrentBuildNumber").and_then(|b| b.parse().ok()).unwrap_or(0);
            let display = query("DisplayVersion").unwrap_or_default();
            // The registry still says "Windows 10" on Windows 11; the build number tells them apart.
            let name = if build >= 22000 { "Windows 11" } else if build > 0 { "Windows 10" } else { "Windows" };
            format!("{name} {display} (build {build})").replace("  ", " ")
        })
        .clone()
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
    if std::env::args().any(|a| a == login::ARG) {
        login::window_main();
    }
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
