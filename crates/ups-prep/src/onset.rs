//! Vocal onset strength — spectral flux on the stem at 10 ms hops.
//! CTC emissions fire once the model has *enough* evidence, i.e. one
//! to three frames after the acoustic onset (80–240 ms on an 80 ms
//! model). Snapping each aligned word start back to the nearest flux
//! peak recovers the real attack.

use rustfft::num_complex::Complex;

pub const HOP_MS: f64 = 10.0;
const SR: usize = 16_000;
const N_FFT: usize = 512;
const HOP: usize = 160;
const N_BANDS: usize = 40;

/// Half-wave-rectified log-magnitude flux per 10 ms frame, scaled so
/// the 95th percentile is 1.0 (song-relative).
pub fn strength(audio16: &[f32]) -> Vec<f32> {
    if audio16.len() < N_FFT {
        return Vec::new();
    }
    let n_fr = 1 + (audio16.len() - N_FFT) / HOP;
    let win: Vec<f32> = (0..N_FFT).map(|i| 0.5 - 0.5
        * (2.0 * std::f32::consts::PI * i as f32 / N_FFT as f32).cos())
        .collect();
    // coarse mel-ish bands over 80 Hz .. 6 kHz (log spaced)
    let n_bins = N_FFT / 2 + 1;
    let edges: Vec<usize> = (0..=N_BANDS).map(|b| {
        let f = 80.0 * (6000.0f32 / 80.0).powf(b as f32 / N_BANDS as f32);
        ((f * N_FFT as f32 / SR as f32) as usize).min(n_bins - 1)
    }).collect();
    let fft = rustfft::FftPlanner::new().plan_fft_forward(N_FFT);
    let mut buf = vec![Complex::new(0.0f32, 0.0); N_FFT];
    let mut scr = vec![Complex::new(0.0f32, 0.0); N_FFT];
    let mut prev = vec![0.0f32; N_BANDS];
    let mut out = Vec::with_capacity(n_fr);
    for fr in 0..n_fr {
        for i in 0..N_FFT {
            buf[i] = Complex::new(audio16[fr * HOP + i] * win[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scr);
        let mut flux = 0.0f32;
        for b in 0..N_BANDS {
            let (lo, hi) = (edges[b], edges[b + 1].max(edges[b] + 1));
            let e: f32 = (lo..hi).map(|k| buf[k].norm_sqr()).sum::<f32>()
                / (hi - lo) as f32;
            let l = (e + 1e-6).ln();
            flux += (l - prev[b]).max(0.0);
            prev[b] = l;
        }
        out.push(flux);
    }
    // song-relative scale
    let mut sorted = out.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = sorted[sorted.len() * 95 / 100].max(1e-3);
    for v in &mut out {
        *v /= p95;
    }
    out
}

/// Strongest onset in [t_ms - back, t_ms + fwd]; None when nothing
/// there rises above `min` (song-relative units).
pub fn snap(strength: &[f32], t_ms: f64, back_ms: f64, fwd_ms: f64,
            min: f32) -> Option<f64> {
    let lo = ((t_ms - back_ms) / HOP_MS).max(0.0) as usize;
    let hi = (((t_ms + fwd_ms) / HOP_MS) as usize).min(strength.len().saturating_sub(1));
    if lo >= hi {
        return None;
    }
    // local peak = above both neighbours; pick the strongest, with a
    // mild preference for peaks nearer the CTC frame
    let mut best: Option<(f32, usize)> = None;
    for i in lo.max(1)..hi {
        let v = strength[i];
        if v < min || v < strength[i - 1] || v < strength[i + 1] {
            continue;
        }
        let dist = ((i as f64 * HOP_MS) - t_ms).abs() / (back_ms + fwd_ms);
        let score = v * (1.0 - 0.3 * dist as f32);
        if best.is_none_or(|b| score > b.0) {
            best = Some((score, i));
        }
    }
    best.map(|(_, i)| i as f64 * HOP_MS)
}
