//! ups-play — terminal karaoke: plays the instrumental track, captures
//! mic pitch, scores live against the .upsong chart.
//!
//!   ups-play <song-dir-or-chart>   play one song folder
//!   ups-play <root> <search>       pick first catalog match
//!
//! Ctrl-C or song end exits; the final score prints.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ups_core::decode;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{OutputStream, Sink, Source};
use ups_core::{catalog, chart, pitch, session};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(first) = args.first() else {
        eprintln!("usage: ups-play <song-dir|library-root> [search]");
        return;
    };
    let path = PathBuf::from(first);
    let (dir, mut media, mut chart_path) = if path.is_dir() && args.len() == 1 {
        (path, None, None)
    } else {
        // catalog search — the resolver already located every file
        let needle = args.get(1).cloned().unwrap_or_default().to_lowercase();
        let songs = catalog::scan(&path);
        let hit = songs.iter().find(|s| {
            format!("{} {}", s.artist, s.title).to_lowercase().contains(&needle)
        });
        match hit {
            Some(s) => (
                s.dir.clone(),
                Some(s.inst.clone().unwrap_or_else(|| s.media.clone())),
                s.chart.clone(),
            ),
            None => {
                eprintln!("no match for '{needle}'");
                return;
            }
        }
    };

    // media: prefer the instrumental, fall back to the original
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for f in rd.flatten() {
            let name = f.file_name().to_string_lossy().to_lowercase();
            if media.is_none() && name.ends_with(".inst.mp4") {
                media = Some(f.path());
            } else if chart_path.is_none() && name.ends_with(".upsong") {
                chart_path = Some(f.path());
            }
        }
        if media.is_none() {
            if let Ok(rd2) = std::fs::read_dir(&dir) {
                media = rd2.flatten().map(|f| f.path()).find(|p| {
                    let n = p.file_name().unwrap_or_default()
                        .to_string_lossy().to_lowercase();
                    n.ends_with(".mp4") && !n.contains(".inst.")
                });
            }
        }
    }
    let (Some(media), Some(chart_path)) = (media, chart_path) else {
        eprintln!("need a media file and a .upsong in {}", dir.display());
        return;
    };
    let c = chart::load_chart(&chart_path);
    let errs = c.validate();
    if !errs.is_empty() {
        eprintln!("chart issues: {}", errs.join("; "));
    }
    println!("{} - {} (bpm {}, gap {}ms)",
             c.artist(), c.title(), c.bpm(), c.gap_ms());

    // ---- audio out ------------------------------------------------------
    // cpal's "default" can resolve to an unplugged/dmix-blocked device
    // (e.g. a desktop session where pipewire owns the card) — walk the
    // enumerated devices until one actually opens.
    let mut out = OutputStream::try_default().ok();
    if out.is_none() {
        if let Ok(devs) = cpal::default_host().output_devices() {
            for dev in devs {
                if let Ok(v) = OutputStream::try_from_device(&dev) {
                    println!("audio out: {}", dev.name().unwrap_or_default());
                    out = Some(v);
                    break;
                }
            }
        }
    }
    let Some((_stream, handle)) = out else {
        eprintln!("no audio output");
        return;
    };
    let src = match decode::StreamingSource::open(&media) {
        Ok(v) => v,
        Err(e) => { eprintln!("decode failed: {e}"); return; }
    };
    let dur_s = src.duration().unwrap_or(0.0);
    println!("streaming {} ({}ch @ {}Hz, {:.0}s)", media.display(),
             src.channels(), src.sample_rate(), dur_s);
    let sink = match Sink::try_new(&handle) {
        Ok(s) => s,
        Err(e) => { eprintln!("sink: {e}"); return; }
    };
    sink.append(src);

    // ---- mic -------------------------------------------------------------
    let shared = Arc::new(Mutex::new(VecDeque::<i16>::with_capacity(8192)));
    let host = cpal::default_host();
    let open_mic = |dev: &cpal::Device| {
        let cfg = dev.default_input_config().ok()?;
        let rate = cfg.sample_rate().0;
        let err_fn = |e: cpal::StreamError| eprintln!("mic error: {e}");
        let cfg_stream: cpal::StreamConfig = cfg.clone().into();
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::I16 => {
                let buf = Arc::clone(&shared);
                dev.build_input_stream(
                    &cfg_stream,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        let mut g = buf.lock().unwrap();
                        g.extend(data.iter().copied());
                        while g.len() > 8192 { g.pop_front(); }
                    },
                    err_fn, None)
            }
            cpal::SampleFormat::F32 => {
                let buf = Arc::clone(&shared);
                dev.build_input_stream(
                    &cfg_stream,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        let mut g = buf.lock().unwrap();
                        g.extend(data.iter().map(|&s|
                            (s.clamp(-1.0, 1.0) * 32767.0) as i16));
                        while g.len() > 8192 { g.pop_front(); }
                    },
                    err_fn, None)
            }
            _ => return None,
        }.ok()?;
        stream.play().ok()?;
        Some((rate, stream))
    };
    // the "default" input can be playback-only (asym) — walk devices
    // until one actually opens for capture
    let (det_rate, _mic_stream) = host.input_devices()
        .ok()
        .and_then(|mut devs| {
            devs.find_map(|dev| {
                open_mic(&dev).map(|(rate, stream)| {
                    println!("mic: {} @ {} Hz",
                             dev.name().unwrap_or_default(), rate);
                    (rate, stream)
                })
            })
        })
        .map(|(r, s)| (r, Some(s)))
        .unwrap_or_else(|| {
            println!("no mic — playback only");
            (44100, None)
        });
    let mut det = pitch::PitchDetector::new(det_rate, 2);
    let mut sess = session::Session::solo(c, "P1", 0, true);

    println!("\nsinging… (ctrl-c to stop)\n");
    let mut last_line = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(45));
        // drain mic into detector
        {
            let mut g = shared.lock().unwrap();
            let chunk: Vec<i16> = g.drain(..).collect();
            if !chunk.is_empty() { det.feed(&chunk); }
        }
        let pr = det.analyze();
        let tone = if pr.valid { Some(pr.tone_abs as i64) } else { None };

        let pos_ms = sink.get_pos().as_secs_f64() * 1000.0;
        let tones = {
            let mut m = std::collections::BTreeMap::new();
            m.insert("P1".to_string(), tone);
            m
        };
        let r = sess.tick(pos_ms, &tones);
        let p1 = r.players.get("P1");

        // current lyric line for display
        let line_txt = sess.scorers.get("P1").map(|s| &s.lines).into_iter()
            .flatten()
            .find(|l| l.first().map(|n| n.start).unwrap_or(0) <= r.beat
                   && r.beat < l.last().map(|n| n.start + n.length).unwrap_or(0))
            .map(|l| l.iter().enumerate().map(|(i, n)| format!(
                "{}{}", if i > 0 && n.word_break { " " } else { "" }, n.text))
                .collect::<String>())
            .unwrap_or_default();
        if line_txt != last_line && !line_txt.is_empty() {
            println!("\n  {}", line_txt);
            last_line = line_txt;
        }
        let tgt = r.targets.get("P1").map(|&t| {
            format!("{}", pitch::PitchResult {
                valid: true, tone: (t % 12) as i32, tone_abs: t as i32,
                volume: 0.0 }.tone_str())
        }).unwrap_or_else(|| "-".into());
        print!("\r\x1b[K  {}:{:02}  score {:>5}  streak {:>2}  \
                sung {:>3}  target {:>3}",
               pos_ms as u64 / 60000, (pos_ms as u64 / 1000) % 60,
               p1.map(|p| p.score).unwrap_or(0),
               p1.map(|p| p.streak).unwrap_or(0), pr.tone_str(), tgt);
        std::io::stdout().flush().ok();

        if sink.empty() {
            break;
        }
    }
    let finals = sess.finish();
    if let Some(f) = finals.get("P1") {
        println!("\n\nfinal: {} pts ({} normal + {} golden + {} line)  \
                  {} notes hit / {} perfect / max streak {}",
                 f.score, f.score_normal, f.score_golden, f.score_line,
                 f.hits, f.perfect, f.max_streak);
    }
}
