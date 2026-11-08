//! Client for the public live.warthunder.com JSON API.
//!
//! The site loads its feed with form-encoded POSTs (`/api/feed/get_regular/`) and single posts with
//! `/api/posts/get/`. No login, cookies or CSRF token are needed for reading and downloading.

use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

pub const BASE: &str = "https://live.warthunder.com";
pub const USER_AGENT: &str = concat!("WT-Armory-Manager/", env!("CARGO_PKG_VERSION"), " (desktop client)");
/// Error text of a download the user cancelled.
pub const CANCELLED: &str = "Cancelled";

/// The installable content types of live.warthunder.com. Hangars are published as "location" posts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Kind {
    Camouflage,
    Sound,
    Sight,
    Mission,
    Model,
    Location,
    Controls,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Camouflage,
        Kind::Sound,
        Kind::Sight,
        Kind::Mission,
        Kind::Model,
        Kind::Location,
        Kind::Controls,
    ];

    /// Value of the `content` form field and of a post's `type` field.
    pub fn code(self) -> &'static str {
        match self {
            Kind::Camouflage => "camouflage",
            Kind::Sound => "sound",
            Kind::Sight => "sight",
            Kind::Mission => "mission",
            Kind::Model => "model",
            Kind::Location => "location",
            Kind::Controls => "controls",
        }
    }

    pub fn from_code(code: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.code() == code)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Camouflage => "Camouflages",
            Kind::Sound => "Sound mods",
            Kind::Sight => "Sights",
            Kind::Mission => "Missions",
            Kind::Model => "Custom models",
            Kind::Location => "Hangars & locations",
            Kind::Controls => "Control presets",
        }
    }

    pub fn singular(self) -> &'static str {
        match self {
            Kind::Camouflage => "Camouflage",
            Kind::Sound => "Sound mod",
            Kind::Sight => "Sight",
            Kind::Mission => "Mission",
            Kind::Model => "Model",
            Kind::Location => "Hangar / location",
            Kind::Controls => "Control preset",
        }
    }

    /// Path on the website for "open in browser".
    pub fn site_path(self) -> &'static str {
        match self {
            Kind::Camouflage => "/feed/camouflages/",
            Kind::Sound => "/feed/sounds/",
            Kind::Sight => "/feed/sights/",
            Kind::Mission => "/feed/missions/",
            Kind::Model => "/feed/models/",
            Kind::Location => "/feed/locations/",
            Kind::Controls => "/feed/controlss/",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    PopularWeek,
    PopularMonth,
    TopAllTime,
    Newest,
    MostDownloaded,
}

