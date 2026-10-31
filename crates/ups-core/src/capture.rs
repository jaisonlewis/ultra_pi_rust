//! Microphone capture — cpal input → shared pitch detector → latest
//! absolute tone (0..45) + level under a lock. Fallible by design:
//! walks input devices until one opens instead of trusting defaults.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::pitch::PitchDetector;

pub struct MicCapture {
    /// latest detected absolute tone (None = no confident pitch)
    pub tone: Arc<Mutex<Option<i64>>>,
    /// smoothed 0..1 input level
    pub level: Arc<Mutex<f64>>,
    stop: Arc<AtomicBool>,
    _stream: cpal::Stream,
    worker: Option<JoinHandle<()>>,
}

// cpal marks Stream !Send for Web/Emscripten reasons; on native hosts
// the stream is owned here and only dropped from one place.
unsafe impl Send for MicCapture {}

impl MicCapture {
    /// Open the first capture-capable input device.
    pub fn open() -> Option<Self> {
        Self::open_named(std::env::var("UPS_MIC_DEVICE").ok().as_deref())
    }

    /// `name` selects a device by substring; None walks all devices.
    pub fn open_named(name: Option<&str>) -> Option<Self> {
        let host = cpal::default_host();
        let devs = host.input_devices().ok()?;
        for dev in devs {
            if let Some(want) = name {
                if !dev.name().unwrap_or_default().contains(want) {
                    continue;
                }
            }
            if let Some(c) = Self::open_dev(dev) {
                return Some(c);
            }
        }
        None
    }

    /// Up to `want` captures — one per physical input device, in
    /// enumeration order. Duets map track i to caps[i]; when fewer mics
    /// than tracks are plugged in the caller reuses the last capture.
    pub fn open_multi(want: usize) -> Vec<Self> {
        let host = cpal::default_host();
        let Ok(devs) = host.input_devices() else { return vec![] };
        devs.filter_map(Self::open_dev).take(want.max(1)).collect()
    }

    /// Open one specific device — stream + detector worker.
    fn open_dev(dev: cpal::Device) -> Option<Self> {
        let mut opened = None;
        for dev in std::iter::once(dev) {
            let Ok(cfg) = dev.default_input_config() else {
                continue;
            };
            let rate = cfg.sample_rate().0;
            // pipewire/loopback nodes can advertise rate 0 — a SamplesBuffer
            // built on it panics, and the detector math divides by it
            if rate == 0 {
                continue;
            }
            let shared = Arc::new(Mutex::new(VecDeque::<i16>::new()));
            let cfg_stream: cpal::StreamConfig = cfg.clone().into();
            // mono downmix — interleaved frames would pitch-shift the
            // detector and double every sample-count heuristic
            let ch = cfg.channels().max(1) as usize;
            let err_fn = |e: cpal::StreamError| eprintln!("mic error: {e}");
            let stream = match cfg.sample_format() {
                cpal::SampleFormat::I16 => {
                    let buf = Arc::clone(&shared);
                    dev.build_input_stream(
                        &cfg_stream,
                        move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            let mut g = buf.lock().unwrap();
                            g.extend(data.chunks(ch).map(|f| f[0]));
                            while g.len() > 8192 {
                                g.pop_front();
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::F32 => {
                    let buf = Arc::clone(&shared);
                    dev.build_input_stream(
                        &cfg_stream,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            let mut g = buf.lock().unwrap();
                            g.extend(data.chunks(ch).map(|f| {
                                (f[0].clamp(-1.0, 1.0) * 32767.0) as i16
                            }));
                            while g.len() > 8192 {
                                g.pop_front();
                            }
                        },
                        err_fn,
                        None,
                    )
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

        let tone = Arc::new(Mutex::new(None::<i64>));
        let level = Arc::new(Mutex::new(0.0f64));
        let stop = Arc::new(AtomicBool::new(false));
        let (tone_w, level_w, stop_w) =
            (Arc::clone(&tone), Arc::clone(&level), Arc::clone(&stop));
        let worker = std::thread::spawn(move || {
            let mut det = PitchDetector::new(rate, 2);
            while !stop_w.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(30));
                let chunk: Vec<i16> = {
                    let mut g = shared.lock().unwrap();
                    g.drain(..).collect()
                };
                if !chunk.is_empty() {
                    let mut sum = 0f64;
                    for &s in &chunk {
                        sum += (s as f64).abs();
                    }
                    *level_w.lock().unwrap() =
                        (sum / chunk.len() as f64 / 32768.0).min(1.0);
                    det.feed(&chunk);
                }
                let pr = det.analyze();
                *tone_w.lock().unwrap() =
                    if pr.valid { Some(pr.tone_abs as i64) } else { None };
            }
        });
        Some(MicCapture {
            tone,
            level,
            stop,
            _stream: stream,
            worker: Some(worker),
        })
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
