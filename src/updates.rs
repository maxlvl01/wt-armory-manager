//! Major War Thunder updates and the "is this mod made for the current version?" check.
//!
//! The latest major update is read from the official changelog
//! (https://warthunder.com/en/game/changelog/): it is always the pinned first entry titled
//! `War Thunder "<Name>" - Changelog`. Its version number is taken from the first hotfix posted
//! after it (`Update 2.59.0.11`). A built-in list is the offline fallback.

use crate::api::{self, Kind};

const CHANGELOG_URL: &str = "https://warthunder.com/en/game/changelog/?page=1";

#[derive(Clone, Debug, PartialEq)]
pub struct MajorUpdate {
    /// "2.59"; empty if it could not be determined.
    pub version: String,
    pub name: String,
    /// Unix seconds, 00:00 UTC on the release day.
    pub date: i64,
}

impl MajorUpdate {
    pub fn label(&self) -> String {
        if self.version.is_empty() {
            format!("\"{}\"", self.name)
        } else {
            format!("{} \"{}\"", self.version, self.name)
        }
    }
}

/// Major updates, newest first (verified against the official changelog on 2026-11-08).
const KNOWN: &[(&str, &str, (i64, u32, u32))] = &[
    ("2.59", "Sky Odyssey", (2026, 9, 16)),
    ("2.57", "Heavy Cavalry", (2026, 6, 24)),
    ("2.55", "Ninth Wave", (2026, 3, 18)),
    ("2.53", "Line of Contact", (2025, 12, 16)),
    ("2.51", "Spearhead", (2025, 11, 11)),
    ("2.49", "Tusk Force", (2025, 9, 23)),
    ("2.47", "Leviathans", (2025, 6, 25)),
    ("2.45", "Hornet's Sting", (2025, 3, 18)),
    ("2.43", "Storm Warning", (2024, 12, 16)),
    ("2.41", "Firebirds", (2024, 11, 19)),
    ("2.39", "Dance of Dragons", (2024, 9, 10)),
    ("2.37", "Seek & Destroy", (2024, 6, 19)),
    ("2.35", "Alpha Strike", (2024, 3, 13)),
    ("2.33", "Air Superiority", (2023, 12, 14)),
];

pub fn known_updates() -> Vec<MajorUpdate> {
    KNOWN
        .iter()
        .map(|(v, n, (y, m, d))| MajorUpdate { version: v.to_string(), name: n.to_string(), date: days_from_civil(*y, *m, *d) * 86_400 })
        .collect()
}

/// The known list with the freshly fetched latest update merged in (newest first).
pub fn merged(latest: Option<&MajorUpdate>) -> Vec<MajorUpdate> {
    let mut list = known_updates();
    if let Some(l) = latest {
        list.retain(|u| !u.name.eq_ignore_ascii_case(&l.name));
        list.push(l.clone());
        list.sort_by_key(|u| std::cmp::Reverse(u.date));
    }
    list
}

pub fn fetch_latest() -> Result<MajorUpdate, String> {
    let html = api::get_text(CHANGELOG_URL)?;
    parse_changelog(&html).ok_or_else(|| "Could not read the War Thunder changelog".to_string())
}

struct Entry {
    id: u64,
    title: String,
    date: Option<i64>,
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let a = s.find(start)? + start.len();
    let b = s[a..].find(end)? + a;
    Some(&s[a..b])
}

fn parse_changelog(html: &str) -> Option<MajorUpdate> {
    let entries: Vec<Entry> = html
        .split("showcase__item")
        .skip(1)
        .filter_map(|block| {
            let id = between(block, "/changelog/current/", "\"")?.trim_end_matches('/').parse().ok()?;
            let title = between(block, "widget__title\">", "</div>")?;
            let title = api::decode_entities(&title.split_whitespace().collect::<Vec<_>>().join(" "));
            let date = between(block, "widget-meta__item", "</li>")
                .and_then(|s| s.split_once('>'))
                .and_then(|(_, text)| parse_long_date(text.trim()));
            Some(Entry { id, title, date })
        })
        .collect();

    // The major update is pinned on top, so take the first one, not the newest by date.
    let major = entries.iter().find(|e| major_name(&e.title).is_some())?;
    let name = major_name(&major.title)?.to_string();
    let date = major.date?;
    // Version: the first `Update X.Y.0.n` hotfix posted after the major.
    let version = entries
        .iter()
        .filter(|e| e.id > major.id)
        .filter_map(|e| hotfix_version(&e.title).map(|v| (e.id, v)))
        .min_by_key(|(id, _)| *id)
        .map(|(_, v)| v)
        .or_else(|| known_updates().into_iter().find(|u| u.name.eq_ignore_ascii_case(&name)).map(|u| u.version))
        .unwrap_or_default();
    Some(MajorUpdate { version, name, date })
}

/// `War Thunder "Sky Odyssey" - Changelog` -> `Sky Odyssey`
fn major_name(title: &str) -> Option<&str> {
    let rest = title.strip_prefix("War Thunder \"")?;
    let end = rest.rfind("\" - Changelog")?;
    Some(&rest[..end]).filter(|n| !n.is_empty())
}

/// `Update 2.59.0.11` -> `2.59` (only `.0.x` hotfixes, which follow a major directly)
fn hotfix_version(title: &str) -> Option<String> {
    let v = title.strip_prefix("Update")?.trim_start().split_whitespace().next()?;
    let parts: Vec<&str> = v.split('.').collect();
    let numeric = parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    (parts.len() == 4 && numeric && parts[2] == "0").then(|| format!("{}.{}", parts[0], parts[1]))
}

