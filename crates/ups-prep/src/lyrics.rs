//! LRCLIB lyrics — port of lyrics.py's fetch/search + youtube-title
//! cleanup. syncedLyrics → [(ms, text)] lines; plainLyrics as fallback.

use serde::Deserialize;
use std::collections::HashSet;

const API: &str = "https://lrclib.net/api/get";
const SEARCH_API: &str = "https://lrclib.net/api/search";
const UA: &str = "UltraPiStar/1.0 (https://github.com/)";

#[derive(Debug, Clone, Default)]
pub struct LyricRecord {
    /// (ms, text) synced lines
    pub synced: Vec<(i64, String)>,
    pub plain: Vec<String>,
    pub has_word_sync: bool,
    pub track_name: String,
    pub artist_name: String,
}

#[derive(Deserialize)]
struct LrcHit {
    #[serde(rename = "trackName")]
    track_name: Option<String>,
    #[serde(rename = "artistName")]
    artist_name: Option<String>,
    duration: Option<f64>,
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
    #[serde(rename = "hasWordSync")]
    has_word_sync: Option<bool>,
}

/// Parse LRC `[mm:ss.xx]` lines → (ms, text). Multi-tag lines expand.
pub fn parse_lrc(src: &str) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    for ln in src.lines() {
        let mut rest = ln;
        let mut ts = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let tag = &stripped[..end];
            let Some((m, s)) = tag.split_once(':') else { break };
            let (Ok(mm), Ok(ss)) =
                (m.trim().parse::<f64>(), s.trim().parse::<f64>())
            else { break };
            ts.push((mm * 60_000.0 + ss * 1000.0) as i64);
            rest = &stripped[end + 1..];
        }
        let text = rest.trim();
        for t in ts {
            if !text.is_empty() || t > 0 {
                out.push((t, text.to_string()));
            }
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

// ---- youtube title cleanup -------------------------------------------

fn clean_title(t: &str) -> String {
    // bracketed segments carrying flavor words get dropped entirely
    let mut out = String::new();
    let mut depth = 0i32;
    let mut seg = String::new();
    for c in t.chars() {
        match c {
            '(' | '[' => { depth += 1; seg.clear(); }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if !seg_junk(&seg) && depth == 0 {
                    out.push_str(&format!("({seg})"));
                }
                seg.clear();
            }
            _ if depth > 0 => seg.push(c),
            _ => out.push(c),
        }
    }
    squash(&out)
}

fn seg_junk(s: &str) -> bool {
    let l = s.to_lowercase();
    ["lyric", "official", "music video", "audio", "color coded",
     "han_rom_eng", "english", "espanol", "karaoke", "fan made",
     "ver.", "version", "hd", "4k", " cc", "m/v", "mv", "가사"]
        .iter().any(|j| l.contains(j))
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
        .trim_matches(|c: char| " \t-–—'\"".contains(c))
        .to_string()
}

/// Strip ALL bracketed segments.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    squash(&out)
}

fn chan_clean(a: &str) -> String {
    let mut s = a.to_string();
    for pat in ["- topic", "vevo", " official", " music"] {
        if let Some(i) = s.to_lowercase().rfind(pat) {
            s.truncate(i);
        }
    }
    s.trim().to_string()
}

/// Ordered (artist, title) guesses for LRCLIB — port of yt_candidates.
pub fn yt_candidates(artist: &str, title: &str) -> Vec<(String, String)> {
    let mut cands: Vec<(String, String)> = Vec::new();
    let t = title.trim();
    for sep in [" - ", " – ", " — ", " | "] {
        if let Some((a, tt)) = t.split_once(sep) {
            cands.push((clean_title(a), clean_title(tt)));
            break;
        }
    }
    cands.push((artist.to_string(), clean_title(t)));
    // retry with parens stripped
    for (ar, ti) in cands.clone() {
        let (pa, pt) = (plain(&ar), plain(&ti));
        if pa != ar || pt != ti {
            cands.push((if pa.is_empty() { ar } else { pa },
                        if pt.is_empty() { ti } else { pt }));
        }
    }
    // channel decorations → bare artist
    for (ar, ti) in cands.clone() {
        let cc = chan_clean(&ar);
        if !cc.is_empty() && cc != ar {
            cands.push((cc, ti));
        }
    }
    // bare-title lookup for uploads with no usable artist
    for ti in [clean_title(t), plain(&clean_title(t))] {
        if !ti.is_empty() {
            cands.push((String::new(), ti));
        }
    }
    let mut seen = HashSet::new();
    cands.into_iter()
        .filter(|(a, t)| !t.is_empty() && seen.insert((a.clone(), t.clone())))
        .collect()
}

fn record(d: LrcHit) -> Option<LyricRecord> {
    let tn = d.track_name?;
    Some(LyricRecord {
        synced: parse_lrc(&d.synced_lyrics.unwrap_or_default()),
        plain: d.plain_lyrics.unwrap_or_default().lines()
            .map(str::trim).filter(|l| !l.is_empty())
            .map(String::from).collect(),
        has_word_sync: d.has_word_sync.unwrap_or(false),
        track_name: tn,
        artist_name: d.artist_name.unwrap_or_default(),
    })
}

/// Exact-ish fetch: artist+title (+duration) → best record.
pub fn fetch(artist: &str, title: &str, duration: Option<f64>)
             -> Option<LyricRecord> {
    let mut url = format!("{API}?track_name={}",
                          urlenc(title));
    if !artist.is_empty() {
        url.push_str(&format!("&artist_name={}", urlenc(artist)));
    }
    if let Some(d) = duration {
        url.push_str(&format!("&duration={d:.0}"));
    }
    let hit = http_json::<LrcHit>(&url).ok()?;
    record(hit)
}

/// Fuzzy search — hits whose duration agrees with the media.
pub fn search(query: &str, duration: Option<f64>)
              -> Vec<LyricRecord> {
    let url = format!("{SEARCH_API}?q={}", urlenc(query));
    let Ok(hits) = http_json::<Vec<LrcHit>>(&url) else {
        return Vec::new();
    };
    hits.into_iter()
        .filter(|d| d.track_name.is_some())
        .filter(|d| d.synced_lyrics.is_some() || d.plain_lyrics.is_some())
        .filter(|d| match (duration, d.duration) {
            (Some(want), Some(got)) => (got - want).abs() <= 15.0,
            _ => true,
        })
        .filter_map(record)
        .collect()
}

fn http_json<T: for<'de> Deserialize<'de>>(url: &str)
        -> Result<T, reqwest::Error> {
    reqwest::blocking::Client::new()
        .get(url)
        .header("User-Agent", UA)
        .timeout(std::time::Duration::from_secs(10))
        .send()?
        .json()
}

fn urlenc(s: &str) -> String {
    s.chars().map(|c| match c {
        'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
            c.to_string()
        }
        ' ' => "%20".into(),
        _ => format!("%{:02X}", c as u32),
    }).collect()
}
