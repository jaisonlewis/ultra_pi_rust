//! The render loop + Driver contract. A driver supplies "what should be
//! on screen now" and receives lifecycle callbacks; the loop owns the
//! audio sink (master clock), the ffmpeg video pipe, mic capture,
//! scoring, and the framebuffer.
//!
//! The framebuffer always matches the window size exactly — minifb
//! requires buffer dims == window dims, so everything renders at the
//! panel's native resolution. Layout scales off a 720p baseline.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use minifb::{Key, Window, WindowOptions};
use rodio::{OutputStream, Sink, Source};
use serde_json::{json, Value};
use ups_core::chart::{self, Chart};
use ups_core::capture::MicCapture;
use ups_core::decode::StreamingSource;
use ups_core::session::Session;

use crate::render;
use crate::text::Text;
use crate::video::VideoPipe;

/// One thing to play — resolved by the driver (queue layer lives
/// outside this crate).
#[derive(Clone)]
pub struct PlayItem {
    /// stable identity — changes mean "the queue advanced"
    pub id: i64,
    /// audio source (instrumental when vocal-cut applies)
    pub audio: PathBuf,
    /// video source — the same file as `audio` (karaoke mp4s carry the
    /// video too), so picture and clock can never drift apart
    pub video: Option<PathBuf>,
    pub chart: Option<PathBuf>,
    pub artist: String,
    pub title: String,
    pub audio_only: bool,
    /// score-attributed singers for the HUD
    pub singer: String,
    pub tracks: Vec<String>,
}

pub trait Driver: Send + Sync {
    /// What should be playing right now (None = idle screen).
    fn current(&self) -> Option<PlayItem>;
    /// Broadcast an event to remote clients (phones/host).
    fn emit(&self, _event: &str, _data: &Value) {}
    /// Current item finished naturally — driver marks it done and
    /// advances; next call to current() returns the next item.
    fn item_done(&self, _id: i64, _score: i64) {}
    fn item_error(&self, _id: i64, _msg: &str) {}
    fn paused(&self) -> bool {
        false
    }
    fn volume(&self) -> f64 {
        1.0
    }
    /// Intermission countdown to display before starting (seconds).
    fn play_at(&self) -> f64 {
        0.0
    }
    /// "Scan to join" QR payload for the idle screen (data URI or "").
    fn idle_info(&self) -> (String, String) {
        (String::new(), String::new())
    }
}

/// Native panel resolution — the first DRM mode is the preferred one
/// on the Pi's HDMI out. Dev machines have no /sys/class/drm entry and
/// fall back to 720p.
fn screen_size() -> (usize, usize) {
    if let Ok(dir) = std::fs::read_dir("/sys/class/drm") {
        for e in dir.flatten() {
            let Ok(txt) = std::fs::read_to_string(e.path().join("modes"))
            else {
                continue;
            };
            if let Some(first) = txt.lines().next() {
                let mut it = first.trim().split('x');
                let (w, h) = (
                    it.next().unwrap_or("0").parse().unwrap_or(0),
                    it.next().unwrap_or("0").parse().unwrap_or(0),
                );
                if w > 0 && h > 0 {
                    return (w, h);
                }
            }
        }
    }
    (1280, 720)
}

/// log to stderr — every call site also flushes through eprintln,
/// but keep one place so the format stays uniform
macro_rules! tlog {
    ($($arg:tt)*) => { eprintln!("tv: {}", format!($($arg)*)) };
}

/// update_with_buffer can fail (compositor drops the surface, resize
/// races) — a failed present shouldn't kill the kiosk, and a *dead*
/// surface should tell us why in the log instead of dying silently.
fn present(window: &mut Window, fb: &[u32], w: usize, h: usize) -> bool {
    match window.update_with_buffer(fb, w, h) {
        Ok(()) => true,
        Err(e) => {
            tlog!("update_with_buffer failed: {e}");
            false
        }
    }
}

