//! Library layout conventions — port of `ultrapistar/mediastore.py`
//! naming rules so Rust-written folders stay identical to Python's.
//!
//! Canonical layout: `<root>/<artist_slug>/<title_slug>/` holding the
//! media + derived sidecars (`.inst.mp4`, `.vocals.wav`,
//! `.analysis.json`, artwork, `.upsong`).

/// lowercase_snake folder component — unicode letters survive
/// (日本語 titles keep their characters), everything else folds to `_`.
/// Mirrors Python's `[^\w]+` collapse + `_+` squeeze + 60-char cap.
pub fn slug(s: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut sep_pending = false;
    for c in s.trim().to_lowercase().chars() {
        if c.is_alphanumeric() || c == '_' {
            if sep_pending && !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            sep_pending = false;
            out.push(c);
        } else {
            sep_pending = true;
        }
    }
    // collapse '_' runs (including literal ones abutting folded runs)
    // and trim — split/filter/join covers both
    let collapsed = out
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    let capped: String = collapsed.chars().take(60).collect();
    let capped = capped.trim_end_matches('_');
    if capped.is_empty() {
        fallback.to_string()
    } else {
        capped.to_string()
    }
}

/// Root-relative `artist_slug/title_slug` song directory.
pub fn song_reldir(artist: &str, title: &str, fallback: &str) -> String {
    let a = slug(artist, "");
    let t = slug(title, &slug(fallback, ""));
    format!("{}/{}", if a.is_empty() { "unknown".to_string() } else { a }, t)
}

/// Legacy `Artist - Title` single-level name — kept for chart file
/// naming.
pub fn song_folder(artist: &str, title: &str, fallback: &str) -> String {
    let bad = |s: &str| -> String {
        s.chars()
            .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                      && !c.is_control())
            .collect()
    };
    let mut name = bad(&format!("{} - {}", artist, title));
    name = name.trim_matches(|c| c == ' ' || c == '.' || c == '-').to_string();
    if name.is_empty() {
        name = bad(fallback).trim_matches(|c| c == ' ' || c == '.' || c == '-').to_string();
    }
    let capped: String = name.chars().take(80).collect();
    let capped = capped.trim_end_matches(|c| c == ' ' || c == '.');
    if capped.is_empty() { "unknown".into() } else { capped.into() }
}
