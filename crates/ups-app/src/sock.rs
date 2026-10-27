//! Socket.IO layer — port of web/sockets.py. Handlers relay the
//! browser <video> clock into the sing session and broadcast state.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{json, Value};
use socketioxide::extract::{Data, SocketRef};
use socketioxide::SocketIo;

use ups_core::chart;
use ups_core::session::Session;

use crate::mic::MicSession;
use crate::state::{now_secs, Entry, QueueItem, State};

pub struct SingCtx {
    pub session: Session,
    _mic: Option<MicSession>,
    pub tones: std::sync::Arc<Mutex<BTreeMap<String, Option<i64>>>>,
    pub last_ms: f64,
}

pub struct App {
    pub st: State,
    pub io: SocketIo,
    pub join_url: String,
    pub sing: Mutex<Option<SingCtx>>,
    pub tv_sid: Mutex<Option<String>>,
    pub music_volume: Mutex<f64>,
    pub mic_delay_ms: Mutex<f64>,
    pub auto_next: Mutex<f64>,
    /// native TV renderer present → don't wait on a browser
    pub native_tv: bool,
    pub paused: std::sync::atomic::AtomicBool,
    /// pw-loopback speaker monitor — None off-PipeWire hosts
    pub monitor: Option<crate::monitor::Monitor>,
}

impl ups_tv::Driver for App {
    /// Queue item → PlayItem; mirrors now_playing_payload's resolution
    /// (instrumental preference, audio-only flag).
    fn current(&self) -> Option<ups_tv::PlayItem> {
        let it = self.st.now_playing().or_else(|| self.st.start_next())?;
        let e = {
            let entries = self.st.entries.read().unwrap();
            entries.iter().find(|e| e.id == it.entry_id).cloned()
        };
        let e = e.as_ref()?;
        let want_cut = it
            .vocal_cut
            .map(|v| v != 0)
            .unwrap_or(self.st.vocal_cut_default);
        let audio = if want_cut {
            e.inst.clone().unwrap_or_else(|| e.media.clone())
        } else {
            e.media.clone()
        };
        // the karaoke mp4 carries the video too — decode frames from the
        // SAME file that supplies the audio clock so they can't drift
        let video = if e.audio_only {
            None
        } else {
            Some(audio.clone())
        };
        let p1 = self.st.performer(it.performer_id);
        let p2 = it.performer2_id.and_then(|id| self.st.performer(id));
        let singer = match (p1, p2) {
            (Some(a), Some(b)) => format!("{} & {}", a.name, b.name),
            (Some(a), None) => a.name,
            _ => String::new(),
        };
        Some(ups_tv::PlayItem {
            id: it.id,
            audio,
            video,
            chart: e.chart.clone(),
            artist: e.artist.clone(),
            title: e.title.clone(),
            audio_only: e.audio_only,
            singer,
            tracks: e.tracks.clone(),
        })
    }

    fn emit(&self, event: &str, data: &Value) {
        let _ = self.io.broadcast().emit(event, data);
    }

    fn item_done(&self, _id: i64, score: i64) {
        let _ = self.io.broadcast().emit(
            "sing:final",
            &json!({"tracks": {}, "score": score}),
        );
        self.st.finish_current("done", Some(score), None);
        self.persist_and_push();
    }

    fn item_error(&self, id: i64, _msg: &str) {
        let _ = self.io.broadcast().emit(
            "toast",
            &json!({"kind":"warn","text":"media error — next"}),
        );
        self.st.finish_current("error", None, None);
        let _ = id;
        self.persist_and_push();
    }