/// The kiosk loop — blocks until the window closes.
pub fn run(driver: Arc<dyn Driver>) {
    // panics on minifb's internal wayland threads print by default, but
    // a panic on THIS thread while a child thread holds the surface
    // deadlocks — hook logs before unwind regardless of thread
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("tv: PANIC: {info}");
        prev(info);
    }));

    let (w, h) = screen_size();
    tlog!("screen {}x{}", w, h);
    let mut window = Window::new(
        // app_id — labwc fullscreens this via a windowRule; sizing the
        // window to the panel makes it fill the screen either way
        "ultra-pi-star",
        w,
        h,
        WindowOptions {
            resize: false,
            borderless: true,
            topmost: true,
            ..WindowOptions::default()
        },
    )
    .expect("window");
    window.set_target_fps(30);
    tlog!("window open");

    let mut fb = vec![0u32; w * h];
    let mut text = Text::new();
    let mut cur_id: Option<i64> = None;

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let item = driver.current();
        match (item, cur_id) {
            (None, _) => {
                cur_id = None;
                draw_idle(&mut fb, w, h, &mut text, &driver);
                present(&mut window, &fb, w, h);
                std::thread::sleep(Duration::from_millis(60));
            }
            (Some(it), _) => {
                cur_id = Some(it.id);
                tlog!("item {} — {}", it.id, it.title);
                play_item(&mut window, &mut fb, w, h, &mut text, &driver, it);
            }
        }
    }
}

/// layout scale — all pixel constants are written for 720p and scaled
/// to the actual panel (1080p → ×1.5)
fn sc(fh: usize) -> f32 {
    fh as f32 / 720.0
}

fn draw_idle(fb: &mut [u32], fw: usize, fh: usize,
             text: &mut Option<Text>, driver: &Arc<dyn Driver>) {
    render::fill(fb, render::BG);
    let s = sc(fh);
    let (qr_data, join_url) = driver.idle_info();
    let title_px = (64.0 * s) as u32;
    let sub_px = (26.0 * s).max(12.0) as u32;
    let url_px = (30.0 * s).max(14.0) as u32;
    if let Some(t) = text {
        let title = "Ultra Pi Star";
        let w = t.width(title, title_px) as i32;
        t.draw(fb, fw, fh, title, (fw as i32 - w) / 2,
               (180.0 * s) as i32, title_px, render::TEXT);
        let sub = "Scan to join & queue a song";
        let w2 = t.width(sub, sub_px) as i32;
        t.draw(fb, fw, fh, sub, (fw as i32 - w2) / 2,
               (270.0 * s) as i32, sub_px, render::TEXT_DIM);
        if !join_url.is_empty() {
            let w3 = t.width(&join_url, url_px) as i32;
            t.draw(fb, fw, fh, &join_url, (fw as i32 - w3) / 2,
                   (560.0 * s) as i32, url_px, render::TEXT);
        }
    }
    // QR modules drawn directly — no image decode needed
    if !qr_data.is_empty() {
        if let Ok(code) = qrcode::QrCode::new(qr_data.as_bytes()) {
            let mods = code.to_colors();
            let side = code.width();
            let cell = (6.0 * s).max(2.0) as i32;
            let ox = (fw as i32 - side as i32 * cell) / 2;
            let oy = (330.0 * s) as i32;
            render::rect(fb, fw, fh, ox - 12, oy - 12,
                         side as i32 * cell + 24,
                         side as i32 * cell + 24, 0xffffff, 255);
            for (i, c) in mods.iter().enumerate() {
                if *c == qrcode::Color::Dark {
                    let x = ox + (i % side) as i32 * cell;
                    let y = oy + (i / side) as i32 * cell;
                    render::rect(fb, fw, fh, x, y, cell, cell, 0x000000, 255);
                }
            }
        }
    }
}

