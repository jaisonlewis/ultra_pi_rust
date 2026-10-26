//! Library catalog — reads the station/Pi manifest and falls back to
//! the two-level `artist/title/` folder scan the Switch uses, so a
//! Rust app and the other frontends see the same library.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct SongRec {
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub local_path: String,
    #[serde(default)]
    pub inst_path: String,
    #[serde(default)]
    pub art: String,
    #[serde(default)]
    pub chart: String,
    #[serde(default)]
    pub duration_sec: Option<f64>,
    #[serde(default)]
    pub provenance: String,
}

impl SongRec {
    /// True when the cluster a record points at is complete enough to
    /// sing: media + karaoke backing + a chart.
    pub fn is_complete(&self, root: &Path) -> bool {
        let media_ok = !self.local_path.is_empty()
            && root.join(&self.local_path).is_file();
        let inst_ok = !self.inst_path.is_empty()
            && root.join(&self.inst_path).is_file();
        media_ok && inst_ok
    }
    pub fn media_path(&self, root: &Path) -> PathBuf {
        root.join(&self.local_path)
    }
    pub fn inst_media(&self, root: &Path) -> PathBuf {
        root.join(&self.inst_path)
    }
    pub fn chart_path(&self, root: &Path) -> Option<PathBuf> {
        if self.chart.is_empty() {
            None
        } else {
            Some(root.join(&self.chart))
        }
    }
}

const MEDIA_EXTS: [&str; 5] = [".mp4", ".mp3", ".m4a", ".webm", ".mkv"];
const SKIP_DIRS: [&str; 4] = ["incomplete", "_unsorted", "_removed", "_removed_peers"];

fn is_source_media(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    let name = match p.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_lowercase(),
        None => return false,
    };
    if name.contains(".inst.") || name.ends_with(".vocals.wav") {
        return false;
    }
    // yt-dlp per-format fragments: yt-<id>.f140.m4a
    let stem = name.rsplitn(2, '.').nth(1).unwrap_or("");
    if stem.contains(".f") && stem.rsplit(".f").next().map(|s| s.chars().all(|c| c.is_ascii_digit())).unwrap_or(false) {
        return false;
    }
    MEDIA_EXTS.iter().any(|e| name.ends_with(e))
}

/// Read `<root>/manifest.json` (station/Pi catalog format).
pub fn read_manifest(root: &Path) -> Vec<SongRec> {
    let raw = match std::fs::read_to_string(root.join("manifest.json")) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = if let Some(obj) = v.get("songs") {
        obj.clone()
    } else {
        v
    };
    serde_json::from_value(arr).unwrap_or_default()
}

/// Fallback: walk `<artist>/<title>/` folders for media (what the
/// Switch does when no manifest exists). Skips scratch/quarantine
/// subtrees.
pub fn scan_folders(root: &Path) -> Vec<SongRec> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for artist in rd.flatten() {
        let adir = artist.path();
        if !adir.is_dir() {
            continue;
        }
        let aname = artist.file_name().to_string_lossy().to_string();
        if SKIP_DIRS.contains(&aname.as_str()) {
            continue;
        }
        let Ok(td) = std::fs::read_dir(&adir) else {
            continue;
        };
        for title in td.flatten() {
            let tdir = title.path();
            if !tdir.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&tdir) else {
                continue;
            };
            let mut media: Option<PathBuf> = None;
            let mut chart: Option<PathBuf> = None;
            for f in files.flatten() {
                let p = f.path();
                if is_source_media(&p) && media.is_none() {
                    media = Some(p.clone());
                }
                if p.extension().map(|e| e == "upsong").unwrap_or(false) && chart.is_none() {
                    chart = Some(p);
                }
            }
            if let Some(m) = media {
                let tname = title.file_name().to_string_lossy().to_string();
                let stem = m.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let rel = |p: &Path| {
                    p.strip_prefix(root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .replace('\\', "/")
                };
                let inst = m.with_file_name(format!("{}.inst.mp4", stem));
                out.push(SongRec {
                    artist: aname.replace('_', " "),
                    title: tname.replace('_', " "),
                    provider: if stem.starts_with("yt-") {
                        "youtube".into()
                    } else {
                        "local".into()
                    },
                    source_id: stem.strip_prefix("yt-").unwrap_or(&stem).to_string(),
                    local_path: rel(&m),
                    inst_path: if inst.is_file() { rel(&inst) } else { String::new() },
                    art: {
                        let j = m.with_file_name(format!("{}.jpg", stem));
                        if j.is_file() { rel(&j) } else { String::new() }
                    },
                    chart: chart.map(|c| rel(&c)).unwrap_or_default(),
                    duration_sec: None,
                    provenance: "folder-scan".into(),
                });
            }
        }
    }
    out
}

/// Manifest records if present, else the folder scan — the same order
/// the Pi/Switch resolve a library.
pub fn scan(root: &Path) -> Vec<SongRec> {
    let m = read_manifest(root);
    if !m.is_empty() {
        return m;
    }
    scan_folders(root)
}
