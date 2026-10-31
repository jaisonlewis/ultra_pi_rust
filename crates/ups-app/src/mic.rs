//! Mic capture for the sing session — cpal input into the shared pitch
//! detector, latest tone per track published under a mutex so the
//! socket loop can read it on every player:position tick.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ups_core::pitch::PitchDetector;

pub struct MicSession {
    /// track -> latest detected absolute tone (None = no confident pitch)
    pub tones: Arc<Mutex<BTreeMap<String, Option<i64>>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    _stream: cpal::Stream,
    worker: Option<JoinHandle<()>>,
}

// cpal marks Stream !Send for Web/Emscripten reasons; on native hosts
// the stream is owned here and only dropped from one place.
unsafe impl Send for MicSession {}

impl MicSession {
    /// Open the first capture-capable device; run the detector on a
    /// worker thread. `tracks` get the same physical mic (single-mic
    /// setups) until per-mic routing lands.
    pub fn start(tracks: &[String]) -> Option<Self> {
        let host = cpal::default_host();
        let devs = host.input_devices().ok()?;
        let mut opened = None;
        for dev in devs {
            let Ok(cfg) = dev.default_input_config() else { continue };
            let rate = cfg.sample_rate().0;
            let shared = Arc::new(Mutex::new(VecDeque::<i16>::new()));
            let cfg_stream: cpal::StreamConfig = cfg.clone().into();
            let err_fn = |e: cpal::StreamError| eprintln!("mic error: {e}");
            let stream = match cfg.sample_format() {
                cpal::SampleFormat::I16 => {
                    let buf = Arc::clone(&shared);
                    dev.build_input_stream(
                        &cfg_stream,
                        move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            let mut g = buf.lock().unwrap();
                            g.extend(data.iter().copied());
                            while g.len() > 8192 {
                                g.pop_front();
                            }
                        },
                        err_fn, None)
                }
                cpal::SampleFormat::F32 => {
                    let buf = Arc::clone(&shared);
                    dev.build_input_stream(
                        &cfg_stream,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            let mut g = buf.lock().unwrap();
                            g.extend(data.iter().map(|&s| {
                                (s.clamp(-1.0, 1.0) * 32767.0) as i16
                            }));
                            while g.len() > 8192 {
                                g.pop_front();
                            }
                        },
                        err_fn, None)
                }
                _ => continue,
            };
            let Ok(stream) = stream else { continue };
            if stream.play().is_err() {
                continue;
            }
            opened = Some((dev, stream, shared, rate));
            break;
        }
        let (dev, stream, shared, rate) = opened?;
        eprintln!("mic: {} @ {} Hz", dev.name().unwrap_or_default(), rate);

        let tones: Arc<Mutex<BTreeMap<String, Option<i64>>>> =
            Arc::new(Mutex::new(
                tracks.iter().map(|t| (t.clone(), None)).collect(),
            ));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tones_w, stop_w) = (Arc::clone(&tones), Arc::clone(&stop));
        let worker = std::thread::spawn(move || {
            let mut det = PitchDetector::new(rate, 2);
            while !stop_w.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(30));
                let chunk: Vec<i16> = {
                    let mut g = shared.lock().unwrap();
                    g.drain(..).collect()
                };
                if !chunk.is_empty() {
                    det.feed(&chunk);
                }
                let pr = det.analyze();
                let tone = if pr.valid {
                    Some(pr.tone_abs as i64)
                } else {
                    None
                };
                let mut t = tones_w.lock().unwrap();
                for v in t.values_mut() {
                    *v = tone;
                }
            }
        });
        Some(MicSession {
            tones,
            stop,
            _stream: stream,
            worker: Some(worker),
        })
    }
}