    fn paused(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn volume(&self) -> f64 {
        *self.music_volume.lock().unwrap()
    }

    fn play_at(&self) -> f64 {
        let Some(it) = self.st.now_playing() else { return 0.0 };
        let mut m = self.st.play_at.lock().unwrap();
        *m.entry(it.id)
            .or_insert_with(|| {
                crate::state::now_secs() + self.st.intermission_sec
            })
    }

    fn idle_info(&self) -> (String, String) {
        (self.join_url.clone(), self.join_url.clone())
    }
}

impl App {
    fn persist_and_push(&self) {
        self.st.persist();
        let _ = self.io.broadcast().emit("state", &self.st.snapshot());
    }
}

/// push_now_playing() port — end the old session, promote the next
/// queued item if idle, build the playback+scoring payload, broadcast.
pub async fn push_now_playing(app: &App) {
    app.sing.lock().unwrap().take(); // drop stops mic capture
    let item = app
        .st
        .now_playing()
        .or_else(|| app.st.start_next());
    let payload = item
        .as_ref()
        .map(|it| now_playing_payload(app, it))
        .unwrap_or(Value::Null);
    let _ = app.io.emit("now_playing", &payload).await;
    let _ = app.io.emit("state", &app.st.snapshot()).await;
}

fn now_playing_payload(app: &App, item: &QueueItem) -> Value {
    let entry = {
        let entries = app.st.entries.read().unwrap();
        entries.iter().find(|e| e.id == item.entry_id).cloned()
    };
    let Some(entry) = entry else {
        return json!({"item": app.st.item_row(item), "waiting": "missing"});
    };
    let want_cut = item
        .vocal_cut
        .map(|v| v != 0)
        .unwrap_or(app.st.vocal_cut_default);
    let play_inst = want_cut && entry.inst.is_some();
    let uri = format!(
        "/media/e{}{}",
        entry.id,
        if play_inst { "?v=inst" } else { "" }
    );
    let played = if play_inst {
        entry.inst.clone().unwrap_or_else(|| entry.media.clone())
    } else {
        entry.media.clone()
    };
    // stock MIME guesses — the browser sniffs these fine
    let mime = if entry.audio_only {
        "audio/mpeg"
    } else {
        "video/mp4"
    };

    let scoring = entry.chart.is_some();
    let lyrics = if scoring {
        let rom = item.lyrics_rom.map(|v| v != 0).unwrap_or(false);
        let c = chart::load_chart(entry.chart.as_ref().unwrap());
        Some(chart::lane_payload(&c, rom, 0.0, &[]))
    } else {
        None
    };
    // native renderer owns the mic + session; browser-TV mode spawns
    // the sing session here instead
    let mic = if scoring && !app.native_tv {
        begin_sing(app, item, &entry)
    } else {
        false
    };
    let play_at = now_secs() + app.st.intermission_sec;

    json!({
        "item": app.st.item_row(item),
        "entry": {
            "id": entry.id,
            "artist": entry.artist,
            "title": entry.title,
            "language": entry.language,
            "is_duet": entry.is_duet,
            "duration_sec": entry.duration_sec,
            "thumbnail": entry.art.as_ref()
                .map(|_| format!("/api/art/{}.jpg", entry.id)),
        },
        "playback": {
            "kind": "file",
            "uri": uri,
            "mime": mime,
            "duration_sec": entry.duration_sec,
            "audio_only": entry.audio_only,
            "path": played.display().to_string(),
        },
        "waiting": null,
        "stage": null,
        "scoring": scoring,
        "play_at": play_at,
        "lyrics": lyrics,
        "music_volume": *app.music_volume.lock().unwrap(),
        "mic": mic,
    })
}

/// SingSession.spawn + start_mic port — build multi-track scorers for
/// the claimed parts and start capture. Returns true if mic opened.
fn begin_sing(app: &App, item: &QueueItem, entry: &Entry) -> bool {
    let Some(chart_path) = &entry.chart else { return false };
    let c = chart::load_chart(chart_path);
    let lead = if item.part_choice.is_empty() {
        "P1"
    } else {
        item.part_choice.as_str()
    };
    let mut tracks: Vec<String> = vec![];
    for t in [lead, "P2"] {
        let t = t.to_string();
        let claimed = t == lead
            || (item.performer2_id.is_some() && t != lead && c.tracks.contains_key(&t));
        if claimed && c.tracks.contains_key(&t) {
            tracks.push(t);
        }
    }
    if tracks.is_empty() {
        tracks = c.tracks.keys().take(1).cloned().collect();
    }
    let session = Session::new(c, &tracks, app.st.difficulty, true);
    let mic = MicSession::start(&tracks);
    let tones = mic
        .as_ref()
        .map(|m| std::sync::Arc::clone(&m.tones))
        .unwrap_or_else(|| {
            std::sync::Arc::new(Mutex::new(
                tracks.iter().map(|t| (t.clone(), None)).collect(),
            ))
        });
    let ok = mic.is_some();
    *app.sing.lock().unwrap() = Some(SingCtx {
        session,
        _mic: mic,
        tones,
        last_ms: 0.0,
    });
    ok
}

/// Finish the sing session, emit sing:final, mark the queue item, and
/// advance — mirrors the player:ended handler's tail.
async fn finish_item(app: &App, item_id: i64, outcome: &str) {
    let mut fin = json!({"tracks": {}, "score": 0});
    if let Some(mut sing) = app.sing.lock().unwrap().take() {
        let tracks = sing.session.finish();
        let score = tracks.values().next().map(|s| s.score).unwrap_or(0);
        fin = json!({"tracks": tracks, "score": score});
    }
    let _ = app.io.emit("sing:final", &fin).await;
    let score = fin["score"].as_i64();
    let next = app.st.finish_current(outcome, score, None);
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    let row = {
        app.st
            .history
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.queue_item_id == Some(item_id))
            .last()
            .cloned()
    };
    if let Some(row) = row {
        let _ = app.io.emit("score:add", &score_add_row(app, &row)).await;
    }
    if next.is_some() {
        Box::pin(push_now_playing(app)).await;
    }
}

