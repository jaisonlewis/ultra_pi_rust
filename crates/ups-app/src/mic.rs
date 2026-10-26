//! Mic capture for the sing session — cpal input into the shared pitch
//! detector, latest tone per track published under a mutex so the
//! socket loop can read it on every player:position tick.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

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
