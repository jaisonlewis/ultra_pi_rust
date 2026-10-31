//! Library catalog — reads the station/Pi manifest and falls back to
//! a folder scan, so a Rust app and the other frontends see the same
//! library.
//!
//! Resolution mirrors the Switch's `catalog.cpp`: manifest paths are
//! root-relative, but prepared sticks also carry the legacy flat layout
//! (`Artist - Title/` dirs + bare filenames + `charts/` dir), so every
//! path resolves through a chain of fallbacks.

use crate::layout;
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

/// A catalog song with every path resolved to disk.
#[derive(Debug, Clone)]
pub struct Song {
    pub artist: String,
    pub title: String,
    pub provider: String,
    pub source_id: String,
    pub dir: PathBuf,
    pub media: PathBuf,
    pub inst: Option<PathBuf>,
    pub chart: Option<PathBuf>,
    pub art: Option<PathBuf>,
    pub duration_sec: Option<f64>,
}

impl Song {
    /// Complete enough to sing: media + karaoke backing + a chart.
    pub fn is_complete(&self) -> bool {
        self.inst.is_some() && self.chart.is_some()
    }
}

const MEDIA_EXTS: [&str; 8] = [
    ".mp4", ".mp3", ".m4a", ".webm", ".mkv", ".ogg", ".wav", ".flac",
];
const SKIP_DIRS: [&str; 6] = [
    "incomplete", "_unsorted", "_removed", "_removed_peers",
    "charts", "chart-library",
];
const ART_NAMES: [&str; 5] = [
    "cover.jpg", "cover.png", "cover.jpeg", "folder.jpg", "artwork.jpg",
];

fn file_name(p: &Path) -> String {
    p.file_name().unwrap_or_default().to_string_lossy().to_lowercase()
}

fn stem_of(name: &str) -> &str {
    name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name)
}

/// Download fragments, pipeline intermediates, yt-dlp per-format
/// leftovers (yt-<id>.f140.m4a) — never a song.
fn is_junk(stem: &str, ext: &str) -> bool {
    if stem.contains(".part")
        || stem.contains(".temp")
        || stem.contains(".vocals")
        || ext == ".ytdl"
    {
        return true;
    }
    match stem.rfind(".f") {
        Some(p) => {
            stem.len() > p + 2 && stem.as_bytes()[p + 2].is_ascii_digit()
        }
        None => false,
    }
}

fn is_source_media(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    let name = file_name(p);
    let ext = name.rsplit_once('.').map(|(_, e)| format!(".{e}")).unwrap_or_default();
    if name.ends_with(".inst.mp4")
        || name.ends_with(".vocals.wav")
        || is_junk(stem_of(&name), &ext)
    {
        return false;
    }
    MEDIA_EXTS.iter().any(|e| name.ends_with(e))
}

fn is_inst(p: &Path) -> bool {
    file_name(p).ends_with(".inst.mp4")
}

