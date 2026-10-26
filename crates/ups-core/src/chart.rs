//! Chart model + UltraStar (.txt) importer + native .upsong format.
//! Port of `ultrapistar/charts.py` (GPL-3.0) — see NOTICE.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const NATIVE_FORMAT: &str = "upsong";
pub const NATIVE_VERSION: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// normal | golden | freestyle | rap | rap-golden
    pub kind: String,
    pub start: i64,
    pub length: i64,
    pub pitch: i64,
    /// clean syllable text (no edge spaces, no '~')
    pub text: String,
    /// this syllable starts a new display word
    pub word_break: bool,
    /// romanized text when the lyric script isn't latin
    pub alt: String,
}

impl Note {
    fn new(kind: &str, start: i64, length: i64, pitch: i64, text: &str) -> Self {
        Note {
            kind: kind.to_string(),
            start,
            length,
            pitch,
            text: text.to_string(),
            word_break: false,
            alt: String::new(),
        }
    }
}

/// One lyric line grouped from notes split at '-' phrase breaks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phrase {
    pub start: i64,
    pub end: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atext: Option<String>,
    pub syls: Vec<Syl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Syl {
    pub t: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub wb: bool,
    pub s: i64,
    pub e: i64,
}

#[derive(Debug, Default)]
pub struct Chart {
    pub headers: BTreeMap<String, String>,
    pub tracks: BTreeMap<String, Vec<Note>>,
    /// phrase-end beats per track
    pub breaks: BTreeMap<String, Vec<i64>>,
    pub errors: Vec<String>,
    /// provenance (native files only)
    pub source: serde_json::Value,
}

impl Chart {
    pub fn new() -> Self {
        let mut c = Chart::default();
        c.tracks.insert("P1".to_string(), Vec::new());
        c.source = serde_json::Value::Null;
        c
    }

    pub fn artist(&self) -> &str {
        self.headers.get("ARTIST").map(|s| s.as_str()).unwrap_or("")
    }
    pub fn title(&self) -> &str {
        self.headers.get("TITLE").map(|s| s.as_str()).unwrap_or("")
    }
    pub fn bpm(&self) -> f64 {
        self.headers
            .get("BPM")
            .and_then(|v| v.replace(',', ".").parse().ok())
            .unwrap_or(0.0)
    }
    pub fn gap_ms(&self) -> f64 {
        self.headers
            .get("GAP")
            .and_then(|v| v.replace(',', ".").parse().ok())
            .unwrap_or(0.0)
    }
    pub fn is_duet(&self) -> bool {
        self.tracks.len() > 1
    }

    /// Notes grouped into lyric lines split at '-' phrase breaks.
    pub fn lines(&self, track: &str) -> Vec<Vec<Note>> {
        let mut brks: Vec<i64> = self
            .breaks
            .get(track)
            .cloned()
            .unwrap_or_default();
        brks.sort_unstable();
        brks.dedup();
        let mut lines: Vec<Vec<Note>> = Vec::new();
        let mut cur: Vec<Note> = Vec::new();
        let mut i = 0;
        for n in self.tracks.get(track).into_iter().flatten() {
            while i < brks.len() && brks[i] <= n.start {
                if let Some(last) = cur.last() {
                    if last.start <= brks[i] {
                        lines.push(std::mem::take(&mut cur));
                    }
                }
                i += 1;
            }
            cur.push(n.clone());
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        lines
    }

    /// {track: [Phrase]} — per-syllable spans for karaoke lyric fill.
    pub fn phrases(&self) -> BTreeMap<String, Vec<Phrase>> {
        let mut result = BTreeMap::new();
        for track in self.tracks.keys() {
            let mut lines = Vec::new();
            for l in self.lines(track) {
                let join = |get: fn(&Note) -> String| -> String {
                    l.iter()
                        .enumerate()
                        .map(|(i, n)| {
                            format!(
                                "{}{}",
                                if i > 0 && n.word_break { " " } else { "" },
                                get(n)
                            )
                        })
                        .collect::<String>()
                        .trim()
                        .to_string()
                };
                let has_alt = l.iter().any(|n| !n.alt.is_empty());
                lines.push(Phrase {
                    start: l[0].start,
                    end: l.last().unwrap().start + l.last().unwrap().length,
                    text: join(|n| n.text.replace('~', "")),
                    atext: if has_alt {
                        Some(join(|n| {
                            (if n.alt.is_empty() { &n.text } else { &n.alt })
                                .replace('~', "")
                        }))
                    } else {
                        None
                    },
                    syls: l
                        .iter()
                        .map(|n| Syl {
                            t: n.text.replace('~', ""),
                            at: if n.alt.is_empty() {
                                None
                            } else {
                                Some(n.alt.clone())
                            },
                            wb: n.word_break,
                            s: n.start,
                            e: n.start + n.length,
                        })
                        .collect(),
                });
            }
            result.insert(track.clone(), lines);
        }
        result
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errs = self.errors.clone();
        if self.artist().is_empty() || self.title().is_empty() {
            errs.push("missing ARTIST/TITLE header".into());
        }
        if self.bpm() <= 0.0 {
            errs.push("invalid or missing BPM".into());
        }
        if !self.tracks.values().any(|t| !t.is_empty()) {
            errs.push("no notes parsed".into());
        }
        errs
    }
}

fn note_kind(c: char) -> Option<&'static str> {
    Some(match c {
        ':' => "normal",
        '*' => "golden",
        'F' => "freestyle",
        'R' => "rap",
        'G' => "rap-golden",
        _ => return None,
    })
}