impl Sort {
    pub const ALL: [Sort; 5] = [
        Sort::PopularWeek,
        Sort::PopularMonth,
        Sort::TopAllTime,
        Sort::Newest,
        Sort::MostDownloaded,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Sort::PopularWeek => "Popular this week",
            Sort::PopularMonth => "Popular this month",
            Sort::TopAllTime => "Top all time",
            Sort::Newest => "Newest",
            Sort::MostDownloaded => "Most downloaded",
        }
    }

    /// (`sort`, `period` in days; 0 = all time)
    fn params(self) -> (&'static str, &'static str) {
        match self {
            Sort::PopularWeek => ("rating", "7"),
            Sort::PopularMonth => ("rating", "30"),
            Sort::TopAllTime => ("rating", "0"),
            Sort::Newest => ("date", "0"),
            Sort::MostDownloaded => ("downloads", "0"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Data model. Every field is lenient: the API mixes numbers, strings and nulls between post types,
// and one odd post must not break a whole page.

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct Author {
    #[serde(default, deserialize_with = "de_u64")]
    pub id: u64,
    #[serde(default, deserialize_with = "de_string")]
    pub nickname: String,
    /// Avatar URL (may be relative or a "no avatar" placeholder; see `avatar_url`).
    #[serde(default, deserialize_with = "de_string")]
    pub avatar: String,
}

impl Author {
    pub fn avatar_url(&self) -> Option<String> {
        let a = self.avatar.trim();
        if a.is_empty() || a.contains("no_avatar") {
            return None;
        }
        Some(if a.starts_with("//") { format!("https:{a}") } else if a.starts_with('/') { format!("{BASE}{a}") } else { a.to_string() })
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct ImgRef {
    #[serde(default, deserialize_with = "de_string")]
    pub src: String,
    #[serde(default, deserialize_with = "de_f32")]
    pub width: f32,
    #[serde(default, deserialize_with = "de_f32")]
    pub height: f32,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct PostImage {
    #[serde(default, deserialize_with = "de_string")]
    pub src: String,
    #[serde(default, deserialize_with = "de_f32")]
    pub width: f32,
    #[serde(default, deserialize_with = "de_f32")]
    pub height: f32,
    #[serde(default, deserialize_with = "de_opt")]
    pub mq: Option<ImgRef>,
    #[serde(default, deserialize_with = "de_opt")]
    pub orig: Option<ImgRef>,
}

impl PostImage {
    pub fn thumb(&self) -> Option<&str> {
        [Some(self.src.as_str()), self.mq.as_ref().map(|m| m.src.as_str())]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
    }

    pub fn large(&self) -> Option<&str> {
        [self.mq.as_ref().map(|m| m.src.as_str()), Some(self.src.as_str())]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
    }

    /// Width / height, falling back to 16:9.
    pub fn ratio(&self) -> f32 {
        let (w, h) = match &self.mq {
            Some(m) if m.width > 0.0 && m.height > 0.0 => (m.width, m.height),
            _ => (self.width, self.height),
        };
        if w > 0.0 && h > 0.0 {
            w / h
        } else {
            16.0 / 9.0
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct PostFile {
    #[serde(default, deserialize_with = "de_string")]
    pub name: String,
    #[serde(default, deserialize_with = "de_string")]
    pub link: String,
    #[serde(default, deserialize_with = "de_u64")]
    pub size: u64,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct Post {
    /// Identifies the post across languages; used in /post/<lang_group>/ URLs and /api/posts/get/.
    #[serde(default, deserialize_with = "de_u64")]
    pub lang_group: u64,
    /// The id of this language version (comments use it as `postId`).
    #[serde(default, deserialize_with = "de_u64")]
    pub id: u64,
    #[serde(rename = "type", default, deserialize_with = "de_string")]
    pub kind: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub created: i64,
    #[serde(default, deserialize_with = "de_opt")]
    pub author: Option<Author>,
    #[serde(default, deserialize_with = "de_u64")]
    pub likes: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub views: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub downloads: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub comments: u64,
    /// HTML.
    #[serde(default, deserialize_with = "de_string")]
    pub description: String,
    #[serde(default, deserialize_with = "de_vec")]
    pub images: Vec<PostImage>,
    #[serde(default, deserialize_with = "de_opt")]
    pub file: Option<PostFile>,
}

impl Post {
    pub fn kind(&self) -> Option<Kind> {
        Kind::from_code(&self.kind)
    }

    pub fn author_name(&self) -> &str {
        self.author
            .as_ref()
            .map(|a| a.nickname.as_str())
            .filter(|n| !n.is_empty())
            .unwrap_or("unknown")
    }

    /// Posts have no title field: use the first line of the description, else the file name.
    pub fn title(&self) -> String {
        let text = html_to_text(&self.description);
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.chars().all(|c| c == '.' || c.is_whitespace()));
        let title = match line {
            Some(l) => l.trim_end_matches("...").trim().to_string(),
            None => self
                .file
                .as_ref()
                .and_then(|f| Path::new(&f.name).file_stem().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "Untitled".into()),
        };
        truncate(&title, 90)
    }

    pub fn text(&self) -> String {
        html_to_text(&self.description)
    }

    pub fn hashtags(&self) -> Vec<String> {
        let mut tags: Vec<String> = Vec::new();
        for word in self.text().split(|c: char| c.is_whitespace() || c == ',') {
            let w = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if w.len() > 1 && w.starts_with('#') && !tags.iter().any(|t| t.eq_ignore_ascii_case(w)) {
                tags.push(w.to_string());
            }
        }
        tags
    }

    pub fn url(&self) -> String {
        format!("{BASE}/post/{}/en/", self.lang_group)
    }

    /// Whether the post looks like anime / itasha artwork. The site has no flag for it, so this
    /// checks hashtags and a few franchise names in the text (words that are also normal War
    /// Thunder vocabulary, like "panzer" or "girl", are deliberately left out).
    pub fn looks_anime(&self) -> bool {
        let norm = |s: &str| s.to_lowercase().replace(['_', '-', ' ', '\''], "");
        let tags: Vec<String> = self.hashtags().iter().map(|t| norm(t.trim_start_matches('#'))).collect();
        if tags.iter().any(|t| ANIME_TAGS.iter().any(|a| norm(a) == *t)) {
            return true;
        }
        let text = format!(" {} ", self.text().to_lowercase());
        ANIME_WORDS.iter().any(|w| {
            text.match_indices(w).any(|(i, _)| {
                let before = text[..i].chars().last().unwrap_or(' ');
                let after = text[i + w.len()..].chars().next().unwrap_or(' ');
                !before.is_alphanumeric() && !after.is_alphanumeric()
            })
        })
    }
}

/// Hashtags (compared without case, spaces, `_` or `-`) that mark anime-styled posts. From a
/// sample of 200 popular and new camouflages.
const ANIME_TAGS: &[&str] = &[
    "anime", "animeskin", "anime_art", "animegirl", "itasha", "waifu", "weeb", "manga", "kawaii", "moe", "neko", "loli",
    "hentai", "ecchi", "nsfw", "lewd", "r18", "girls_und_panzer", "gup", "pravda_high_school", "girls_frontline",
    "azur_lane", "kancolle", "kantai_collection", "arknights", "rezero", "blue_archive", "genshin", "genshin_impact",
    "hololive", "vtuber", "touhou", "idolmaster", "theidolmaster", "evangelion", "hatsune_miku", "miku", "umamusume",
    "honkai", "nikke", "vocaloid",
];

/// Whole words or phrases in the description that mark anime-styled posts.
const ANIME_WORDS: &[&str] = &[
    "anime", "itasha", "waifu", "hentai", "girls und panzer", "girls' frontline", "girls frontline", "azur lane", "kancolle",
    "kantai collection", "blue archive", "genshin", "hololive", "touhou", "idolm@ster", "idolmaster", "evangelion",
    "arknights", "re:zero", "hatsune miku", "vocaloid", "uma musume", "umamusume",
];

/// A creator's public profile, read from their page (the site has no JSON for it).
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub nickname: String,
    pub avatar: Option<String>,
    pub reputation: u64,
    pub posts: u64,
    pub comments: u64,
    pub subscribers: u64,
}

impl Profile {
    pub fn url(id: u64) -> String {
        format!("{BASE}/user/{id}/")
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Comment {
    #[serde(default, deserialize_with = "de_u64")]
    pub id: u64,
    /// 0 = top level, else the id of the comment it answers.
    #[serde(default, deserialize_with = "de_u64")]
    pub parent: u64,
    #[serde(default, deserialize_with = "de_opt")]
    pub author: Option<Author>,
    #[serde(default, deserialize_with = "de_i64")]
    pub created: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub rating: i64,
    /// HTML.
    #[serde(default, deserialize_with = "de_string")]
    pub text: String,
    #[serde(default)]
    pub deleted: bool,
}

// ---------------------------------------------------------------------------------------------
// Requests

fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            // Downloads can be large; API calls set their own shorter timeout.
            .timeout(None)
            .build()
            .expect("HTTP client")
    })
}

fn net_err(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "live.warthunder.com did not answer in time".into()
    } else if e.is_connect() {
        "Could not connect to live.warthunder.com. Check your internet connection.".into()
    } else {
        format!("Network error: {e}")
    }
}

/// The live.warthunder.com session cookies ("name=value; ...") after logging in.
static SESSION: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

pub fn set_session(cookies: Option<String>) {
    if let Ok(mut s) = SESSION.write() {
        *s = cookies;
    }
}

fn session() -> Option<String> {
    SESSION.read().ok().and_then(|s| s.clone())
}

fn with_session(req: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    match session() {
        Some(c) => req.header(reqwest::header::COOKIE, c),
        None => req,
    }
}

fn post_form(path: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    let text = with_session(client().post(format!("{BASE}{path}")))
        .timeout(Duration::from_secs(25))
        .form(form)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(net_err)?;
    serde_json::from_str(&text).map_err(|_| "The site sent an unexpected answer".to_string())
}

/// Turns free text into the hashtag search the site understands ("leopard 2a6" -> "#leopard #2a6").
fn hashtagify(search: &str) -> String {
    search
        .split_whitespace()
        .map(|w| if w.starts_with('#') { w.to_string() } else { format!("#{w}") })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn fetch_feed(
    kind: Kind,
    sort: Sort,
    search: &str,
    vehicle_type: &str,
    page: u32,
) -> Result<Vec<Post>, String> {
    let (sort_key, period) = sort.params();
    let search = hashtagify(search);
    let page = page.to_string();
    let mut form = vec![
        ("content", kind.code()),
        ("sort", sort_key),
        ("period", period),
        ("searchString", search.as_str()),
        ("page", page.as_str()),
        ("subtype", "all"),
    ];
    if !vehicle_type.is_empty() {
        form.push(("vehicleType", vehicle_type));
    }
    let v = post_form("/api/feed/get_regular/", &form)?;
    if v.get("status").and_then(Value::as_str) != Some("OK") {
        let msg = v.get("msg").and_then(Value::as_str).unwrap_or("unknown error");
        return Err(format!("live.warthunder.com: {msg}"));
    }
    let list = v
        .pointer("/data/list")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(list
        .into_iter()
        .filter_map(|item| serde_json::from_value::<Post>(item).ok())
        .filter(|p| p.lang_group != 0 && p.kind() == Some(kind))
        .collect())
}

/// Full post with medium-size images and the untruncated description.
pub fn fetch_post(lang_group: u64) -> Result<Post, String> {
    let id = lang_group.to_string();
    let v = post_form("/api/posts/get/", &[("lang_group", id.as_str()), ("language", "en")])?;
    if v.get("status").and_then(Value::as_str) == Some("ERR") {
        return Err("This post no longer exists".into());
    }
    serde_json::from_value(v).map_err(|_| "Could not read the post".into())
}

fn ok_list(v: &Value) -> Result<Vec<Value>, String> {
    if v.get("status").and_then(Value::as_str) != Some("OK") {
        let msg = v.get("msg").or_else(|| v.get("message")).and_then(Value::as_str).unwrap_or("unknown error");
        return Err(format!("live.warthunder.com: {msg}"));
    }
    Ok(v.pointer("/data/list").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// One page (25) of a creator's posts, newest first, only the types this app can install.
pub fn fetch_user_posts(user_id: u64, page: u32) -> Result<(Vec<Post>, bool), String> {
    let (user, page) = (user_id.to_string(), page.to_string());
    let form = [
        ("content", "all"),
        ("sort", "created"),
        ("user", user.as_str()),
        ("period", "0"),
        ("searchString", ""),
        ("page", page.as_str()),
        ("featured", "false"),
        ("subtype", "all"),
    ];
    let list = ok_list(&post_form("/api/feed/get_user/", &form)?)?;
    let more = !list.is_empty();
    let posts = list
        .into_iter()
        .filter_map(|item| serde_json::from_value::<Post>(item).ok())
        .filter(|p| p.lang_group != 0 && p.kind().is_some())
        .collect();
    Ok((posts, more))
}

/// The number in `<strong>N</strong>` right after `marker` ("Reputation: <strong>2036</strong>").
fn strong_after(html: &str, marker: &str) -> u64 {
    (|| {
        let rest = &html[html.find(marker)? + marker.len()..];
        let a = rest.find("<strong>")? + 8;
        let b = rest[a..].find("</strong>")? + a;
        rest[a..b].trim().replace([',', ' '], "").parse().ok()
    })()
    .unwrap_or(0)
}

/// The number in `<strong>N</strong>` right before `suffix` ("<strong>21</strong> posts").
fn strong_before(html: &str, suffix: &str) -> u64 {
    (|| {
        let before = &html[..html.find(suffix)?];
        let a = before.rfind("<strong>")? + 8;
        before[a..].trim().replace([',', ' '], "").parse().ok()
    })()
    .unwrap_or(0)
}

fn img_after(html: &str, marker: &str) -> Option<String> {
    let i = html.find(marker)?;
    let rest = &html[i..];
    let a = rest.find("<img")?;
    let src = rest[a..].find("src=\"")? + a + 5;
    let end = rest[src..].find('"')? + src;
    let url = decode_entities(&rest[src..end]);
    (!url.is_empty() && !url.contains("no_avatar")).then_some(url)
}

pub fn fetch_profile(user_id: u64) -> Result<Profile, String> {
    let html = with_session(client().get(Profile::url(user_id)))
        .timeout(Duration::from_secs(25))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(net_err)?;
    let nickname = html
        .find("data_line nickname")
        .and_then(|i| {
            let rest = &html[i..];
            let a = rest.find("<span>")? + 6;
            let b = rest[a..].find("</span>")? + a;
            Some(decode_entities(rest[a..b].trim()))
        })
        .unwrap_or_default();
    if nickname.is_empty() {
        return Err("This creator's profile could not be read".into());
    }
    Ok(Profile {
        nickname,
        avatar: img_after(&html, "class=\"avatar \"").or_else(|| img_after(&html, "class=\"avatar\"")),
        reputation: strong_after(&html, "Reputation:"),
        posts: strong_before(&html, "</strong> posts"),
        comments: strong_before(&html, "</strong> comments"),
        subscribers: strong_before(&html, "</strong> subscribers"),
    })
}

/// All comments of a post (the site sends them in one answer). Works without logging in.
pub fn fetch_comments(post_id: u64, lang_group: u64) -> Result<Vec<Comment>, String> {
    let (pid, lg) = (post_id.to_string(), lang_group.to_string());
    let list = ok_list(&post_form("/api/comment/get_list/", &[("postId", pid.as_str()), ("langGroup", lg.as_str())])?)?;
    Ok(list.into_iter().filter_map(|c| serde_json::from_value::<Comment>(c).ok()).filter(|c| !c.deleted).collect())
}

fn ok_data(v: Value) -> Result<Value, String> {
    if v.get("status").and_then(Value::as_str) == Some("OK") {
        return Ok(v.get("data").cloned().unwrap_or(Value::Null));
    }
    let msg = v.get("msg").or_else(|| v.get("message")).and_then(Value::as_str).unwrap_or("");
    Err(if msg.is_empty() { "live.warthunder.com refused this. Are you still logged in?".into() } else { format!("live.warthunder.com: {msg}") })
}

/// Toggles the like on a post (needs a session). Returns (likes, liked now).
pub fn toggle_like(lang_group: u64) -> Result<(u64, bool), String> {
    if session().is_none() {
        return Err("Log in to like posts".into());
    }
    let lg = lang_group.to_string();
    let data = ok_data(post_form("/api/post/like/", &[("lang_group", lg.as_str())])?)?;
    let likes = data.get("likes").and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok())).unwrap_or(0);
    let liked = data.get("isLiked").and_then(|v| v.as_bool().or_else(|| Some(v.as_u64()? != 0))).unwrap_or(false);
    Ok((likes, liked))
}

/// Posts a comment (needs a session). `reply_to` is the id of the comment being answered.
pub fn post_comment(post_id: u64, lang_group: u64, text: &str, reply_to: Option<u64>) -> Result<(), String> {
    if session().is_none() {
        return Err("Log in to comment".into());
    }
    let (pid, lg) = (post_id.to_string(), lang_group.to_string());
    let reply = reply_to.map(|r| r.to_string()).unwrap_or_default();
    let mut form = vec![("text", text), ("postId", pid.as_str()), ("langGroup", lg.as_str())];
    if reply_to.is_some() {
        form.push(("replyId", reply.as_str()));
    }
    ok_data(post_form("/api/comment/create/", &form)?).map(|_| ())
}

/// The logged-in user's id according to the site (0 = the cookies are not, or no longer, valid).
pub fn whoami() -> Result<u64, String> {
    let html = with_session(client().get(format!("{BASE}/feed/")))
        .timeout(Duration::from_secs(25))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(net_err)?;
    Ok(parse_user_id(&html))
}

/// Reads `user_id: 123` from the page's `live_wt` settings script.
pub fn parse_user_id(html: &str) -> u64 {
    html.find("user_id:")
        .map(|i| html[i + 8..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Downloads `url` to `dest` (via a .part file), reporting (bytes done, total) as it goes.
/// Returns the file's upload date (`Last-Modified`) if the server sent one.
/// Streams `url` to `dest`. `progress` is called every 256 KB; returning `false` cancels.
pub fn download(url: &str, dest: &Path, mut progress: impl FnMut(u64, Option<u64>) -> bool) -> Result<Option<i64>, String> {
    let mut resp = client()
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(net_err)?;
    let total = resp.content_length();
    let last_modified = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .and_then(crate::updates::parse_http_date);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Could not create download folder: {e}"))?;
    }
    let part = dest.with_extension("part");
    let mut out = fs::File::create(&part).map_err(|e| format!("Could not save download: {e}"))?;
    let mut buf = vec![0u8; 64 * 1024];
    let (mut done, mut reported) = (0u64, 0u64);
    loop {
        let n = resp.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("Could not save download: {e}"))?;
        done += n as u64;
        if done - reported >= 256 * 1024 {
            if !progress(done, total) {
                drop(out);
                let _ = fs::remove_file(&part);
                return Err(CANCELLED.into());
            }
            reported = done;
        }
    }
    progress(done, total);
    drop(out);
    fs::rename(&part, dest).map_err(|e| format!("Could not save download: {e}"))?;
    Ok(last_modified)
}

// ---------------------------------------------------------------------------------------------
// Text helpers

pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max_chars - 1).collect();
        t.push('…');
        t
    }
}

/// Minimal HTML to plain text: block tags become line breaks, entities are decoded.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let Some(gt) = rest[lt..].find('>') else {
            rest = &rest[lt..];
            break;
        };
        let tag = rest[lt + 1..lt + gt].trim().to_ascii_lowercase();
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        match name.as_str() {
            "br" => out.push('\n'),
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" if tag.starts_with('/') => out.push('\n'),
            "li" if !tag.starts_with('/') => out.push_str("\n- "),
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    out.push_str(rest);
    let decoded = decode_entities(&out);
    let mut text = String::new();
    let mut blank = 0;
    for line in decoded.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 || text.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(line.trim_start());
        text.push('\n');
    }
    text.trim_end().to_string()
}

/// GET a web page as text (used for the official changelog).
pub fn get_text(url: &str) -> Result<String, String> {
    client()
        .get(url)
        .timeout(Duration::from_secs(25))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(net_err)
}

/// When the file behind a download link was last uploaded, from the CDN's `Last-Modified`
/// header. Posts have no "edited" field, but re-uploading a file changes this date.
pub fn file_last_modified(link: &str) -> Option<i64> {
    let resp = client().head(link).timeout(Duration::from_secs(20)).send().ok()?;
    let value = resp.headers().get(reqwest::header::LAST_MODIFIED)?.to_str().ok()?;
    crate::updates::parse_http_date(value)
}

pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let end = after.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let ent = &after[..e];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                    u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
                }
                _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            ch.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------------------
// Lenient deserializers

fn de_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        Value::Bool(b) => b as u64,
        _ => 0,
    })
}

