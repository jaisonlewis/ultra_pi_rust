//! In-memory app state — catalog entries, performers, queue, history,
//! browser sessions. Replaces the Python app's SQLite layer for the
//! party loop; persists to `ups-state.json` for restart recovery.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ups_core::catalog;

/// "Artist - Title" split — the filename/folder convention used by
/// uploads and chart-library matching.
pub fn split_artist_title(s: &str) -> (String, String) {
    match s.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (String::new(), s.trim().to_string()),
    }
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One playable catalog row (a manifest song): entry_id is 1-based
/// manifest order — stable across scans of an unchanged manifest.
#[derive(Clone)]
pub struct Entry {
    pub id: i64,
    pub song_id: i64,
    pub artist: String,
    pub title: String,
    pub language: String,
    pub provider: String,
    pub source_id: String,
    pub duration_sec: Option<f64>,
    pub media: PathBuf,
    pub inst: Option<PathBuf>,
    pub chart: Option<PathBuf>,
    pub art: Option<PathBuf>,
    pub is_duet: bool,
    pub tracks: Vec<String>,
    pub chart_alt: bool,
    pub audio_only: bool,
    /// chart file mtime at last parse — lets rescan skip load_chart
    /// for the (overwhelmingly common) unchanged-chart case
    chart_mtime: Option<u64>,
}

impl Entry {
    /// Row shape the phone UI renders (catalog.search() port).
    pub fn row(&self, top: Option<&TopScore>) -> Value {
        json!({
            "id": self.song_id,
            "entry_id": self.id,
            "artist": self.artist,
            "title": self.title,
            "language": self.language,
            "is_duet": self.is_duet,
            "tracks": self.tracks,
            "provider": self.provider,
            "source_id": self.source_id,
            "status": "available",
            "duration_sec": self.duration_sec,
            "thumbnail": if self.art.is_some() {
                Value::from(format!("/api/art/{}.jpg", self.id))
            } else { Value::Null },
            "provenance": "station",
            "inst_path": self.inst.as_ref().map(|_| "x"),
            "chart_verified": self.chart.is_some() as i64,
            "scoring_available": self.chart.is_some(),
            "chart_alt": self.chart_alt,
            "chart_duet": self.is_duet,
            "possible_chart": false,
            "karaoke": true,
            "in_library": true,
            "top_score": top.map(|t| t.score),
            "top_scorer": top.map(|t| t.scorer.clone()),
        })
    }
}