/// Import an UltraStar .txt chart into the normalized model.
pub fn parse_chart(path: &Path) -> Chart {
    let mut chart = Chart::default();
    let mut current = "P1".to_string();
    let mut last_raw = String::new();
    let mut shifts: BTreeMap<String, i64> = BTreeMap::new();
    shifts.insert("P1".into(), 0);
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) => {
            chart.errors.push(format!("unreadable: {e}"));
            return chart;
        }
    };

    for (lineno, rawline) in raw.lines().enumerate() {
        let line = rawline.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Some(h) = line.strip_prefix('#') {
            let (key, value) = h.split_once(':').unwrap_or((h, ""));
            chart.headers
                .insert(key.trim().to_uppercase(), value.trim().to_string());
        } else if line.starts_with('P') && line[1..].chars().all(|c| c.is_ascii_digit()) && line.len() > 1 {
            current = line.trim().to_uppercase();
            chart.tracks.entry(current.clone()).or_default();
            shifts.entry(current.clone()).or_insert(0);
            last_raw.clear();
        } else if note_kind(line.chars().next().unwrap()).is_some() {
            // parse from rawline so syllable edge-spaces survive: charts
            // mark word boundaries with a leading space on the next
            // syllable OR a trailing space on this one
            let body = rawline.trim_start_matches([' ', '\t', '\u{feff}']);
            let body = &body[1..];
            let body = body.trim_start_matches([' ', '\t']);
            let mut parts = body.splitn(4, ' ');
            let (Some(p0), Some(p1), Some(p2), Some(raw_t)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                chart
                    .errors
                    .push(format!("line {}: malformed note", lineno + 1));
                continue;
            };
            let (Ok(start), Ok(length), Ok(pitch)) = (
                p0.parse::<i64>(),
                p1.parse::<i64>(),
                p2.parse::<i64>(),
            ) else {
                chart
                    .errors
                    .push(format!("line {}: non-numeric note fields", lineno + 1));
                continue;
            };
            let word_break = if raw_t.starts_with('~') {
                false
            } else {
                raw_t.chars().next().map(|c| c.is_whitespace()).unwrap_or(false)
                    || last_raw.chars().last().map(|c| c.is_whitespace()).unwrap_or(false)
                    || chart.tracks.get(&current).map(|t| t.is_empty()).unwrap_or(true)
            };
            last_raw = raw_t.to_string();
            let mut n = Note::new(
                note_kind(line.chars().next().unwrap()).unwrap(),
                start + shifts.get(&current).copied().unwrap_or(0),
                length,
                pitch,
                &raw_t.replace('~', "").trim().to_string(),
            );
            n.word_break = word_break;
            chart.tracks.entry(current.clone()).or_default().push(n);
        } else if let Some(rest) = line.strip_prefix('-') {
            let parts: Vec<&str> = rest.trim().split_whitespace().collect();
            let beat: i64 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
            let rel = chart
                .headers
                .get("RELATIVE")
                .map(|v| matches!(v.to_uppercase().as_str(), "YES" | "TRUE" | "1"))
                .unwrap_or(false);
            if rel {
                // USDX: Rel += Param2 — note beats count from their own
                // phrase's start
                let nxt: i64 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(beat);
                let s = shifts.entry(current.clone()).or_insert(0);
                *s += nxt;
                chart.breaks.entry(current.clone()).or_default().push(*s);
            } else {
                chart.breaks.entry(current.clone()).or_default().push(beat);
            }
        } else if line == "E" {
            break;
        } else {
            chart.errors.push(format!("line {}: unrecognized content", lineno + 1));
        }
    }
    chart
}

// ---------- native .upsong format ----------

#[derive(Serialize, Deserialize)]
struct NativeNote {
    s: i64,
    l: i64,
    p: i64,
    t: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    k: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wb: Option<i64>,
}

