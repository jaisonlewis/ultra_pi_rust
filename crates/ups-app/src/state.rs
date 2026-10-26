//! In-memory app state — catalog entries, performers, queue, history,
//! browser sessions. Replaces the Python app's SQLite layer for the
//! party loop; persists to `ups-state.json` for restart recovery.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ups_core::catalog;

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One playable catalog row (a manifest song): entry_id is 1-based
/// manifest order — stable across scans of an unchanged manifest.
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
}

pub struct State {
    pub root: PathBuf,
    pub host_key: String,
    pub build_v: String,
    pub difficulty: i64,
    pub vocal_cut_default: bool,
    pub intermission_sec: f64,
    pub entries: Vec<Entry>,
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
        let entries = catalog::scan(&root)
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let id = (i + 1) as i64;
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
                    artist: s.artist,
                    title: s.title,
                    language: String::new(),
                    provider: s.provider,
                    source_id: s.source_id,
                    duration_sec: s.duration_sec,
                    media: s.media,
                    inst: s.inst,
                    chart: s.chart,
                    art: s.art,
                    is_duet,
                    tracks,
                    chart_alt,
                    audio_only,
                }
            })
            .collect();
        let mut st = State {
            root,
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
    }

    pub fn persist(&self) {
        let p = Persisted {
            performers: self.performers.lock().unwrap().clone(),
            queue: self.queue.lock().unwrap().clone(),
            history: self.history.lock().unwrap().clone(),
            next_performer: *self.next_performer.lock().unwrap(),
            next_queue_item: *self.next_queue_item.lock().unwrap(),
            host_performer_id: *self.host_performer_id.lock().unwrap(),
        };
        let tmp = self.state_path.with_extension("tmp");
        if let Ok(s) = serde_json::to_string(&p) {
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &self.state_path);
            }
        }
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
        let entry = self.entries.iter().find(|e| e.id == entry_id);
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
            song_id: entry.map(|e| e.song_id).unwrap_or(entry_id),
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
        let e = self.entries.iter().find(|e| e.id == it.entry_id);
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
            "artist": e.map(|e| e.artist.clone()).unwrap_or_default(),
            "title": e.map(|e| e.title.clone()).unwrap_or_default(),
            "is_duet": e.map(|e| e.is_duet).unwrap_or(false),
            "provider": e.map(|e| e.provider.clone()).unwrap_or_default(),
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
