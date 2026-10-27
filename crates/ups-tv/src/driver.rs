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
    /// persisted chart->media nudge for this entry (0 when untouched)
    pub sync_offset: f64,
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
    /// The app-level pitch autosync toggle (POST /playback/autosync).
    fn autosync_enabled(&self) -> bool {
        false
    }
    /// A host nudge landed — apply this absolute offset to the live
    /// session (drains the mailbox; None = nothing pending).
    fn take_sync_override(&self) -> Option<f64> {
        None
    }
    /// A manual resync was requested (POST /playback/resync).
    fn take_resync_request(&self) -> bool {
        false
    }
    /// The solver locked a new offset — persist it against the entry
    /// and refresh phone-side lyrics.
    fn sync_offset_applied(&self, _off: f64) {}
    /// "Scan to join" QR payload for the idle screen (data URI or "").
    fn idle_info(&self) -> (String, String) {
        (String::new(), String::new())
    }
    /// The room's host code while nobody holds host — drives the
    /// persistent "Host code: XXXX" chip on every TV surface. None =
    /// a host is claimed, hide it; it comes back the moment they
    /// step down.
    fn host_badge(&self) -> Option<String> {
        None
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

/// rating tiers on the scorer's 0..8 scale — same ladder as the web
/// TV ("PERFECT LINE" reserved for the top rung)
const RATING_LABELS: [&str; 9] = [
    "", "cool", "cool", "good", "good", "great", "excellent",
    "superb", "PERFECT LINE",
];

struct Pop {
    text: String,
    born: f32,
    big: bool,
    golden: bool,
}

/// live effects state — particles, rating pops, eased score digits,
/// streak milestones. All state is per-song; idle drift lives in run().
struct Fx {
    parts: Vec<render::Particle>,
    pops: Vec<Pop>,
    disp_score: f64,
    last_streak: i64,
    rng: u64,
}

impl Fx {
    fn new() -> Self {
        Fx {
            parts: Vec::new(),
            pops: Vec::new(),
            disp_score: 0.0,
            last_streak: 0,
            rng: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64 | 1)
                .unwrap_or(0x9e3779b9),
        }
    }
    fn rand(&mut self) -> f32 {
        self.rng = self
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.rng >> 33) as u32) as f32 / u32::MAX as f32
    }
    fn burst(&mut self, x: f32, y: f32, n: usize, rgb: u32, now: f32,
             power: f32, grav: f32, life: f32) {
        for _ in 0..n {
            let a = self.rand() * std::f32::consts::TAU;
            let v = power * (0.4 + self.rand());
            let pl = life * (0.6 + self.rand() * 0.8);
            self.parts.push(render::Particle {
                x,
                y,
                vx: a.cos() * v,
                vy: a.sin() * v - power * 0.4,
                born: now,
                life: pl,
                rgb,
                grav,
            });
        }
    }
    fn pop(&mut self, text: String, now: f32, big: bool, golden: bool) {
        if text.is_empty() {
            return;
        }
        self.pops.push(Pop { text, born: now, big, golden });
    }
    fn step(&mut self, dt: f32, now: f32) {
        self.parts.retain(|p| p.alive(now));
        for p in &mut self.parts {
            p.step(dt);
        }
        self.pops.retain(|p| now - p.born < 1.6);
    }
}

/// Why the kiosk loop ended — `Quit` means the user pressed Escape
/// (don't reopen), `Lost` means the compositor took the surface
/// (screen blanked, wayland hiccup): the caller should respawn it,
/// killing the whole server over a blanked screen is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Quit,
    Lost,
}