fn list_dir(dir: &Path, dirs_only: bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|f| f.path())
                .filter(|p| {
                    if p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with('.'))
                        .unwrap_or(true)
                    {
                        return false;
                    }
                    if dirs_only {
                        p.is_dir()
                    } else {
                        p.is_file()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Pick media + instrumental from a dir, optionally pinned to one stem.
/// Mirrors the Switch's `pickMedia`.
fn pick_media(dir: &Path, want_stem: &str) -> Option<(PathBuf, Option<PathBuf>)> {
    let mut media = None;
    let mut inst = None;
    for p in list_dir(dir, false) {
        let name = file_name(&p);
        let st = stem_of(&name).to_string();
        if is_inst(&p) {
            if inst.is_none() {
                inst = Some(p);
            }
            continue;
        }
        if !is_source_media(&p) {
            continue;
        }
        if !want_stem.is_empty() && st != want_stem {
            continue;
        }
        if media.is_none() {
            media = Some(p);
        }
    }
    media.map(|m| (m, inst))
}

/// Artwork lookup: per-song sidecar first, then folder art names.
/// Mirrors the Switch's `findArt`.
fn find_art(dir: &Path, media_stem: &str) -> Option<PathBuf> {
    if !media_stem.is_empty() {
        for e in [".jpg", ".png", ".jpeg"] {
            let p = dir.join(format!("{media_stem}{e}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    for n in ART_NAMES {
        let p = dir.join(n);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Every `.upsong` under the root, indexed once per scan by stem and
/// by parent folder name. The old code re-walked the whole tree per
/// chart-less song — O(songs × dirs) — which turned a multi-thousand
/// song library rescan into minutes of grind on the Pi's USB mount.
#[derive(Default)]
pub struct ChartIndex {
    by_stem: std::collections::HashMap<String, PathBuf>,
}

fn index_charts_rec(dir: &Path, depth: u32, ix: &mut ChartIndex) {
    if depth > 4 {
        return;
    }
    for p in list_dir(dir, false) {
        if p.extension().map(|e| e == "upsong").unwrap_or(false) {
            let st = stem_of(&file_name(&p)).to_string();
            ix.by_stem.entry(st).or_insert_with(|| p.clone());
        }
    }
    for p in list_dir(dir, true) {
        index_charts_rec(&p, depth + 1, ix);
    }
}

pub fn chart_index(root: &Path) -> ChartIndex {
    let mut ix = ChartIndex::default();
    index_charts_rec(root, 0, &mut ix);
    ix
}

/// Chart resolution, mirroring the Switch's `findChart` plus the
/// manifest-declared path and the `charts/<Artist - Title>.upsong`
/// convention the prepared-stick layout uses.
fn find_chart(root: &Path, dir: &Path, media_stem: &str,
              folder_name: &str, ix: &ChartIndex) -> Option<PathBuf> {
    let p = dir.join(format!("{media_stem}.upsong"));
    if p.is_file() {
        return Some(p);
    }
    // a one-song folder holds its own chart
    for f in list_dir(dir, false) {
        if f.extension().map(|e| e == "upsong").unwrap_or(false) {
            return Some(f);
        }
    }
    // prepared sticks: charts/<Artist - Title>.upsong
    let p = root.join("charts").join(format!("{folder_name}.upsong"));
    if p.is_file() {
        return Some(p);
    }
    // anywhere under the root (charts/, chart-library/, nested dirs)
    ix.by_stem
        .get(media_stem)
        .cloned()
        .or_else(|| ix.by_stem.get(folder_name).cloned())
}

/// `Artist - Title` folder name → (artist, title). A name without the
/// separator lands entirely on title.
fn split_folder(name: &str) -> (String, String) {
    match name.find(" - ") {
        Some(pos) => (name[..pos].to_string(), name[pos + 3..].to_string()),
        None => (String::new(), name.to_string()),
    }
}

/// snake_case slug → "Title Case" for display on manifest-less sticks.
fn unslug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut cap = true;
    for mut c in s.chars() {
        if c == '_' || c == '-' {
            c = ' ';
            cap = true;
        } else if cap && c.is_ascii_lowercase() {
            c = c.to_ascii_uppercase();
            cap = false;
        } else if c != ' ' {
            cap = false;
        }
        out.push(c);
    }
    out
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

/// Resolve one manifest record to on-disk paths. Returns None when no
/// media file can be found anywhere the record points.
fn resolve_rec(root: &Path, rec: &SongRec,
               ix: &ChartIndex) -> Option<Song> {
    if rec.local_path.is_empty() {
        return None;
    }
    let folder_name = layout::song_folder(&rec.artist, &rec.title, "");
    let want_stem = stem_of(
        &Path::new(&rec.local_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
    )
    .to_string();

    // media: root-relative path first (station's two-level layout)
    let full = root.join(&rec.local_path);
    let (dir, media, mut inst) = if full.is_file() {
        let dir = full.parent().unwrap_or(root).to_path_buf();
        let inst = {
            let i = dir.join(format!("{want_stem}.inst.mp4"));
            if i.is_file() { Some(i) } else { None }
        };
        (dir, full.clone(), inst)
    } else {
        // the manifest names an exact file — trust it even when its
        // stem looks like a fragment (`.f140.m4a`, `.temp.mp4`);
        // is_junk filtering is for unguided scans only
        let base = Path::new(&rec.local_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let exact = |d: &Path| {
            let m = d.join(&base);
            if m.is_file() {
                let i = d.join(format!("{want_stem}.inst.mp4"));
                Some((d.to_path_buf(), m,
                      if i.is_file() { Some(i) } else { None }))
            } else {
                None
            }
        };
        // declared dir (nested rel_path), then the flat
        // 'Artist - Title/' stick layout
        let declared = full.parent().unwrap_or(root).to_path_buf();
        let flat = root.join(&folder_name);
        let mut picked = exact(&declared)
            .or_else(|| pick_media(&declared, &want_stem)
                .map(|(m, i)| (declared.clone(), m, i)))
            .or_else(|| exact(&flat))
            .or_else(|| pick_media(&flat, &want_stem)
                .or_else(|| pick_media(&flat, ""))
                .map(|(m, i)| (flat.clone(), m, i)));
        if picked.is_none() {
            // dir name may predate the current song_folder naming —
            // last resort: whichever one-level subdir holds the file
            for d in list_dir(root, true) {
                if let Some(p) = exact(&d) {
                    picked = Some(p);
                    break;
                }
            }
        }
        picked?
    };

    // manifest-declared inst_path can point elsewhere
    if !rec.inst_path.is_empty() {
        let p = root.join(&rec.inst_path);
        if p.is_file() {
            inst = Some(p);
        } else {
            let flat = dir.join(
                Path::new(&rec.inst_path).file_name().unwrap_or_default(),
            );
            if flat.is_file() {
                inst = Some(flat);
            }
        }
    }

    let mut chart = if rec.chart.is_empty() {
        None
    } else {
        let p = root.join(&rec.chart);
        if p.is_file() {
            Some(p)
        } else {
            let flat = root.join("charts").join(
                Path::new(&rec.chart).file_name().unwrap_or_default(),
            );
            if flat.is_file() { Some(flat) } else { None }
        }
    };
    if chart.is_none() {
        chart = find_chart(root, &dir, &want_stem, &folder_name, ix);
    }

    let art = if rec.art.is_empty() {
        None
    } else {
        let p = root.join(&rec.art);
        if p.is_file() { Some(p) } else { None }
    }
    .or_else(|| find_art(&dir, &want_stem));

    Some(Song {
        artist: rec.artist.clone(),
        title: rec.title.clone(),
        provider: rec.provider.clone(),
        source_id: rec.source_id.clone(),
        dir,
        media,
        inst,
        chart,
        art,
        duration_sec: rec.duration_sec,
    })
}

/// Fallback: walk song folders for media. `Artist - Title/` one-level
/// (prepared sticks), `artist/title/` two-level (station output) and
/// flat root files all resolve — the same set the Switch scans.
pub fn scan_folders(root: &Path) -> Vec<Song> {
    let ix = chart_index(root);
    let mut out = Vec::new();
    let song = |dir: PathBuf, artist: String, title: String| {
        let (media, inst) = pick_media(&dir, "")?;
        let mstem = stem_of(&file_name(&media)).to_string();
        let folder = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        Some(Song {
            provider: if mstem.starts_with("yt-") {
                "youtube".into()
            } else {
                "local".into()
            },
            source_id: mstem.strip_prefix("yt-").unwrap_or(&mstem).into(),
            chart: find_chart(root, &dir, &mstem, &folder, &ix),
            art: find_art(&dir, &mstem),
            artist,
            title,
            dir,
            media,
            inst,
            duration_sec: None,
        })
    };
    for adir in list_dir(root, true) {
        let aname = adir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if SKIP_DIRS.contains(&aname.as_str()) {
            continue;
        }
        // one-level 'Artist - Title/' song dir?
        if pick_media(&adir, "").is_some() {
            let (a, t) = split_folder(&aname);
            if let Some(s) = song(adir, a, t) {
                out.push(s);
            }
            continue;
        }
        // two-level artist/title/ layout
        for tdir in list_dir(&adir, true) {
            if pick_media(&tdir, "").is_none() {
                continue;
            }
            let tname = tdir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if let Some(s) = song(tdir, unslug(&aname), unslug(&tname)) {
                out.push(s);
            }
        }
    }
    // flat root media files (un-organized prepared roots)
    for p in list_dir(root, false) {
        if !is_source_media(&p) {
            continue;
        }
        let st = stem_of(&file_name(&p)).to_string();
        let (a, t) = split_folder(&st);
        out.push(Song {
            title: if t.is_empty() { st.clone() } else { t },
            artist: a,
            provider: if st.starts_with("yt-") {
                "youtube".into()
            } else {
                "local".into()
            },
            source_id: st.strip_prefix("yt-").unwrap_or(&st).into(),
            dir: root.to_path_buf(),
            media: p.clone(),
            inst: {
                let i = root.join(format!("{st}.inst.mp4"));
                if i.is_file() { Some(i) } else { None }
            },
            chart: find_chart(root, root, &st, &st, &ix),
            art: find_art(root, &st),
            duration_sec: None,
        });
    }
    out
}

/// Does `dir` hold song content? Bounded checks only — a manifest,
/// a media file at the top, or a subdir that contains one. Keeps
/// discovery from mistaking a source checkout's `ultrapistar/`
/// package dir (or any same-named folder) for a real library.
fn looks_like_library(dir: &Path) -> bool {
    if dir.join("manifest.json").is_file() {
        return true;
    }
    for p in list_dir(dir, false).into_iter().take(64) {
        if is_source_media(&p) {
            return true;
        }
    }
    for d in list_dir(dir, true).into_iter().take(48) {
        for p in list_dir(&d, false).into_iter().take(48) {
            if is_source_media(&p) {
                return true;
            }
        }
        // song dirs nest one deeper in the two-level layout
        for dd in list_dir(&d, true).into_iter().take(16) {
            for p in list_dir(&dd, false).into_iter().take(48) {
                if is_source_media(&p) {
                    return true;
                }
            }
        }
    }
    false
}

/// A mounted volume qualifies as a media stick when it carries a
/// `ultrapistar/` dir holding song content (the prepared-stick
/// convention) or an `.upsmedia` marker file — the same rule the
/// Python app uses, so a drive built for one appliance works on all
/// of them. Returns the directory to treat as the song root.
pub fn stick_root(mount: &Path) -> Option<PathBuf> {
    let inner = mount.join("ultrapistar");
    if inner.is_dir() && looks_like_library(&inner) {
        return Some(inner);
    }
    if mount.join(".upsmedia").exists() {
        return Some(mount.to_path_buf());
    }
    None
}

/// Directories that may hold removable media mounts on this platform.
/// UPS_MEDIA_DIRS adds/extends (colon/semicolon-separated list).
fn mount_bases() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Ok(extra) = std::env::var("UPS_MEDIA_DIRS") {
        for p in extra.split([':', ';']).filter(|s| !s.is_empty()) {
            v.push(PathBuf::from(p));
        }
    }
    #[cfg(unix)]
    {
        for b in ["/media", "/run/media", "/mnt", "/home"] {
            v.push(PathBuf::from(b));
        }
    }
    #[cfg(windows)]
    {
        for c in 'D'..='Z' {
            let d = PathBuf::from(format!("{c}:\\"));
            if d.exists() {
                v.push(d);
            }
        }
    }
    v
}

/// Every media stick found on mounted volumes: `/media/<u>/<label>`,
/// `/run/media/<u>/<label>`, `/mnt/<label>`, `D:\` … — and for
/// user-level dirs one more level down (`/media/jaison/STICK`).
/// Multiple drives all count — the library is their union.
/// Order is mount scan order (stable); UPS_MEDIA_DIR short-circuits.
pub fn find_sticks() -> Vec<PathBuf> {
    if let Ok(p) = std::env::var("UPS_MEDIA_DIR") {
        if !p.is_empty() {
            let p = PathBuf::from(p);
            return vec![stick_root(&p).unwrap_or(p)];
        }
    }
    let mut found: Vec<PathBuf> = Vec::new();
    let mut mounts: Vec<PathBuf> = Vec::new();
    for base in mount_bases() {
        if let Some(p) = stick_root(&base) {
            found.push(p);
        }
        let Ok(rd) = std::fs::read_dir(&base) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir()
                || p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with('.'))
                    .unwrap_or(true)
            {
                continue;
            }
            mounts.push(p);
        }
    }
    // one level down — /media/<user>/<label>, /run/media/<user>/<label>
    for m in mounts {
        if let Some(p) = stick_root(&m) {
            found.push(p);
            continue;
        }
        if let Ok(rd) = std::fs::read_dir(&m) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Some(s) = stick_root(&p) {
                        found.push(s);
                    }
                }
            }
        }
    }
    found
}

/// Back-compat single-root lookup — the first discovered stick.
pub fn find_stick() -> Option<PathBuf> {
    find_sticks().into_iter().next()
}

/// Resolve the effective media roots, in order:
///   1. `--root` aimed at a mountpoint descends into its `ultrapistar/`
///   2. every discovered stick joins the library — multiple pendrives
///      union (the Python app's `media_dir` + `media_extra_dirs`
///      semantics: first root is the primary, others read alongside)
///   3. `--root` verbatim always rides along as the static root
///      (UPS_NO_USB=1 skips discovery, making it fully authoritative)
pub fn resolve_roots(explicit: &Path) -> Vec<PathBuf> {
    let explicit = stick_root(explicit).unwrap_or_else(|| explicit.to_path_buf());
    let no_usb = std::env::var("UPS_NO_USB")
        .map(|v| v == "1")
        .unwrap_or(false);
    let mut roots: Vec<PathBuf> = Vec::new();
    if no_usb {
        roots.push(explicit);
    } else {
        roots.extend(find_sticks());
        // the explicit root joins the union only when it carries song
        // content — a bare cwd (e.g. `ups-app --tv` from the repo dir)
        // must not drag the whole working tree into the scan. With no
        // sticks at all it stays as the fallback/write target.
        if roots.is_empty() || looks_like_library(&explicit) {
            if !roots.contains(&explicit) {
                roots.push(explicit);
            }
        }
    }
    roots
}

/// Back-compat: the first resolved root.
pub fn resolve_root(explicit: &Path) -> PathBuf {
    resolve_roots(explicit)
        .into_iter()
        .next()
        .unwrap_or_else(|| explicit.to_path_buf())
}

/// Manifest records plus any folders the manifest doesn't know about —
/// a stick built by folder copy has no manifest at all, and folders
/// dropped onto a manifest-managed root still count (the manifest may
/// lag the filesystem). Deduped by resolved media path so a folder
/// whose media a manifest record already claims isn't listed twice.
pub fn scan(root: &Path) -> Vec<Song> {
    let recs = read_manifest(root);
    if recs.is_empty() {
        return scan_folders(root);
    }
    let ix = chart_index(root);
    let mut songs: Vec<Song> =
        recs.iter().filter_map(|r| resolve_rec(root, r, &ix)).collect();
    if songs.is_empty() {
        return scan_folders(root);
    }
    let seen: std::collections::HashSet<PathBuf> =
        songs.iter().map(|s| s.media.clone()).collect();
    songs.extend(
        scan_folders(root)
            .into_iter()
            .filter(|s| !seen.contains(&s.media)),
    );
    songs
}

/// Union scan across several roots — multiple pendrives count as one
/// library. A song duplicated on two sticks (same provider id, or
/// same normalized artist+title) lists once; the earlier root wins.
pub fn scan_roots(roots: &[PathBuf]) -> Vec<Song> {
    let mut out: Vec<Song> = Vec::new();
    let mut seen_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut seen_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for s in scan(root) {
            let name_key = format!(
                "{}\u{0}{}",
                s.artist.to_lowercase(),
                s.title.to_lowercase()
            );
            if !s.source_id.is_empty()
                && !seen_ids.insert(s.source_id.clone())
            {
                continue;
            }
            if !seen_names.insert(name_key) {
                continue;
            }
            out.push(s);
        }
    }
    out
}
