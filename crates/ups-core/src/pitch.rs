//! Pitch detection — Rust port of UltraStar WorldParty's
//! `TCaptureBuffer` analyzer (`src/base/URecord.pas`, GPL-3.0),
//! via `ultrapistar/pitch.py`.
//!
//!   * keep the newest 4096 int16 samples in an analysis buffer
//!   * gate on volume: max amplitude of the first 1024 samples must
//!     clear a threshold (0.05/0.10/0.15/0.20 by ThresholdIndex)
//!   * score each of 46 halftone candidates C2..A5 by one-period-lag
//!     autocorrelation: weight = 1 - mean(|s[i] - s[i+lag]|)/65535
//!   * best weight wins -> tone_abs (0..45) and tone (0..11)

use std::collections::VecDeque;

pub const BASE_TONE_FREQ: f64 = 65.4064; // lowest analyzed tone: C2
pub const NUM_HALFTONES: usize = 46; // C2..A5
pub const ANALYSIS_BUFFER_SIZE: usize = 4096;
pub const VOLUME_WINDOW: usize = 1024;
pub const THRESHOLDS: [f64; 4] = [0.05, 0.10, 0.15, 0.20];

pub const TONE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// candidate frequencies, C2..A5
pub fn tone_freq(i: usize) -> f64 {
    BASE_TONE_FREQ * 2f64.powf(i as f64 / 12.0)
}

#[derive(Debug, Clone, Copy)]
pub struct PitchResult {
    pub valid: bool,
    pub tone: i32,     // semitone within octave, 0..11
    pub tone_abs: i32, // 0..45 (C2..A5)
    pub volume: f64,
}

impl PitchResult {
    pub const INVALID: Self =
        PitchResult { valid: false, tone: -1, tone_abs: -1, volume: 0.0 };

    /// Port of TCaptureBuffer.GetToneString — e.g. "A4".
    pub fn tone_str(&self) -> String {
        if !self.valid {
            return "-".into();
        }
        format!("{}{}", TONE_NAMES[self.tone as usize], self.tone_abs / 12 + 2)
    }
    pub fn freq(&self) -> f64 {
        if self.valid {
            tone_freq(self.tone_abs as usize)
        } else {
            0.0
        }
    }
}

/// Streaming pitch detector; feed it int16 mono PCM chunks.
pub struct PitchDetector {
    sample_rate: u32,
    threshold: f64,
    /// newest 4096 samples (upstream: AnalysisBuffer ring of 4096)
    buffer: VecDeque<i16>,
    /// lag per candidate tone (samples per period)
    lags: Vec<usize>,
}

impl PitchDetector {
    pub fn new(sample_rate: u32, threshold_index: usize) -> Self {
        PitchDetector {
            sample_rate,
            threshold: THRESHOLDS[threshold_index.clamp(0, 3)],
            buffer: VecDeque::with_capacity(ANALYSIS_BUFFER_SIZE + 512),
            lags: (0..NUM_HALFTONES)
                .map(|i| {
                    (sample_rate as f64 / tone_freq(i)).round().max(1.0) as usize
                })
                .collect(),
        }
    }

    /// Append int16 mono PCM; keeps the newest 4096 samples.
    pub fn feed(&mut self, pcm: &[i16]) {
        self.buffer.extend(pcm.iter().copied());
        while self.buffer.len() > ANALYSIS_BUFFER_SIZE {
            self.buffer.pop_front();
        }
    }

    /// Port of TCaptureBuffer.AnalyzeBuffer.
    pub fn analyze(&self) -> PitchResult {
        let n = self.buffer.len();
        if n < VOLUME_WINDOW {
            return PitchResult::INVALID;
        }
        let volume = (0..VOLUME_WINDOW)
            .map(|i| self.buffer[i] as i32)
            .fold(0i32, |a, b| a.max(b.abs())) as f64
            / 32768.0;
        if volume < self.threshold {
            return PitchResult { valid: false, volume, ..PitchResult::INVALID };
        }

        let buf: Vec<i32> = self.buffer.iter().map(|&s| s as i32).collect();
        let mut weights = [-1.0f64; NUM_HALFTONES];
        for (tone, &lag) in self.lags.iter().enumerate() {
            if lag >= n {
                continue;
            }
            // upstream divides by buffer size, not sample count — kept faithful
            let accum: i64 = (0..n - lag)
                .map(|i| (buf[i] - buf[i + lag]).abs() as i64)
                .sum();
            weights[tone] = 1.0 - accum as f64 / 65535.0 / n as f64;
        }
        let best_w = weights.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        // a pure tone also correlates at lags that are integer multiples of
        // its period (subharmonics win on weight alone); among weights within
        // EPS of the maximum prefer the *highest* frequency
        const EPS: f64 = 0.01;
        let best_tone = weights
            .iter()
            .rposition(|&w| w >= best_w - EPS)
            .unwrap_or(0) as i32;
        PitchResult {
            valid: true,
            tone: best_tone % 12,
            tone_abs: best_tone,
            volume,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Nearest tone_abs index for a Hz value (C2=0 .. A5=45).
pub fn freq_to_tone_abs(freq: f64) -> i32 {
    if freq <= 0.0 {
        return -1;
    }
    (0..NUM_HALFTONES)
        .min_by(|&a, &b| {
            (freq / tone_freq(a))
                .log2()
                .abs()
                .partial_cmp(&(freq / tone_freq(b)).log2().abs())
                .unwrap()
        })
        .map(|i| i as i32)
        .unwrap_or(-1)
}