// ---------------------------------------------------------------------------------------------
// Compatibility

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Compat {
    /// Uploaded or updated on/after the latest major update.
    Current,
    /// `n` major updates have come out since the file was uploaded.
    Behind(usize),
}

pub fn check(updated: i64, majors: &[MajorUpdate]) -> Compat {
    // A day of slack: authors often re-upload on release day itself.
    let behind = majors.iter().filter(|m| m.date > updated + 86_400).count();
    if behind == 0 {
        Compat::Current
    } else {
        Compat::Behind(behind)
    }
}

/// How likely a type is to break with a major update.
pub fn sensitive(kind: Kind) -> bool {
    !matches!(kind, Kind::Camouflage | Kind::Controls)
}

pub fn explain(compat: Compat, kind: Kind, majors: &[MajorUpdate]) -> String {
    let latest = majors.first().map(|m| format!("{} ({})", m.label(), fmt_date(m.date))).unwrap_or_default();
    match compat {
        Compat::Current => format!("Made or updated for the current version: uploaded after {latest}."),
        Compat::Behind(n) => {
            let since = majors.get(n - 1).map(|m| m.label()).unwrap_or_default();
            let risk = match kind {
                Kind::Sound => "Sound mods often stop working or sound wrong after major updates.",
                Kind::Location => "Custom hangars often break after major updates.",
                Kind::Model | Kind::Mission => "User models and missions can break when the game's files change.",
                Kind::Sight => "Sights usually keep working, but new vehicles may not be covered.",
                Kind::Camouflage => "Skins usually keep working unless the vehicle's model was changed.",
                Kind::Controls => "Control presets keep working, but new keybinds from later updates will be unset.",
            };
            format!(
                "Uploaded before {since}; {n} major update{} since. {risk}",
                if n == 1 { "" } else { "s" }
            )
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Dates (no date crate needed: civil-from-days per Howard Hinnant's algorithms)

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn month(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    MONTHS.iter().position(|m| n.starts_with(m)).map(|i| i as u32 + 1)
}

/// "16 September 2026" -> unix seconds
pub fn parse_long_date(s: &str) -> Option<i64> {
    let mut it = s.split_whitespace();
    let d: u32 = it.next()?.parse().ok()?;
    let m = month(it.next()?)?;
    let y: i64 = it.next()?.parse().ok()?;
    Some(days_from_civil(y, m, d) * 86_400)
}

/// RFC 7231 date, "Tue, 25 Mar 2025 09:01:16 GMT" -> unix seconds
pub fn parse_http_date(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    let d: u32 = parts[1].parse().ok()?;
    let m = month(parts[2])?;
    let y: i64 = parts[3].parse().ok()?;
    let hms: Vec<i64> = parts[4].split(':').filter_map(|p| p.parse().ok()).collect();
    let secs = match hms.as_slice() {
        [h, mi, se] => h * 3600 + mi * 60 + se,
        _ => 0,
    };
    Some(days_from_civil(y, m, d) * 86_400 + secs)
}

/// "16 Sep 2026"
pub fn fmt_date(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    let name = MONTHS[(m - 1) as usize];
    let mut name = name.to_string();
    name[..1].make_ascii_uppercase();
    format!("{d} {name} {y}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(parse_long_date("16 September 2026"), Some(days_from_civil(2026, 9, 16) * 86_400));
        assert_eq!(parse_http_date("Tue, 25 Mar 2025 09:01:16 GMT"), Some(1_742_893_276));
        assert_eq!(fmt_date(1_742_893_276), "25 Mar 2025");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn changelog_is_parsed() {
        let html = r#"
        <div class="showcase__item widget"><a class="widget__link" href="/en/game/changelog/current/1894"></a>
          <div class="widget__pin"></div><div class="widget__content"><div class="widget__title">
            War Thunder &quot;Sky Odyssey&quot; - Changelog
          </div><ul class="widget__meta widget-meta"><li class="widget-meta__item widget-meta__item--right">
            16 September 2026 </li></ul></div></div>
        <div class="showcase__item widget"><a class="widget__link" href="/en/game/changelog/current/1990"></a>
          <div class="widget__title"> Update 2.59.0.92 </div><li class="widget-meta__item">29 October 2026</li></div>
        <div class="showcase__item widget"><a class="widget__link" href="/en/game/changelog/current/1895"></a>
          <div class="widget__title"> Update 2.59.0.11 </div><li class="widget-meta__item">17 September 2026</li></div>
        <div class="showcase__item widget"><a class="widget__link" href="/en/game/changelog/current/1880"></a>
          <div class="widget__title"> Update 2.57.0.120 </div><li class="widget-meta__item">2 September 2026</li></div>
        "#;
        let u = parse_changelog(html).unwrap();
        assert_eq!(u.name, "Sky Odyssey");
        assert_eq!(u.version, "2.59");
        assert_eq!(u.date, days_from_civil(2026, 9, 16) * 86_400);
    }

    #[test]
    fn compat_counts_majors_since_upload() {
        let majors = known_updates();
        assert_eq!(check(days_from_civil(2026, 10, 1) * 86_400, &majors), Compat::Current);
        assert_eq!(check(days_from_civil(2026, 9, 16) * 86_400, &majors), Compat::Current, "release day counts");
        assert_eq!(check(days_from_civil(2026, 7, 1) * 86_400, &majors), Compat::Behind(1));
        assert_eq!(check(days_from_civil(2025, 3, 25) * 86_400, &majors), Compat::Behind(7));
    }
}