#[derive(Serialize)]
struct NativeChart<'a> {
    format: &'a str,
    version: i64,
    headers: &'a BTreeMap<String, String>,
    source: &'a serde_json::Value,
    tracks: BTreeMap<String, Vec<NativeNote>>,
    breaks: &'a BTreeMap<String, Vec<i64>>,
}

pub fn native_path(path: &Path) -> PathBuf {
    path.with_extension("upsong")
}

pub fn to_native(chart: &Chart) -> serde_json::Value {
    let tracks: BTreeMap<String, Vec<NativeNote>> = chart
        .tracks
        .iter()
        .map(|(t, notes)| {
            (
                t.clone(),
                notes
                    .iter()
                    .map(|n| NativeNote {
                        s: n.start,
                        l: n.length,
                        p: n.pitch,
                        t: n.text.clone(),
                        at: if n.alt.is_empty() { None } else { Some(n.alt.clone()) },
                        k: if n.kind == "normal" { None } else { Some(n.kind.clone()) },
                        wb: if n.word_break { Some(1) } else { None },
                    })
                    .collect(),
            )
        })
        .collect();
    serde_json::to_value(NativeChart {
        format: NATIVE_FORMAT,
        version: NATIVE_VERSION,
        headers: &chart.headers,
        source: &chart.source,
        tracks,
        breaks: &chart.breaks,
    })
    .unwrap_or(serde_json::Value::Null)
}

pub fn from_native(d: &serde_json::Value) -> Result<Chart, String> {
    if d.get("format").and_then(|v| v.as_str()) != Some(NATIVE_FORMAT)
        || d.get("version").and_then(|v| v.as_i64()) != Some(NATIVE_VERSION)
    {
        return Err("unsupported upsong format/version".into());
    }
    let mut chart = Chart::new();
    if let Some(h) = d.get("headers").and_then(|v| v.as_object()) {
        chart.headers = h
            .iter()
            .map(|(k, v)| (k.to_uppercase(), v.as_str().unwrap_or("").to_string()))
            .collect();
    }
    chart.source = d.get("source").cloned().unwrap_or(serde_json::Value::Null);
    if let Some(tracks) = d.get("tracks").and_then(|v| v.as_object()) {
        chart.tracks = tracks
            .iter()
            .map(|(t, notes)| {
                let notes: Vec<Note> = notes
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|n| {
                                let s = n.get("s")?.as_i64()?;
                                let l = n.get("l")?.as_i64()?;
                                let p = n.get("p")?.as_i64()?;
                                Some(Note {
                                    kind: n
                                        .get("k")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("normal")
                                        .to_string(),
                                    start: s,
                                    length: l,
                                    pitch: p,
                                    text: n
                                        .get("t")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    word_break: n
                                        .get("wb")
                                        .and_then(|v| v.as_i64())
                                        .map(|v| v != 0)
                                        .unwrap_or(false),
                                    alt: n
                                        .get("at")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (t.clone(), notes)
            })
            .collect();
    }
    if chart.tracks.is_empty() {
        chart.tracks.insert("P1".into(), Vec::new());
    }
    if let Some(brks) = d.get("breaks").and_then(|v| v.as_object()) {
        chart.breaks = brks
            .iter()
            .map(|(t, bs)| {
                (
                    t.clone(),
                    bs.as_array()
                        .map(|a| a.iter().filter_map(|b| b.as_i64()).collect())
                        .unwrap_or_default(),
                )
            })
            .collect();
    }
    Ok(chart)
}

pub fn save_native(chart: &Chart, path: &Path) -> std::io::Result<()> {
    std::fs::write(
        path,
        serde_json::to_string(&to_native(chart)).unwrap_or_default(),
    )
}

/// Load the canonical chart — a native .upsong when present, otherwise
/// import the .txt and cache the native form alongside it (lazy migration;
/// ignored when the directory is read-only).
pub fn load_chart(path: &Path) -> Chart {
    if path.extension().map(|e| e == "upsong").unwrap_or(false) {
        let mut chart = Chart::new();
        match std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|d| from_native(&d).ok())
        {
            Some(c) => return c,
            None => {
                let txt = path.with_extension("txt");
                if txt.exists() {
                    chart = parse_chart(&txt);
                    if chart.errors.is_empty() {
                        let _ = save_native(&chart, path);
                    }
                    return chart;
                }
                chart.errors.push("bad upsong".into());
                return chart;
            }
        }
    }
    let nat = native_path(path);
    if nat.exists() {
        let fresh = match (nat.metadata(), path.metadata()) {
            (Ok(n), Ok(p)) => n.modified().ok() >= p.modified().ok(),
            _ => false,
        };
        if fresh {
            return load_chart(&nat);
        }
    }
    let chart = parse_chart(path);
    if chart.errors.is_empty() {
        let _ = save_native(&chart, &nat);
    }
    chart
}