/// The kiosk loop — blocks until the window closes.
pub fn run(driver: Arc<dyn Driver>) -> Exit {
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
    let mut idle_fx = Fx::new();

    while window.is_open() {
        if window.is_key_down(Key::Escape) {
            return Exit::Quit;
        }
        let item = driver.current();
        match (item, cur_id) {
            (None, _) => {
                cur_id = None;
                draw_idle(&mut fb, w, h, &mut text, &driver, &mut idle_fx);
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
    Exit::Lost
}

/// layout scale — all pixel constants are written for 720p and scaled
/// to the actual panel (1080p → ×1.5)
fn sc(fh: usize) -> f32 {
    fh as f32 / 720.0
}

/// "Host code: XXXX" bottom-right — same contract as the browser TV's
/// #host-badge: visible on every screen until a host claims the room,
/// then hidden (and back the instant they release it).
fn draw_host_badge(fb: &mut [u32], fw: usize, fh: usize,
                   text: &mut Option<Text>, driver: &Arc<dyn Driver>) {
    let (Some(key), Some(t)) = (driver.host_badge(), text.as_mut())
    else {
        return;
    };
    if key.is_empty() {
        return;
    }
    let s = sc(fh);
    let label = format!("Host code: {key}");
    let px = (22.0 * s).max(11.0) as u32;
    let w = t.width(&label, px) as i32;
    let pad = (10.0 * s) as i32;
    let bw = w + pad * 2;
    let bh = px as i32 + pad * 2;
    let x = fw as i32 - bw - (16.0 * s) as i32;
    let y = fh as i32 - bh - (14.0 * s) as i32;
    render::rect(fb, fw, fh, x, y, bw, bh, 0x000000, 170);
    t.draw(fb, fw, fh, &label, x + pad, y + pad, px, render::TEXT);
}

fn draw_idle(fb: &mut [u32], fw: usize, fh: usize,
             text: &mut Option<Text>, driver: &Arc<dyn Driver>,
             fx: &mut Fx) {
    render::fill(fb, render::BG);
    // slow drifting embers — seeded once, recycled forever; subtle
    // motion so the idle screen feels alive without being a lightshow
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f32())
        .unwrap_or(0.0);
    if fx.parts.is_empty() {
        for _ in 0..70 {
            let x = fx.rand() * fw as f32;
            let y = fx.rand() * fh as f32;
            let vx = -6.0 - fx.rand() * 10.0;
            let vy = -3.0 - fx.rand() * 7.0;
            let born = now - fx.rand() * 12.0;
            let life = 12.0 + fx.rand() * 8.0;
            let rgb = [0x4da3ff, 0xffd24d, 0x8a93a5]
                [(fx.rand() * 3.0) as usize];
            fx.parts.push(render::Particle {
                x, y, vx, vy, born, life, rgb, grav: 0.0,
            });
        }
    }
    let mut respawn = Vec::new();
    for (i, p) in fx.parts.iter_mut().enumerate() {
        p.step(0.06);
        if !p.alive(now) || p.x < -4.0 || p.y < -4.0 {
            respawn.push(i);
        }
    }
    for i in respawn {
        let y = fx.rand() * fh as f32;
        let vx = -6.0 - fx.rand() * 10.0;
        let vy = -3.0 - fx.rand() * 7.0;
        let life = 12.0 + fx.rand() * 8.0;
        let p = &mut fx.parts[i];
        p.x = fw as f32 + 4.0;
        p.y = y;
        p.vx = vx;
        p.vy = vy;
        p.born = now;
        p.life = life;
    }
    render::draw_particles(fb, fw, fh, &fx.parts, now);
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
    draw_host_badge(fb, fw, fh, text, driver);
}

/// Copy `path` into /dev/shm so playback reads from RAM — the stick's
/// USB bandwidth is shared with catalog rescans and chart loads.
/// Skipped silently when shm isn't there or the copy fails.
fn stage_shm(path: &std::path::Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let dst = PathBuf::from("/dev/shm").join(format!(
        "ups-{}-{}", std::process::id(), name.to_string_lossy()
    ));
    if std::fs::copy(path, &dst).is_err() {
        let _ = std::fs::remove_file(&dst); // drop a partial copy
        return None;
    }
    Some(dst)
}

/// RAII cleanup for shm-staged media — removed when play_item returns.
struct Staged(Option<PathBuf>);
impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn play_item(window: &mut Window, fb: &mut [u32], fw: usize, fh: usize,
             text: &mut Option<Text>,
             driver: &Arc<dyn Driver>, it: PlayItem) {
    let s = sc(fh);
    // Preload during the intermission countdown: decode the whole audio
    // track to PCM in RAM (the sink's stream can never underrun) and
    // stage the video into /dev/shm so reads don't contend with the USB
    // stick's chart/rescan traffic. Runs on its own thread — the
    // countdown window absorbs the work, worst case a few seconds late.
    let audio_p = it.audio.clone();
    let video_p = it.video.clone();
    let preload = std::thread::spawn(move || {
        let src = StreamingSource::open(&audio_p).ok();
        let pcm = src.map(|s| {
            let ch = s.channels();
            let rate = s.sample_rate();
            (s.collect::<Vec<f32>>(), ch, rate)
        });
        let shm = video_p.as_ref().and_then(|v| stage_shm(v));
        (pcm, shm)
    });
    let staged = Staged(None);

    let chart: Option<Chart> =
        it.chart.as_ref().map(|p| chart::load_chart(p));
    let tracks = if it.tracks.is_empty() {
        vec!["P1".to_string()]
    } else {
        it.tracks.clone()
    };
    let mut sess = chart.clone().map(|c| {
        let mut s = Session::new(c, &tracks, 1, true);
        s.set_sync_offset(it.sync_offset);
        s
    });
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
    let mut gap_ms = chart
        .as_ref()
        .map(|c| c.gap_ms() + it.sync_offset)
        .unwrap_or(0.0);

    let mic = MicCapture::open();
    let mic_tone = mic.as_ref().map(|m| Arc::clone(&m.tone));
    driver.emit("mic:state", &json!({"ok": mic.is_some()}));

    let mut frame = vec![0u32; fw * fh];
    let mut hit_flash_until = Instant::now();
    let mut last_tick = 0u64;
    let mut fx = Fx::new();
    let mut target_score: f64 = 0.0;
    let mut cur_streak: i64 = 0;
    let song_start = Instant::now();
    let lane_y = (fh as f64 * 0.58) as i32;
    let lane_h = (fh as f64 * 0.16) as i32;

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
            draw_host_badge(fb, fw, fh, text, driver);
            present(window, fb, fw, fh);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // countdown over — collect the preload. PCM decode wins when it
    // succeeded; anything else falls back to the streaming source.
    let (pcm, shm) = preload.join().unwrap_or((None, None));
    let mut staged = staged;
    staged.0 = shm;
    let mut pipe = if !it.audio_only {
        staged.0.as_ref().or(it.video.as_ref())
            .and_then(|v| VideoPipe::open(v, fw, fh, 25.0))
    } else {
        None
    };
    tlog!("video pipe: {}", if pipe.is_some() { "open" } else { "none" });

    // audio is the master clock — same rule as the web TV
    let src: Box<dyn Source<Item = f32> + Send> = match pcm {
        Some((samples, ch, rate)) if !samples.is_empty() => Box::new(
            rodio::buffer::SamplesBuffer::new(ch, rate, samples),
        ),
        _ => match StreamingSource::open(&it.audio) {
            Ok(s) => Box::new(s),
            Err(e) => {
                driver.item_error(it.id, &format!("decode: {e}"));
                std::thread::sleep(Duration::from_millis(300));
                return;
            }
        },
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
    sink.append(src);
    sink.set_volume(driver.volume() as f32);
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
    let pop_px = (34.0 * s).max(16.0) as u32;
    let pop_big_px = (52.0 * s).max(24.0) as u32;
    let mut last_frame = Instant::now();

    loop {
        let dt = last_frame.elapsed().as_secs_f32().min(0.1);
        last_frame = Instant::now();
        let now_f32 = song_start.elapsed().as_secs_f32();
        fx.step(dt, now_f32);
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
                        // hand the spent buffer back to the reader
                        let old = std::mem::replace(&mut frame, f);
                        let _ = p.ret_tx.try_send(old);
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

        // host sync nudges / manual resync ride in through the driver
        if let Some(off) = driver.take_sync_override() {
            if let Some(s) = &mut sess {
                s.set_sync_offset(off);
                gap_ms = s.base_ms;
            }
        }
        if driver.take_resync_request() {
            if let Some(s) = &mut sess {
                s.request_resync(pos_ms);
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
                if driver.autosync_enabled() {
                    if let Some(a) = s.maybe_autosync(pos_ms) {
                        if let Some(off) = a["offset_ms"].as_f64() {
                            s.set_sync_offset(off);
                            gap_ms = s.base_ms;
                            driver.sync_offset_applied(off);
                        }
                        driver.emit("sing:autosync", &a);
                    }
                }
                let now_f32 = song_start.elapsed().as_secs_f32();
                if r.last_hit.as_ref().and_then(|h| h.hit) == Some(true) {
                    hit_flash_until =
                        Instant::now() + Duration::from_millis(200);
                    // sparks at the hit note's row on the playhead
                    let geo = render::lane_geo(fw, lane_h, 6000.0);
                    let tone = r
                        .last_hit
                        .as_ref()
                        .and_then(|h| h.tone)
                        .unwrap_or(24);
                    let (px, ty) = render::lane_xy(
                        geo, lane_y, lane_h, pos_ms, pos_ms, tone);
                    let gold = r
                        .last_hit
                        .as_ref()
                        .and_then(|h| h.kind.as_deref())
                        .map(|k| k.contains("golden"))
                        .unwrap_or(false);
                    fx.burst(
                        px as f32, ty as f32, 9,
                        if gold { render::GOLDEN } else { 0xcfe6ff },
                        now_f32, 90.0, 160.0, 0.9,
                    );
                }
                for ev in &r.line_events {
                    let ri = (ev.rating as usize)
                        .min(RATING_LABELS.len() - 1);
                    let perfect = ev.rating >= 8;
                    fx.pop(RATING_LABELS[ri].to_string(), now_f32,
                           perfect, ev.rating >= 6);
                }
                let st = r
                    .players
                    .get(&tracks[0])
                    .map(|p| p.streak)
                    .unwrap_or(0);
                if st > fx.last_streak && st >= 5 && st % 5 == 0 {
                    fx.pop(format!("x{st} streak!"), now_f32, true,
                           true);
                }
                fx.last_streak = st;
                cur_streak = st;
                target_score = r
                    .players
                    .get(&tracks[0])
                    .map(|p| (p.score + p.golden + p.line) as f64)
                    .unwrap_or(target_score);
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
        // compose — the pipe hands frames in minifb's own bgr0
        // layout, so the blit is one memcpy, not a 2M-pixel convert
        if pipe.is_some() {
            fb.copy_from_slice(&frame);
        } else {
            render::fill(fb, render::BG);
        }

        if !notes.is_empty() {
            let sung = mic_tone.as_ref().and_then(|t| *t.lock().unwrap());
            render::draw_lane(
                fb, fw, fh, &notes, gap_ms, beat_ms, pos_ms, lane_y,
                lane_h, 6000.0, sung, Instant::now() < hit_flash_until,
            );
            let log: Vec<(f64, i64)> = sess
                .as_ref()
                .map(|s| {
                    s.tone_log
                        .values()
                        .flatten()
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            render::draw_trail(fb, fw, fh, &log, pos_ms, lane_y,
                               lane_h, 6000.0);
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
            // eased score digits — the display value chases the real
            // total instead of snapping (exponential approach, ~150ms)
            fx.disp_score += (target_score - fx.disp_score)
                * (1.0 - (-6.0 * dt as f64).exp());
            t.draw(fb, fw, fh,
                   &format!("{} — {}", it.artist, it.title),
                   (16.0 * s) as i32, (14.0 * s) as i32, hud_px,
                   render::TEXT_DIM);
            t.draw(fb, fw, fh,
                   &format!("{}", fx.disp_score.round() as i64),
                   fw as i32 - (160.0 * s) as i32, (12.0 * s) as i32,
                   score_px, render::TEXT);
            if cur_streak >= 2 {
                t.draw(fb, fw, fh, &format!("x{cur_streak}"),
                       fw as i32 - (120.0 * s) as i32,
                       (58.0 * s) as i32, hud_px, render::STREAK);
            }
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
            // rating pops rise and die over ~1.6s — perfect/big lines
            // take the center in gold
            for pop in &fx.pops {
                let age = now_f32 - pop.born;
                let px = if pop.big { pop_big_px } else { pop_px };
                let w = t.width(&pop.text, px) as i32;
                let py = lane_y - (20.0 * s) as i32
                    - (age * 44.0 * s) as i32;
                let col = if pop.golden { render::GOLDEN }
                          else { render::TEXT };
                t.draw(fb, fw, fh, &pop.text, (fw as i32 - w) / 2,
                       py, px, col);
            }
        }
        render::draw_particles(fb, fw, fh, &fx.parts, now_f32);
        draw_host_badge(fb, fw, fh, text, driver);

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
            results_card(window, fb, fw, fh, text, driver,
                         &it, sess.as_mut());
            driver.item_done(it.id, score);
            return;
        }
    }
}

fn results_card(window: &mut Window, fb: &mut [u32], fw: usize, fh: usize,
                text: &mut Option<Text>, driver: &Arc<dyn Driver>,
                it: &PlayItem, sess: Option<&mut Session>) {
    let s = sc(fh);
    let fin = sess.map(|s| s.finish()).unwrap_or_default();
    let wait_until = Instant::now() + Duration::from_secs(8);
    let head_px = (30.0 * s).max(14.0) as u32;
    let line_px = (36.0 * s).max(16.0) as u32;
    // results confetti — one burst from the top-center, falls and
    // fades through the card
    let card_start = Instant::now();
    let mut confetti = Fx::new();
    for (i, col) in [render::GOLDEN, render::NOTE, render::FREESTYLE,
                     0xc792ff, 0xff5a5a].iter().enumerate()
    {
        confetti.burst(fw as f32 / 2.0, fh as f32 * 0.18, 48, *col,
                       i as f32 * 0.01, 260.0, 320.0, 3.2);
    }
    let mut card_last = Instant::now();
    while window.is_open() && Instant::now() < wait_until {
        let cdt = card_last.elapsed().as_secs_f32().min(0.1);
        card_last = Instant::now();
        let cnow = card_start.elapsed().as_secs_f32();
        confetti.step(cdt, cnow);
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
        render::draw_particles(fb, fw, fh, &confetti.parts, cnow);
        draw_host_badge(fb, fw, fh, text, driver);
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