fn score_add_row(app: &App, h: &crate::state::HistoryRow) -> Value {
    let e = {
        let entries = app.st.entries.read().unwrap();
        entries.iter().find(|e| e.song_id == h.song_id)
            .map(|e| (e.artist.clone(), e.title.clone()))
    };
    let p1 = app.st.performer(h.performer_id);
    let p2 = h.performer2_id.and_then(|id| app.st.performer(id));
    let name = match (p1, p2) {
        (Some(a), Some(b)) => format!("{} & {}", a.name, b.name),
        (Some(a), None) => a.name,
        _ => "?".into(),
    };
    json!({
        "id": h.queue_item_id.unwrap_or(0),
        "performer": name,
        "song_id": h.song_id,
        "artist": e.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
        "title": e.as_ref().map(|f| f.1.clone()).unwrap_or_default(),
        "score": h.score,
        "played_at": h.played_at,
    })
}

fn host_state(app: &App) -> Value {
    let claimed = app.st.host_performer_id.lock().unwrap().is_some();
    json!({"claimed": claimed, "key": app.st.host_key})
}

const TONE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn tone_name(tone_abs: i64) -> String {
    format!(
        "{}{}",
        TONE_NAMES[(tone_abs.rem_euclid(12)) as usize],
        tone_abs.div_euclid(12) + 2
    )
}

fn get_ms(data: &Value) -> Option<f64> {
    data["ms"]
        .as_f64()
        .or_else(|| data["ms"].as_i64().map(|v| v as f64))
}