fn de_i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    })
}

fn de_f32<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    })
}

fn de_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    })
}

fn de_opt<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).ok())
}

fn de_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items.into_iter().filter_map(|i| serde_json::from_value(i).ok()).collect(),
        _ => Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_is_flattened() {
        let t = html_to_text("<p><b>Leopard 2A8 &quot;Thomas&quot;</b></p><p>line&nbsp;two<br>three &#38; four</p>");
        assert_eq!(t, "Leopard 2A8 \"Thomas\"\nline two\nthree & four");
    }

    #[test]
    fn search_becomes_hashtags() {
        assert_eq!(hashtagify(" leopard  #t_34 "), "#leopard #t_34");
    }
}

#[cfg(test)]
mod site_tests {
    use super::*;

    fn post(desc: &str) -> Post {
        Post { description: desc.into(), ..Default::default() }
    }

    #[test]
    fn anime_detection() {
        assert!(post("Nishizumi's Tiger #girls_und_panzer").looks_anime());
        assert!(post("Cute skin #Anime #T_34").looks_anime());
        assert!(post("Azur Lane themed destroyer").looks_anime());
        assert!(!post("Panzer IV desert camo #pz4 #germany").looks_anime());
        assert!(!post("Fate of the Leopard 2 #leopard").looks_anime());
        assert!(!post("Animated decal pack").looks_anime()); // not the whole word "anime"
    }

    #[test]
    fn profile_numbers_parse() {
        let html = r#"<div class="data_line reputation"><span>Reputation: <strong>2036</strong></span></div>
            <div class="data_line"><span><strong>21</strong> posts</span></div>
            <div class="data_line subscribers"><span><strong>1,165</strong> subscribers</span></div>"#;
        assert_eq!(strong_after(html, "Reputation:"), 2036);
        assert_eq!(strong_before(html, "</strong> posts"), 21);
        assert_eq!(strong_before(html, "</strong> subscribers"), 1165);
        assert_eq!(strong_before(html, "</strong> comments"), 0);
        assert_eq!(parse_user_id("var live_wt = { user_id: 4242, x: 1 }"), 4242);
        assert_eq!(parse_user_id("user_id: 0,"), 0);
    }
}