fn chart_mtime(p: &Option<PathBuf>) -> Option<u64> {
    p.as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// catalog::Song → Entry — loads the chart for duet/alt flags and
/// tags audio-only media. Shared by the boot scan and hot rescan.
fn entry_of(s: &catalog::Song, id: i64) -> Entry {
    let (is_duet, tracks, chart_alt) = s
        .chart
        .as_ref()
        .map(|p| {
            let c = ups_core::chart::load_chart(p);
            (
                c.is_duet(),
                c.tracks.keys().cloned().collect::<Vec<_>>(),
                c.tracks
                    .values()
                    .flatten()
                    .any(|n| !n.alt.is_empty()),
            )
        })
        .unwrap_or((false, vec!["P1".to_string()], false));
    let name = s
        .media
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let audio_only = ["mp3", "m4a", "ogg", "wav", "flac"]
        .iter()
        .any(|e| name.ends_with(e));
    Entry {
        id,
        song_id: id,
        artist: s.artist.clone(),
        title: s.title.clone(),
        language: String::new(),
        provider: s.provider.clone(),
        source_id: s.source_id.clone(),
        duration_sec: s.duration_sec,
        media: s.media.clone(),
        inst: s.inst.clone(),
        chart: s.chart.clone(),
        art: s.art.clone(),
        is_duet,
        tracks,
        chart_alt,
        chart_mtime: chart_mtime(&s.chart),
        audio_only,
    }
}

/// What a rescan changed — drives the host toast + state broadcast.
#[derive(Default)]
pub struct Rescan {
    pub added: usize,
    pub removed: usize,
    pub updated: usize,
    /// entries that gained a chart or karaoke track this scan
    pub upgraded: usize,
    pub total: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Performer {
    pub id: i64,
    pub name: String,
    pub color: String,
}

const PALETTE: [&str; 8] = [
    "#ff5a5a", "#4da3ff", "#ffd24d", "#5ad17a", "#c792ff", "#ff9e64",
    "#4dd0e1", "#f47fb8",
];

#[derive(Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: i64,
    pub status: String, // queued | playing | done | skipped | error
    pub position: f64,
    pub part_choice: String,
    pub created_at: f64,
    pub song_id: i64,
    pub entry_id: i64,
    pub performer_id: i64,
    pub performer2_id: Option<i64>,
    pub vocal_cut: Option<i64>,
    pub lyrics_rom: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HistoryRow {
    pub queue_item_id: Option<i64>,
    pub song_id: i64,
    pub performer_id: i64,
    pub performer2_id: Option<i64>,
    pub played_at: f64,
    pub score: i64,
}

#[derive(Default, Clone)]
pub struct SessionCookie {
    pub performer_id: Option<i64>,
    pub is_host: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    performers: Vec<Performer>,
    queue: Vec<QueueItem>,
    history: Vec<HistoryRow>,
    next_performer: i64,
    next_queue_item: i64,
    host_performer_id: Option<i64>,
    /// entry_id -> learned chart->media nudge (host sync buttons)
    #[serde(default)]
    sync_offsets: BTreeMap<i64, f64>,
    #[serde(default)]
    autosync_mic: Option<bool>,
    #[serde(default)]
    output_target: Option<String>,
}

pub struct State {
    root: RwLock<PathBuf>,
    /// the --root arg (or cwd) as passed — re-resolved each rescan so
    /// a newly-mounted stick can take over mid-run
    explicit_root: PathBuf,
    pub host_key: String,
    pub build_v: String,
    pub difficulty: i64,
    pub vocal_cut_default: bool,
    pub intermission_sec: f64,
    pub entries: RwLock<Vec<Entry>>,
    pub performers: Mutex<Vec<Performer>>,
    pub queue: Mutex<Vec<QueueItem>>,
    pub history: Mutex<Vec<HistoryRow>>,
    pub sessions: Mutex<BTreeMap<String, SessionCookie>>,
    pub host_performer_id: Mutex<Option<i64>>,
    next_performer: Mutex<i64>,
    next_queue_item: Mutex<i64>,
    state_path: PathBuf,
    /// item_id -> epoch when its countdown ends
    pub play_at: Mutex<BTreeMap<i64, f64>>,
    /// entry_id -> chart->media nudge (persists across restarts)
    pub sync_offsets: Mutex<BTreeMap<i64, f64>>,
    /// pitch-solve autosync toggle (GET/POST /playback/autosync)
    pub autosync_mic: Mutex<bool>,
    /// saved audio output (sink_name or card json) applied at boot
    pub output_target: Mutex<Option<String>>,
}

#[derive(Clone)]
pub struct TopScore {
    pub score: i64,
    pub scorer: String,
}

impl State {
    pub fn new(root: PathBuf, host_key: String, build_v: String,
               difficulty: i64, vocal_cut: bool, intermission_sec: f64,
               state_path: PathBuf) -> Self {
        let resolved = catalog::resolve_root(&root);
        let entries = RwLock::new(
            catalog::scan(&resolved)
                .iter()
                .enumerate()
                .map(|(i, s)| entry_of(s, (i + 1) as i64))
                .collect::<Vec<_>>(),
        );
        let mut st = State {
            root: RwLock::new(resolved),
            explicit_root: root,
            host_key,
            build_v,
            difficulty,
            vocal_cut_default: vocal_cut,
            intermission_sec,
            entries,
            performers: Mutex::new(Vec::new()),
            queue: Mutex::new(Vec::new()),
            history: Mutex::new(Vec::new()),
            sessions: Mutex::new(BTreeMap::new()),
            host_performer_id: Mutex::new(None),
            next_performer: Mutex::new(1),
            next_queue_item: Mutex::new(1),
            state_path,
            play_at: Mutex::new(BTreeMap::new()),
            sync_offsets: Mutex::new(BTreeMap::new()),
            autosync_mic: Mutex::new(true),
            output_target: Mutex::new(None),
        };
        st.restore();
        st
    }

    fn restore(&mut self) {
        let Ok(raw) = std::fs::read_to_string(&self.state_path) else {
            return;
        };
        let Ok(p) = serde_json::from_str::<Persisted>(&raw) else {
            return;
        };
        // a 'playing' item at boot was interrupted mid-song — demote it
        // so the queue resumes rather than stranding the TV
        let mut queue = p.queue;
        for it in queue.iter_mut() {
            if it.status == "playing" {
                it.status = "queued".into();
            }
        }
        *self.performers.get_mut().unwrap() = p.performers;
        *self.queue.get_mut().unwrap() = queue;
        *self.history.get_mut().unwrap() = p.history;
        *self.next_performer.get_mut().unwrap() = p.next_performer.max(1);
        *self.next_queue_item.get_mut().unwrap() = p.next_queue_item.max(1);
        *self.host_performer_id.get_mut().unwrap() = p.host_performer_id;
        *self.sync_offsets.get_mut().unwrap() = p.sync_offsets;
        if let Some(a) = p.autosync_mic {
            *self.autosync_mic.get_mut().unwrap() = a;
        }
        *self.output_target.get_mut().unwrap() = p.output_target;
    }

    pub fn persist(&self) {
        let p = Persisted {
            performers: self.performers.lock().unwrap().clone(),
            queue: self.queue.lock().unwrap().clone(),
            history: self.history.lock().unwrap().clone(),
            next_performer: *self.next_performer.lock().unwrap(),
            next_queue_item: *self.next_queue_item.lock().unwrap(),
            host_performer_id: *self.host_performer_id.lock().unwrap(),
            sync_offsets: self.sync_offsets.lock().unwrap().clone(),
            autosync_mic: Some(*self.autosync_mic.lock().unwrap()),
            output_target: self.output_target.lock().unwrap().clone(),
        };
        let tmp = self.state_path.with_extension("tmp");
        if let Ok(s) = serde_json::to_string(&p) {
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &self.state_path);
            }
        }
    }

    /// The effective media root (may point at a USB stick).
    pub fn root(&self) -> PathBuf {
        self.root.read().unwrap().clone()
    }

    /// Re-check whether a better media root appeared — a stick
    /// mounted since boot, or the current root's mount pulled. The
    /// same resolution rule as startup: `ultrapistar/` descend,
    /// discovered stick wins, explicit --root is the fallback.
    /// Swaps `root` when the answer changes; callers rescan after.
    pub fn refresh_root(&self) -> bool {
        let next = catalog::resolve_root(&self.explicit_root);
        let mut root = self.root.write().unwrap();
        if next != *root {
            eprintln!("media root: {} -> {}", root.display(),
                      next.display());
            *root = next;
            true
        } else {
            false
        }
    }

    /// Re-walk the media root and merge changes in place — new song
    /// folders appear, removed ones drop, and songs that gained an
    /// inst/chart/art sidecar upgrade without losing their entry id
    /// (so the queue keeps pointing at the right song).
    pub fn rescan(&self) -> Rescan {
        self.refresh_root();
        let songs = catalog::scan(&self.root());
        let mut entries = self.entries.write().unwrap();
        let mut r = Rescan {
            total: songs.len(),
            ..Default::default()
        };
        let mut next_id =
            entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
        let mut keep: HashSet<PathBuf> = HashSet::with_capacity(songs.len());
        for s in &songs {
            keep.insert(s.media.clone());
            match entries.iter_mut().find(|e| e.media == s.media) {
                Some(e) => {
                    // cheap path: same chart file, unchanged mtime — keep
                    // the parsed flags and only refresh path-level fields.
                    // 400 chart parses per 15s tick starved the Pi's audio.
                    if e.chart == s.chart && e.chart_mtime == chart_mtime(&s.chart)
                    {
                        let same = e.artist == s.artist
                            && e.title == s.title
                            && e.duration_sec == s.duration_sec
                            && e.inst == s.inst
                            && e.art == s.art
                            && e.provider == s.provider
                            && e.source_id == s.source_id;
                        if !same {
                            e.artist = s.artist.clone();
                            e.title = s.title.clone();
                            e.duration_sec = s.duration_sec;
                            e.inst = s.inst.clone();
                            e.art = s.art.clone();
                            e.provider = s.provider.clone();
                            e.source_id = s.source_id.clone();
                            r.updated += 1;
                        }
                        continue;
                    }
                    let fresh = entry_of(s, e.id);
                    if e.chart.is_none() && fresh.chart.is_some()
                        || e.inst.is_none() && fresh.inst.is_some()
                    {
                        r.upgraded += 1;
                    }
                    let same = e.artist == fresh.artist
                        && e.title == fresh.title
                        && e.duration_sec == fresh.duration_sec
                        && e.inst == fresh.inst
                        && e.chart == fresh.chart
                        && e.art == fresh.art
                        && e.chart_alt == fresh.chart_alt
                        && e.is_duet == fresh.is_duet
                        && e.tracks == fresh.tracks;
                    if !same {
                        *e = fresh;
                        r.updated += 1;
                    } else {
                        // same entry, new chart mtime — stamp it so the
                        // 15s tick doesn't re-parse this chart forever
                        e.chart_mtime = fresh.chart_mtime;
                    }
                }
                None => {
                    entries.push(entry_of(s, next_id));
                    next_id += 1;
                    r.added += 1;
                }
            }
        }
        let before = entries.len();
        entries.retain(|e| keep.contains(&e.media));
        r.removed = before - entries.len();
        r
    }

    /// Flip the romanized-lyrics flag on a queue item (host toggle).
    pub fn set_item_rom(&self, item_id: i64, rom: bool) -> bool {
        let mut q = self.queue.lock().unwrap();
        if let Some(it) = q.iter_mut().find(|i| i.id == item_id) {
            it.lyrics_rom = Some(rom as i64);
            return true;
        }
        false
    }

    // -- sync / settings ----------------------------------------------------
    /// Learned chart->media nudge for an entry (0 when untouched).
    pub fn sync_offset(&self, entry_id: i64) -> f64 {
        self.sync_offsets
            .lock()
            .unwrap()
            .get(&entry_id)
            .copied()
            .unwrap_or(0.0)
    }

    pub fn set_sync_offset(&self, entry_id: i64, off_ms: f64) {
        self.sync_offsets
            .lock()
            .unwrap()
            .insert(entry_id, off_ms);
        self.persist();
    }

    pub fn set_autosync_mic(&self, on: bool) {
        *self.autosync_mic.lock().unwrap() = on;
        self.persist();
    }

    // -- performers ---------------------------------------------------------
    pub fn add_performer(&self, name: &str) -> Performer {
        let mut perfs = self.performers.lock().unwrap();
        // same-name reuse keeps a refresh from minting a new singer
        if let Some(p) = perfs.iter().find(|p| p.name == name) {
            return p.clone();
        }
        let mut n = self.next_performer.lock().unwrap();
        let p = Performer {
            id: *n,
            name: name.to_string(),
            color: PALETTE[(*n as usize - 1) % PALETTE.len()].to_string(),
        };
        *n += 1;
        perfs.push(p.clone());
        drop(perfs);
        drop(n);
        self.persist();
        p
    }

    pub fn performer(&self, id: i64) -> Option<Performer> {
        self.performers.lock().unwrap().iter().find(|p| p.id == id).cloned()
    }

    // -- queue --------------------------------------------------------------
    pub fn queue_add(&self, entry_id: i64, performer_id: i64,
                     performer2_id: Option<i64>, part_choice: String,
                     vocal_cut: Option<i64>,
                     lyrics_rom: Option<i64>) -> i64 {
        let entry = self.entries.read().unwrap()
            .iter().find(|e| e.id == entry_id).map(|e| e.song_id);
        let mut q = self.queue.lock().unwrap();
        let mut n = self.next_queue_item.lock().unwrap();
        let top = q
            .iter()
            .filter(|i| i.status == "queued")
            .map(|i| i.position)
            .fold(0.0f64, f64::max);
        let item = QueueItem {
            id: *n,
            status: "queued".into(),
            position: top + 1.0,
            part_choice,
            created_at: now_secs(),
            song_id: entry.unwrap_or(entry_id),
            entry_id,
            performer_id,
            performer2_id,
            vocal_cut,
            lyrics_rom,
        };
        *n += 1;
        q.push(item);
        let id = q.last().unwrap().id;
        drop(q);
        drop(n);
        self.persist();
        id
    }

    pub fn now_playing(&self) -> Option<QueueItem> {
        self.queue
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.status == "playing")
            .cloned()
    }

    pub fn start_next(&self) -> Option<QueueItem> {
        {
            let mut q = self.queue.lock().unwrap();
            if q.iter().any(|i| i.status == "playing") {
                drop(q);
                return self.now_playing();
            }
            let idx = q
                .iter()
                .enumerate()
                .filter(|(_, i)| i.status == "queued")
                .min_by(|a, b| a.1.position.partial_cmp(&b.1.position).unwrap())
                .map(|(i, _)| i);
            let Some(idx) = idx else { return None };
            q[idx].status = "playing".into();
        }
        self.persist();
        self.now_playing()
    }

    pub fn finish_current(&self, outcome: &str, score: Option<i64>,
                          perf2: Option<i64>) -> Option<QueueItem> {
        {
            let mut q = self.queue.lock().unwrap();
            if let Some(cur) = q.iter_mut().find(|i| i.status == "playing") {
                cur.status = outcome.into();
                if let Some(score) = score {
                    self.history.lock().unwrap().push(HistoryRow {
                        queue_item_id: Some(cur.id),
                        song_id: cur.song_id,
                        performer_id: cur.performer_id,
                        performer2_id: cur.performer2_id.or(perf2),
                        played_at: now_secs(),
                        score,
                    });
                }
            }
        }
        self.persist();
        self.start_next()
    }

    pub fn queue_list(&self) -> Vec<QueueItem> {
        let mut items: Vec<QueueItem> = self
            .queue
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.status == "queued" || i.status == "playing")
            .cloned()
            .collect();
        items.sort_by(|a, b| {
            (b.status == "playing")
                .cmp(&(a.status == "playing"))
                .then(a.position.partial_cmp(&b.position).unwrap())
        });
        items
    }

    /// Queue item joined with entry + performer fields — the ITEM_SQL row.
    pub fn item_row(&self, it: &QueueItem) -> Value {
        let e = {
            let entries = self.entries.read().unwrap();
            entries.iter().find(|e| e.id == it.entry_id).map(|e| (
                e.artist.clone(), e.title.clone(), e.is_duet,
                e.provider.clone(),
            ))
        };
        let p1 = self.performer(it.performer_id);
        let p2 = it.performer2_id.and_then(|id| self.performer(id));
        json!({
            "id": it.id,
            "status": it.status,
            "position": it.position,
            "part_choice": it.part_choice,
            "created_at": it.created_at,
            "song_id": it.song_id,
            "performer_id": it.performer_id,
            "performer2_id": it.performer2_id,
            "vocal_cut": it.vocal_cut,
            "lyrics_rom": it.lyrics_rom,
            "artist": e.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
            "title": e.as_ref().map(|f| f.1.clone()).unwrap_or_default(),
            "is_duet": e.as_ref().map(|f| f.2).unwrap_or(false),
            "provider": e.as_ref().map(|f| f.3.clone()).unwrap_or_default(),
            "entry_id": it.entry_id,
            "entry_status": "available",
            "singer": p1.as_ref().map(|p| p.name.clone()),
            "singer_color": p1.as_ref().map(|p| p.color.clone()),
            "singer2": p2.as_ref().map(|p| p.name.clone()),
            "singer2_color": p2.as_ref().map(|p| p.color.clone()),
        })
    }

    /// queue.snapshot() port — {now_playing, queue, queue_count}.
    pub fn snapshot(&self) -> Value {
        let items = self.queue_list();
        let playing = items.iter().find(|i| i.status == "playing");
        let queued: Vec<Value> = items
            .iter()
            .filter(|i| i.status == "queued")
            .map(|i| self.item_row(i))
            .collect();
        json!({
            "now_playing": playing.map(|i| self.item_row(i)),
            "queue": queued,
            "queue_count": queued.len() + playing.is_some() as usize,
            "song_count": self.entries.read().unwrap().len(),
            "library_root": self.root().to_string_lossy(),
        })
    }

    pub fn top_scores(&self) -> BTreeMap<i64, TopScore> {
        let perfs = self.performers.lock().unwrap();
        let name_of = |id: i64| {
            perfs
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .unwrap_or_default()
        };
        let mut out: BTreeMap<i64, TopScore> = BTreeMap::new();
        for h in self.history.lock().unwrap().iter() {
            if h.score <= 0 {
                continue;
            }
            let scorer = match h.performer2_id {
                Some(p2) => format!("{} & {}", name_of(h.performer_id),
                                    name_of(p2)),
                None => name_of(h.performer_id),
            };
            let e = out.entry(h.song_id).or_insert(TopScore {
                score: 0,
                scorer: String::new(),
            });
            if h.score > e.score {
                e.score = h.score;
                e.scorer = scorer;
            }
        }
        out
    }
}