pub fn register(io: &SocketIo, app: std::sync::Arc<App>) {
    let app2 = app.clone();
    io.ns("/", move |s: SocketRef| {
        let app = app2.clone();
        async move {
        let hello = json!({"v": app.st.build_v});
        let _ = s.emit("hello", &hello);
        let _ = s.emit("state", &app.st.snapshot());
        let _ = s.emit("host:state", &host_state(&app));
        let _ = s.emit("toast", &json!({"kind":"info","text":"connected"}));

        let a = app.clone();
        s.on("state:pull", move |s: SocketRef| {
            let app = a.clone();
            async move {
                let _ = s.emit("state", &app.st.snapshot());
                let _ = s.emit("host:state", &host_state(&app));
            }
        });

        let a = app.clone();
        s.on("tv:ready", move |s: SocketRef| {
            let app = a.clone();
            async move {
                *app.tv_sid.lock().unwrap() = Some(s.id.to_string());
                if app.st.now_playing().is_none() {
                    if app.st.start_next().is_none() {
                        *app.auto_next.lock().unwrap() = now_secs();
                    }
                }
                Box::pin(push_now_playing(&app)).await;
                let _ = app.io.emit("state", &app.st.snapshot()).await;
                let _ = app.io.emit("host:state", &host_state(&app)).await;
            }
        });

        let a = app.clone();
        s.on("player:position", move |_s: SocketRef, Data(data): Data<Value>| {
            let app = a.clone();
            async move {
                let Some(ms) = get_ms(&data) else { return };
                let upd = {
                    let mut slot = app.sing.lock().unwrap();
                    slot.as_mut().map(|sing| {
                        let tones = sing.tones.lock().unwrap().clone();
                        let mic_tone = tones.values().find_map(|v| *v);
                        let r = sing.session.tick(ms, &tones);
                        sing.last_ms = ms;
                        let mic_disp = mic_tone.map(|t| t % 12).unwrap_or(-1);
                        json!({
                            "ms": ms,
                            "beat": r.beat,
                            "mic": mic_disp,
                            "players": r.players,
                            "tones": tones.iter().map(|(t, v)| {
                                (t.clone(), v.map(|x| x % 12).unwrap_or(-1))
                            }).collect::<BTreeMap<String, i64>>(),
                            "tone": mic_disp,
                            "tone_str": mic_tone.map(tone_name),
                            "hit": r.last_hit,
                            "line_events": r.line_events,
                        })
                    })
                };
                let _ = app.io.emit("clock", &json!({"ms": ms})).await;
                if let Some(u) = upd {
                    let _ = app.io.emit("sing:update", &u).await;
                    if let Some(events) = u["line_events"].as_array() {
                        for ev in events {
                            let _ = app.io.emit("sing:line", ev).await;
                        }
                    }
                }
            }
        });

        let a = app.clone();
        s.on("player:ended", move |_s: SocketRef, Data(_d): Data<Value>| {
            let app = a.clone();
            async move {
                let item = app.st.now_playing();
                let Some(item) = item else { return };
                Box::pin(finish_item(&app, item.id, "done")).await;
            }
        });

        let a = app.clone();
        s.on("player:error", move |_s: SocketRef, Data(_d): Data<Value>| {
            let app = a.clone();
            async move {
                let item = app.st.now_playing();
                let Some(item) = item else { return };
                let _ = app.io.emit(
                    "toast",
                    &json!({"kind":"warn","text":"media error — next"}),
                ).await;
                Box::pin(finish_item(&app, item.id, "error")).await;
            }
        });

        let a = app.clone();
        s.on("player:stuck", move |_s: SocketRef| {
            let app = a.clone();
            async move {
                if let Some(sid) = app.tv_sid.lock().unwrap().clone() {
                    if let Ok(sid) = sid.parse() {
                        if let Some(tv) = app.io.get_socket(sid) {
                            let _ = tv.emit("player:retry", &json!({}));
                        }
                    }
                }
            }
        });

        let a = app.clone();
        s.on_disconnect(move |s: SocketRef| {
            let app = a.clone();
            async move {
                let mut tv = app.tv_sid.lock().unwrap();
                if tv.as_deref() == Some(&s.id.to_string()) {
                    *tv = None;
                }
            }
        });
        }
    });
}
