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
const USER_AGENT: &str = "WT-Armory-Manager/0.1 (desktop client)";

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

fn post_form(path: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    let text = client()
        .post(format!("{BASE}{path}"))
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

/// Downloads `url` to `dest` (via a .part file), reporting (bytes done, total) as it goes.
/// Returns the file's upload date (`Last-Modified`) if the server sent one.
pub fn download(url: &str, dest: &Path, mut progress: impl FnMut(u64, Option<u64>)) -> Result<Option<i64>, String> {
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
            progress(done, total);
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
