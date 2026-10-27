//! Album art lookup — iTunes → Deezer, port of artwork.py.

use anyhow::Result;
use std::path::Path;

const UA: &str = "ultrapistar/0.4 (+https://github.com)";

fn clean(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn get_json(url: &str) -> Option<serde_json::Value> {
    reqwest::blocking::Client::new()
        .get(url).header("User-Agent", UA)
        .timeout(std::time::Duration::from_secs(10))
        .send().ok()?.json().ok()
}

fn urlenc(s: &str) -> String {
    s.chars().map(|c| match c {
        'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
            c.to_string()
        }
        ' ' => "+".into(),
        _ => format!("%{:02X}", c as u32),
    }).collect()
}

fn itunes_art(artist: &str, title: &str) -> Option<String> {
    let q = urlenc(&format!("{} {}", clean(artist), clean(title)));
    let data = get_json(&format!(
        "https://itunes.apple.com/search?term={q}&entity=song&limit=5"))?;
    let want = (clean(artist), clean(title));
    let mut best: Option<(i32, String)> = None;
    for item in data["results"].as_array()? {
        let got = (clean(item["artistName"].as_str().unwrap_or("")),
                   clean(item["trackName"].as_str().unwrap_or("")));
        let Some(art) = item["artworkUrl100"].as_str() else { continue };
        let score = (got.0 == want.0) as i32
            + (want.1.contains(&got.1) || got.1.contains(&want.1)) as i32;
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, art.to_string()));
        }
    }
    let (score, art) = best?;
    if score == 0 {
        return None; // random result, not a real match
    }
    Some(art.replace("100x100", "600x600"))
}

fn deezer_art(artist: &str, title: &str) -> Option<String> {
    let q = urlenc(&format!("artist:\"{artist}\" track:\"{title}\""));
    let data = get_json(&format!(
        "https://api.deezer.com/search?q={q}&limit=5"))?;
    let (want_a, want_t) = (clean(artist), clean(title));
    for item in data["data"].as_array()? {
        let got_a = clean(item["artist"]["name"].as_str().unwrap_or(""));
        let got_t = clean(item["title"].as_str().unwrap_or(""));
        if !(got_a == want_a || want_t.contains(&got_t)
             || got_t.contains(&want_t)) {
            continue;
        }
        if let Some(art) = item["album"]["cover_xl"].as_str()
            .or(item["album"]["cover_big"].as_str()) {
            return Some(art.to_string());
        }
    }
    None
}

/// Best remote artwork URL for artist/title.
pub fn lookup(artist: &str, title: &str) -> Option<String> {
    itunes_art(artist, title).or_else(|| deezer_art(artist, title))
}

/// Screengrab from the media ~20% in → `dest` (jpeg via ffmpeg).
fn grab_frame(media: &Path, dest: &Path) -> bool {
    let off = crate::ff::duration(media).ok()
        .map(|d| (d * 0.2).clamp(5.0, 60.0))
        .unwrap_or(10.0);
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-ss", &format!("{off:.1}"),
               "-i"]).arg(media)
        .args(["-frames:v", "1", "-q:v", "4"])
        .arg(dest)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    ok && dest.metadata().map(|m| m.len() > 2000).unwrap_or(false)
}

/// Artwork to `dest`. A screengrab wins where one is available — the
/// provider's poster frame (YouTube thumb) or a frame grabbed from
/// the media — matching what plays on the TV. Album art (iTunes →
/// Deezer) is the fallback for audio-only sources.
pub fn save(artist: &str, title: &str, dest: &Path,
            thumb_url: Option<&str>, media: Option<&Path>) -> Result<bool> {
    if let Some(url) = thumb_url {
        if let Ok(resp) = reqwest::blocking::Client::new()
            .get(url).header("User-Agent", UA)
            .timeout(std::time::Duration::from_secs(15)).send() {
            if let Ok(bytes) = resp.bytes() {
                if bytes.len() > 2000 {
                    std::fs::write(dest, &bytes)?;
                    return Ok(true);
                }
            }
        }
    }
    if let Some(m) = media {
        if grab_frame(m, dest) {
            return Ok(true);
        }
    }
    if let Some(url) = lookup(artist, title) {
        if let Ok(resp) = reqwest::blocking::Client::new()
            .get(&url).header("User-Agent", UA)
            .timeout(std::time::Duration::from_secs(15)).send() {
            if let Ok(bytes) = resp.bytes() {
                if bytes.len() > 2000 {
                    std::fs::write(dest, &bytes)?;
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}