fn play_item(window: &mut Window, fb: &mut [u32], fw: usize, fh: usize,
             text: &mut Option<Text>,
             driver: &Arc<dyn Driver>, it: PlayItem) {
    let s = sc(fh);
    // audio is the master clock — same rule as the web TV
    let src = match StreamingSource::open(&it.audio) {
        Ok(s) => s,
        Err(e) => {
            driver.item_error(it.id, &format!("decode: {e}"));
            std::thread::sleep(Duration::from_millis(300));
            return;
        }
    };
    let dur = src.total_duration().unwrap_or(Duration::ZERO);
    let (_stream, handle) = match OutputStream::try_default() {
        Ok(v) => v,
        Err(e) => {
            driver.item_error(it.id, &format!("audio out: {e}"));
            return;
        }
    };
    let sink = Sink::try_new(&handle).expect("sink");
    sink.pause(); // held until the countdown finishes
    sink.append(src);
    sink.set_volume(driver.volume() as f32);

    let chart: Option<Chart> =
        it.chart.as_ref().map(|p| chart::load_chart(p));
    let tracks = if it.tracks.is_empty() {
        vec!["P1".to_string()]
    } else {
        it.tracks.clone()
    };
    let mut sess = chart
        .clone()
        .map(|c| Session::new(c, &tracks, 1, true));
    let phrases = chart
        .as_ref()
        .and_then(|c| c.phrases().get(&tracks[0]).cloned())
        .unwrap_or_default();
    let notes = chart
        .as_ref()
        .and_then(|c| c.tracks.get(&tracks[0]).cloned())
        .unwrap_or_default();
    let beat_ms = chart
        .as_ref()
        .map(|c| if c.bpm() > 0.0 { 15000.0 / c.bpm() } else { 0.0 })
        .unwrap_or(0.0);
    let gap_ms = chart.as_ref().map(|c| c.gap_ms()).unwrap_or(0.0);

    let mic = MicCapture::open();
    let mic_tone = mic.as_ref().map(|m| Arc::clone(&m.tone));
    driver.emit("mic:state", &json!({"ok": mic.is_some()}));

    let mut frame = vec![0u8; fw * fh * 3];
    let mut hit_flash_until = Instant::now();
    let mut last_tick = 0u64;

    // intermission countdown — sink is paused, so nothing is audible
    // until it ends (media starts decoding only after, too)
    let play_at = driver.play_at();
    if play_at > now_secs() {
        let label_px = (40.0 * s).max(16.0) as u32;
        let num_px = (90.0 * s) as u32;
        let singer_px = (26.0 * s).max(12.0) as u32;
        loop {
            let left = play_at - now_secs();
            if left <= 0.0
                || !window.is_open()
                || window.is_key_down(Key::Escape)
            {
                break;
            }
            if driver.current().map(|c| c.id) != Some(it.id) {
                return; // queue moved on during countdown
            }
            render::fill(fb, render::BG);
            if let Some(t) = text {
                let label = format!("{} — {}", it.artist, it.title);
                let w = t.width(&label, label_px) as i32;
                t.draw(fb, fw, fh, &label, (fw as i32 - w) / 2,
                       fh as i32 / 2 - (40.0 * s) as i32, label_px,
                       render::TEXT);
                let n = format!("{}", left.ceil() as i32);
                let w2 = t.width(&n, num_px) as i32;
                t.draw(fb, fw, fh, &n, (fw as i32 - w2) / 2,
                       fh as i32 / 2 + (40.0 * s) as i32, num_px,
                       render::GOLDEN);
                if !it.singer.is_empty() {
                    let sv = format!("get ready, {}", it.singer);
                    let w3 = t.width(&sv, singer_px) as i32;
                    t.draw(fb, fw, fh, &sv, (fw as i32 - w3) / 2,
                           fh as i32 / 2 - (90.0 * s) as i32, singer_px,
                           render::TEXT_DIM);
                }
            }
            present(window, fb, fw, fh);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let mut pipe = if !it.audio_only {
        it.video
            .as_ref()
            .and_then(|v| VideoPipe::open(v, fw, fh, 25.0))
    } else {
        None
    };
    tlog!("video pipe: {}", if pipe.is_some() { "open" } else { "none" });
    let mut stat_at = Instant::now();
    let mut video_done = pipe.is_none();
    let mut audio_done_at: Option<Instant> = None;
    let dur_ms = dur.as_secs_f64() * 1000.0;
    let play_start = Instant::now();
    sink.play();

    let lyric_px = (42.0 * s).max(18.0) as u32;
    let next_px = (26.0 * s).max(12.0) as u32;
    let hud_px = (22.0 * s).max(10.0) as u32;
    let score_px = (44.0 * s) as u32;
    let singer_px = (20.0 * s).max(10.0) as u32;
    let paused_px = (60.0 * s) as u32;

    loop {
        if !window.is_open() || window.is_key_down(Key::Escape) {
            sink.stop();
            return;
        }
        // queue advanced underneath us (host skip) — abort
        if driver.current().map(|c| c.id) != Some(it.id) {
            sink.stop();
            return;
        }
        if driver.paused() {
            sink.pause();
        } else {
            sink.play();
        }
        sink.set_volume(driver.volume() as f32);

        // audio is the master clock — but when it runs out before the
        // video does (shorter instrumental/stem), coast the position on
        // wall time so the leftover frames keep playing to the tail
        let audio_pos_ms = sink.get_pos().as_secs_f64() * 1000.0;
        if audio_done_at.is_none()
            && (sink.empty() || (dur > Duration::ZERO && audio_pos_ms >= dur_ms))
        {
            audio_done_at = Some(Instant::now());
        }
        let pos_ms = audio_done_at
            .map(|t| dur_ms + t.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(audio_pos_ms);

        if let Some(p) = &mut pipe {
            let want = (pos_ms / 1000.0 * p.fps) as u64;
            loop {
                if p.frames >= want {
                    break;
                }
                match p.rx.try_recv() {
                    Ok(f) => {
                        frame = f;
                        p.frames += 1;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        video_done = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
        }

        // scoring tick ~ every 50ms (position events, like tv.js)
        if pos_ms as u64 / 50 != last_tick {
            last_tick = pos_ms as u64 / 50;
            if let Some(s) = &mut sess {
                let mut tones: BTreeMap<String, Option<i64>> =
                    BTreeMap::new();
                let tone =
                    mic_tone.as_ref().and_then(|t| *t.lock().unwrap());
                for tr in &tracks {
                    tones.insert(tr.clone(), tone);
                }
                let r = s.tick(pos_ms, &tones);
                if r.last_hit.as_ref().and_then(|h| h.hit) == Some(true) {
                    hit_flash_until =
                        Instant::now() + Duration::from_millis(200);
                }
                driver.emit(
                    "clock",
                    &json!({"ms": pos_ms}),
                );
                driver.emit(
                    "sing:update",
                    &json!({
                        "ms": pos_ms,
                        "beat": r.beat,
                        "mic": tone.map(|t| t % 12).unwrap_or(-1),
                        "players": r.players,
                        "tone": tone.map(|t| t % 12).unwrap_or(-1),
                        "hit": r.last_hit,
                        "line_events": r.line_events,
                    }),
                );
                for ev in &r.line_events {
                    driver.emit("sing:line", &json!(ev));
                }
            }
        }
        // telemetry — frame lag tells us if the decoder can't keep up
        if stat_at.elapsed() > Duration::from_secs(2) {
            stat_at = Instant::now();
            if let Some(p) = &pipe {
                let want = (pos_ms / 1000.0 * p.fps) as u64;
                tlog!(
                    "pos {:.1}s wall {:.1}s produced {} decoded {} want {} \
                     lag {} frames",
                    pos_ms / 1000.0,
                    play_start.elapsed().as_secs_f64(),
                    p.produced
                        .load(std::sync::atomic::Ordering::Relaxed),
                    p.frames,
                    want,
                    want as i64 - p.frames as i64
                );
            }
        }
        // compose
        if pipe.is_some() {
            render::blit_video(fb, &frame, fw, fh);
        } else {
            render::fill(fb, render::BG);
        }

        let lane_y = (fh as f64 * 0.58) as i32;
        let lane_h = (fh as f64 * 0.16) as i32;
        if !notes.is_empty() {
            let sung = mic_tone.as_ref().and_then(|t| *t.lock().unwrap());
            render::draw_lane(
                fb, fw, fh, &notes, gap_ms, beat_ms, pos_ms, lane_y,
                lane_h, 6000.0, sung, Instant::now() < hit_flash_until,
            );
        }

        if let Some(t) = text {
            let lh_y = (fh as f64 * 0.82) as i32;
            let cur = phrases.iter().find(|p| {
                pos_ms >= gap_ms + p.start as f64 * beat_ms
                    && pos_ms < gap_ms + p.end as f64 * beat_ms + 400.0
            });
            let next = phrases
                .iter()
                .find(|p| gap_ms + p.start as f64 * beat_ms > pos_ms);
            if let Some(p) = cur {
                let mut cx =
                    (fw as i32 - t.width(&p.text, lyric_px) as i32) / 2;
                for syl in &p.syls {
                    let sung_on =
                        gap_ms + syl.s as f64 * beat_ms <= pos_ms;
                    cx += t.draw(
                        fb, fw, fh, &syl.t, cx, lh_y, lyric_px,
                        if sung_on { render::SUNG } else { render::TEXT },
                    ) as i32;
                    if syl.wb {
                        cx += t.draw(fb, fw, fh, " ", cx, lh_y,
                                     lyric_px, render::TEXT)
                            as i32;
                    }
                }
            }
            if let Some(p) = next {
                let w = t.width(&p.text, next_px) as i32;
                t.draw(fb, fw, fh, &p.text, (fw as i32 - w) / 2,
                       lh_y + (62.0 * s) as i32, next_px,
                       render::TEXT_DIM);
            }
            let score = sess
                .as_ref()
                .and_then(|s| {
                    s.scorers
                        .get(&tracks[0])
                        .map(|sc| sc.state.total())
                })
                .unwrap_or(0);
            t.draw(fb, fw, fh,
                   &format!("{} — {}", it.artist, it.title),
                   (16.0 * s) as i32, (14.0 * s) as i32, hud_px,
                   render::TEXT_DIM);
            t.draw(fb, fw, fh, &format!("{score}"),
                   fw as i32 - (160.0 * s) as i32, (12.0 * s) as i32,
                   score_px, render::TEXT);
            if !it.singer.is_empty() {
                t.draw(fb, fw, fh, &it.singer, (16.0 * s) as i32,
                       (38.0 * s) as i32, singer_px, render::TEXT_DIM);
            }
            if driver.paused() {
                let p = "PAUSED";
                let w = t.width(p, paused_px) as i32;
                t.draw(fb, fw, fh, p, (fw as i32 - w) / 2,
                       fh as i32 / 2, paused_px, render::GOLDEN);
            }
        }

        if !present(window, fb, fw, fh) {
            sink.stop();
            return;
        }
        std::thread::sleep(Duration::from_millis(4));

        // song ends when BOTH are done: audio reached its end and the
        // video pipe has drained its last frame
        if audio_done_at.is_some() && video_done {
            tlog!("item {} finished (audio {}s, video done)", it.id, dur.as_secs());
            let score = sess
                .as_mut()
                .and_then(|s| {
                    s.finish()
                        .values()
                        .next()
                        .map(|f| f.score)
                })
                .unwrap_or(0);
            results_card(window, fb, fw, fh, text, &it, sess.as_mut());
            driver.item_done(it.id, score);
            return;
        }
    }
}

fn results_card(window: &mut Window, fb: &mut [u32], fw: usize, fh: usize,
                text: &mut Option<Text>, it: &PlayItem,
                sess: Option<&mut Session>) {
    let s = sc(fh);
    let fin = sess.map(|s| s.finish()).unwrap_or_default();
    let wait_until = Instant::now() + Duration::from_secs(8);
    let head_px = (30.0 * s).max(14.0) as u32;
    let line_px = (36.0 * s).max(16.0) as u32;
    while window.is_open() && Instant::now() < wait_until {
        render::fill(fb, render::BG);
        if let Some(t) = text {
            let head = format!("{} — {}", it.artist, it.title);
            let w = t.width(&head, head_px) as i32;
            t.draw(fb, fw, fh, &head, (fw as i32 - w) / 2,
                   fh as i32 / 2 - (150.0 * s) as i32, head_px,
                   render::TEXT_DIM);
            let mut y = fh as i32 / 2 - (90.0 * s) as i32;
            for (tr, s) in &fin {
                let line = format!(
                    "{tr}   {} pts   {}/{} hits   {} perfect   x{} streak",
                    s.score, s.hits, s.notes, s.perfect, s.max_streak);
                let w = t.width(&line, line_px) as i32;
                t.draw(fb, fw, fh, &line, (fw as i32 - w) / 2, y,
                       line_px, render::TEXT);
                y += (64.0 * sc(fh)) as i32;
            }
        }
        present(window, fb, fw, fh);
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
