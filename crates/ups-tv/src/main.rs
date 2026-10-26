//! ups-tv — native karaoke screen. One framebuffer, video piped from
//! ffmpeg, overlays drawn as rects + fontdue text. This is the
//! Chromium replacement: ~40 MB instead of ~1 GB.
//!
//! usage: ups-tv <library-root> <search>   — find & play a song
//!        ups-tv <song-dir>               — play a folder directly
//!        --audio-only                     — skip the video pipe

mod render;
mod text;
mod video;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use minifb::{Key, Window, WindowOptions};
use rodio::{OutputStream, Sink, Source};
use ups_core::catalog;
use ups_core::chart::{self, Chart};
use ups_core::capture::MicCapture;
use ups_core::decode::StreamingSource;
use ups_core::session::Session;

use render::{W, H};

struct Playable {
    media: PathBuf,
    inst: Option<PathBuf>,
    chart: Option<PathBuf>,
    artist: String,
    title: String,
}

fn resolve(args: &[String]) -> Option<Playable> {
    let path = PathBuf::from(&args[0]);
    let needle = args.get(1).cloned().unwrap_or_default().to_lowercase();
    if path.is_dir() && needle.is_empty() {
        // directory mode: find media/chart inside
        let recs = catalog::scan_folders(&path.parent()?.to_path_buf());
        let s = recs.into_iter().find(|s| {
            s.media.parent() == Some(path.as_path())
        });
        return s.map(|s| Playable {
            media: s.media,
            inst: s.inst,
            chart: s.chart,
            artist: s.artist,
            title: s.title,
        });
    }
    // root + search mode
    catalog::scan(&path)
        .into_iter()
        .find(|s| {
            s.title.to_lowercase().contains(&needle)
                || s.artist.to_lowercase().contains(&needle)
        })
        .map(|s| Playable {
            media: s.media,
            inst: s.inst,
            chart: s.chart,
            artist: s.artist,
            title: s.title,
        })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: ups-tv <root|dir> [search] [--audio-only]");
        return;
    }
    let audio_only = args.iter().any(|a| a == "--audio-only");
    let Some(song) = resolve(&args[1..]) else {
        eprintln!("song not found");
        return;
    };
    eprintln!("playing: {} — {}", song.artist, song.title);

    // instrumental if present (default vocal cut, like UPS_VOCAL_CUT=1)
    let play_media = song.inst.clone().unwrap_or_else(|| song.media.clone());
    let src = StreamingSource::open(&play_media).expect("decode failed");

    // video frames come from the ORIGINAL file — keeps picture even
    // when the instrumental is an audio-only stem
    let has_video = !audio_only && !["mp3", "m4a", "ogg", "wav", "flac"]
        .iter()
        .any(|e| song.media.display().to_string().to_lowercase().ends_with(e));
    let mut pipe = if has_video {
        video::VideoPipe::open(&song.media, W, H, 25.0)
    } else {
        None
    };

    let (stream, handle) = OutputStream::try_default().expect("no output");
    let sink = Sink::try_new(&handle).expect("sink");
    let dur = src.total_duration().unwrap_or(Duration::ZERO);
    sink.append(src);
    sink.play();
    let _keep = stream;

    let chart: Option<Chart> =
        song.chart.as_ref().map(|p| chart::load_chart(p));
    let track = "P1".to_string();
    let mut sess = chart.as_ref().map(|c| {
        Session::new(c.clone(), &[track.clone()], 1, true)
    });
    let phrases = chart
        .as_ref()
        .map(|c| c.phrases().get("P1").cloned().unwrap_or_default())
        .unwrap_or_default();
    let notes = chart
        .as_ref()
        .and_then(|c| c.tracks.get("P1").cloned())
        .unwrap_or_default();
    let beat_ms = chart
        .as_ref()
        .map(|c| if c.bpm() > 0.0 { 15000.0 / c.bpm() } else { 0.0 })
        .unwrap_or(0.0);
    let gap_ms = chart.as_ref().map(|c| c.gap_ms()).unwrap_or(0.0);

    let mic = MicCapture::open();
    let mic_tone = mic.as_ref().map(|m| std::sync::Arc::clone(&m.tone));
    eprintln!("mic: {}", if mic.is_some() { "ok" } else { "none" });

    let mut window = Window::new(
        &format!("Ultra Pi Star — {} — {}", song.artist, song.title),
        W, H,
        WindowOptions { resize: false, ..WindowOptions::default() },
    )
    .expect("window");
    window.set_target_fps(30);

    let mut text = text::Text::new();
    let mut fb = vec![0u32; W * H];
    let mut frame = vec![0u8; W * H * 3];
    render::fill(&mut fb, render::BG);

    let mut last_hit_beat = -1i64;
    let mut hit_flash_until = Instant::now();

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let pos = sink.get_pos().as_secs_f64() * 1000.0;
        let pos_ms = pos as f64;

        // -- drain video pipe to the frame matching the audio clock
        if let Some(p) = &mut pipe {
            let want = (pos_ms / 1000.0 * p.fps) as u64;
            loop {
                match p.rx.try_recv() {
                    Ok(f) => {
                        frame = f;
                        p.frames += 1;
                        if p.frames >= want {
                            break; // caught up — keep this frame
                        }
                        // else discard stale frame, pull next
                    }
                    Err(_) => break,
                }
            }
        }

        // -- tick scoring
        let mut sung = None;
        let mut score = 0i64;
        let mut streak = 0i64;
        if let Some(s) = &mut sess {
            let mut tones: BTreeMap<String, Option<i64>> =
                BTreeMap::new();
            let tone = mic_tone
                .as_ref()
                .and_then(|t| *t.lock().unwrap());
            tones.insert(track.clone(), tone);
            let r = s.tick(pos_ms, &tones);
            sung = tone;
            score = r.players.get(&track).map(|p| p.score).unwrap_or(0);
            streak =
                r.players.get(&track).map(|p| p.streak).unwrap_or(0);
            if let Some(h) = r.last_hit.as_ref() {
                if h.beat > last_hit_beat {
                    last_hit_beat = h.beat;
                    if h.hit == Some(true) {
                        hit_flash_until =
                            Instant::now() + Duration::from_millis(200);
                    }
                }
            }
        }

        // -- compose frame
        if pipe.is_some() {
            render::blit_video(&mut fb, &frame);
        } else {
            render::fill(&mut fb, render::BG);
        }

        // pitch lane band
        let lane_y = (H as f64 * 0.58) as i32;
        let lane_h = (H as f64 * 0.16) as i32;
        if !notes.is_empty() {
            render::draw_lane(
                &mut fb, &notes, gap_ms, beat_ms, pos_ms,
                lane_y, lane_h, 6000.0, sung,
                Instant::now() < hit_flash_until,
            );
        }

        // lyrics — current phrase centered, sung syllables highlighted
        if let Some(t) = &mut text {
            let lh_y = (H as f64 * 0.82) as i32;
            let cur = phrases
                .iter()
                .find(|p| {
                    pos_ms >= gap_ms + p.start as f64 * beat_ms
                        && pos_ms < gap_ms + p.end as f64 * beat_ms + 400.0
                });
            let next = phrases
                .iter()
                .find(|p| gap_ms + p.start as f64 * beat_ms > pos_ms);
            if let Some(p) = cur {
                let mut cx =
                    (W as i32 - t.width(&p.text, 42) as i32) / 2;
                for syl in &p.syls {
                    let sung_on = gap_ms + syl.s as f64 * beat_ms
                        <= pos_ms;
                    let s = if syl.wb { syl.t.as_str() } else { &*syl.t };
                    cx += t.draw(
                        &mut fb, W, H, s, cx, lh_y, 42,
                        if sung_on { render::SUNG } else { render::TEXT },
                    ) as i32;
                    if syl.wb {
                        cx += t.draw(&mut fb, W, H, " ", cx, lh_y, 42,
                                     render::TEXT) as i32;
                    }
                }
            }
            if let Some(p) = next {
                let w = t.width(&p.text, 26) as i32;
                t.draw(&mut fb, W, H, &p.text, (W as i32 - w) / 2,
                       lh_y + 62, 26, render::TEXT_DIM);
            }
            // HUD
            t.draw(&mut fb, W, H,
                   &format!("{} — {}", song.artist, song.title),
                   20, 16, 22, render::TEXT_DIM);
            t.draw(&mut fb, W, H, &format!("{score}"), W as i32 - 160,
                   16, 44, render::TEXT);
            if streak > 1 {
                t.draw(&mut fb, W, H, &format!("x{streak}"),
                       W as i32 - 160, 64, 22, render::GOLDEN);
            }
            let secs = pos_ms / 1000.0;
            t.draw(&mut fb, W, H,
                   &format!("{}:{:02}", secs as i64 / 60,
                            secs as i64 % 60),
                   20, H as i32 - 30, 20, render::TEXT_DIM);
        }

        window.update_with_buffer(&fb, W, H).unwrap();
        std::thread::sleep(Duration::from_millis(4));

        // song over → final card
        if dur > Duration::ZERO && sink.get_pos() >= dur {
            break;
        }
    }

    // results card
    if let Some(s) = &mut sess {
        let fin = s.finish();
        if let Some(t) = &mut text {
            let wait_until = Instant::now() + Duration::from_secs(8);
            while window.is_open() && Instant::now() < wait_until {
                render::fill(&mut fb, render::BG);
                let mut y = H as i32 / 2 - 120;
                for (tr, s) in &fin {
                    let line = format!(
                        "{tr}  score {}   hits {}/{}   perfect {}   streak x{}",
                        s.score, s.hits, s.notes, s.perfect, s.max_streak);
                    let w = t.width(&line, 34) as i32;
                    t.draw(&mut fb, W, H, &line, (W as i32 - w) / 2,
                           y, 34, render::TEXT);
                    y += 60;
                }
                window.update_with_buffer(&fb, W, H).unwrap();
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    }
}