impl Drop for MicSession {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Speaker→mic loop-delay calibration — port of `mic.py`'s click
/// method: loud 1kHz clicks out the default output, matched-filter the
/// raw capture for them, median the per-click delays. Blocking ~3s —
/// call off the async executor.
pub fn calibrate() -> Result<f64, String> {
    const N_CLICKS: usize = 4;
    const CLICK_MS: f64 = 30.0;
    const GAP_MS: f64 = 350.0;
    const LEAD_MS: f64 = 500.0;

    // buffer raw capture with arrival times — weak room pickup defeats
    // energy thresholds but not correlation
    let host = cpal::default_host();
    let devs = host.input_devices().map_err(|e| e.to_string())?;
    let mut opened = None;
    for dev in devs {
        let Ok(cfg) = dev.default_input_config() else { continue };
        let rate = cfg.sample_rate().0 as f64;
        let shared = Arc::new(Mutex::new(Vec::<(Instant, Vec<i16>)>::new()));
        let cfg_stream: cpal::StreamConfig = cfg.clone().into();
        let err_fn = |e: cpal::StreamError| eprintln!("mic error: {e}");
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::I16 => {
                let buf = Arc::clone(&shared);
                dev.build_input_stream(
                    &cfg_stream,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        buf.lock().unwrap()
                            .push((Instant::now(), data.to_vec()));
                    },
                    err_fn, None)
            }
            cpal::SampleFormat::F32 => {
                let buf = Arc::clone(&shared);
                dev.build_input_stream(
                    &cfg_stream,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        buf.lock().unwrap().push((
                            Instant::now(),
                            data.iter().map(|&s| {
                                (s.clamp(-1.0, 1.0) * 32767.0) as i16
                            }).collect(),
                        ));
                    },
                    err_fn, None)
            }
            _ => continue,
        };
        let Ok(stream) = stream else { continue };
        if stream.play().is_err() {
            continue;
        }
        opened = Some((stream, shared, rate));
        break;
    }
    let (_cap, shared, sr) = opened
        .ok_or_else(|| "no capture device".to_string())?;

    // click pattern in the capture's own rate — the acoustic 1kHz is
    // what the mic hears either way, and correlating in one domain
    // dodges resample skew
    let click: Vec<i16> = (0..(sr * CLICK_MS / 1000.0) as usize)
        .map(|i| ((2.0 * std::f64::consts::PI * 1000.0 * i as f64 / sr)
                      .sin() * 32767.0 * 0.9) as i16)
        .collect();
    let gap_len = (sr * GAP_MS / 1000.0) as usize;
    let mut buf = vec![0i16; (sr * LEAD_MS / 1000.0) as usize];
    let mut offsets = Vec::new();
    for _ in 0..N_CLICKS {
        offsets.push(buf.len());
        buf.extend_from_slice(&click);
        buf.resize(buf.len() + gap_len, 0);
    }

    // play through the default output — the same sink routing songs
    // use, so the measured loop matches the scoring loop
    let (_out, handle) = rodio::OutputStream::try_default()
        .map_err(|e| format!("audio out: {e}"))?;
    let sink = rodio::Sink::try_new(&handle).map_err(|e| e.to_string())?;
    let pcm_f32: Vec<f32> = buf.iter()
        .map(|&v| v as f32 / 32768.0).collect();
    sink.append(rodio::buffer::SamplesBuffer::new(1, sr as u32, pcm_f32));
    sink.play();
    let t0 = Instant::now();
    sink.sleep_until_end();
    std::thread::sleep(Duration::from_millis(400));
    drop(sink);

    // rebuild the capture timeline: chunks arrived contiguously, so
    // the last chunk's arrival anchors the whole buffer
    let chunks = std::mem::take(&mut *shared.lock().unwrap());
    let Some(&(t_end, _)) = chunks.last() else {
        return Err("mic captured nothing".into());
    };
    let total: usize = chunks.iter().map(|(_, c)| c.len()).sum();
    let pcm: Vec<f32> = chunks.iter()
        .flat_map(|(_, c)| c.iter().map(|&v| v as f32)).collect();

    // normalized cross-correlation of the click against the capture
    let cf: Vec<f32> = click.iter().map(|&v| v as f32).collect();
    let n = pcm.len().saturating_sub(cf.len());
    if n == 0 {
        return Err("capture too short".into());
    }
    let c_e: f32 = cf.iter().map(|v| v * v).sum();
    let mut corr = vec![0f32; n];
    for i in 0..n {
        let (mut acc, mut e) = (0f32, 0f32);
        for k in 0..cf.len() {
            acc += pcm[i + k] * cf[k];
            e += pcm[i + k] * pcm[i + k];
        }
        if e > 0.0 {
            corr[i] = acc / (e * c_e).sqrt();
        }
    }

    // strongest peaks, greedily separated — real clicks stand far above
    // noise even at very low mic levels
    let mean = corr.iter().sum::<f32>() / n as f32;
    let var = corr.iter()
        .map(|&v| (v - mean) * (v - mean)).sum::<f32>() / n as f32;
    let thr = mean + 6.0 * var.sqrt();
    let min_sep = (0.2 * sr) as usize;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| corr[b].total_cmp(&corr[a]));
    let mut peaks: Vec<usize> = Vec::new();
    for i in order {
        if corr[i] < thr || peaks.len() == N_CLICKS {
            break;
        }
        if peaks.iter().all(|&p| i.abs_diff(p) > min_sep) {
            peaks.push(i);
        }
    }
    peaks.sort_unstable();

    // heard onset time per peak, then delay = heard - emitted
    let onset: Vec<f64> = peaks.iter().map(|&i| {
        t_end.checked_sub(Duration::from_secs_f64(
            (total - i) as f64 / sr))
            .map(|t| t.saturating_duration_since(t0).as_secs_f64())
            .unwrap_or(f64::NEG_INFINITY)
    }).collect();
    let emits: Vec<f64> = offsets.iter()
        .map(|&o| o as f64 / sr).collect();
    let gap_s = GAP_MS / 1000.0;
    let mut delays: Vec<f64> = emits.iter().filter_map(|&e| {
        onset.iter()
            .find(|&&t| t >= e && t <= e + gap_s)
            .map(|&t| t - e)
    }).collect();
    delays.retain(|&d| (0.0..=2.0).contains(&d));
    delays.sort_by(|a, b| a.total_cmp(b));
    if delays.is_empty() {
        return Err("no clicks heard — is the volume up?".into());
    }
    Ok(delays[delays.len() / 2] * 1000.0)
}
