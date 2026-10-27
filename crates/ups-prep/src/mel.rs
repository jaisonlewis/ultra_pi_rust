//! RMVPE frontend — log-mel spectrogram, port of rmvpe_onnx's
//! MelSpectrogram (librosa-compatible, htk=True convention).

use rustfft::num_complex::Complex;

pub const SR: u32 = 16000;
pub const N_MELS: usize = 128;
pub const WIN: usize = 1024;
pub const HOP: usize = 160;
pub const FMIN: f32 = 30.0;
pub const FMAX: f32 = 8000.0;
const CLAMP: f32 = 1e-5;

/// hz → mel (HTK formula — what librosa uses with htk=True).
fn hz2mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}

/// librosa mel filterbank (htk, norm=slaney default) → (n_mels, n_fft/2+1)
fn mel_basis() -> Vec<Vec<f32>> {
    let n_bins = WIN / 2 + 1;
    let freqs: Vec<f32> = (0..n_bins)
        .map(|k| k as f32 * SR as f32 / WIN as f32)
        .collect();
    // mel points at fmin..fmax + n_mels
    let m0 = hz2mel(FMIN);
    let m1 = hz2mel(FMAX);
    let mpts: Vec<f32> = (0..N_MELS + 2)
        .map(|i| m0 + (m1 - m0) * i as f32 / (N_MELS + 1) as f32)
        .collect();
    let fpts: Vec<f32> =
        mpts.iter().map(|&m| 700.0 * (10f32.powf(m / 2595.0) - 1.0)).collect();
    let mut basis = vec![vec![0.0f32; n_bins]; N_MELS];
    for m in 0..N_MELS {
        let (lo, c, hi) = (fpts[m], fpts[m + 1], fpts[m + 2]);
        for (k, &f) in freqs.iter().enumerate() {
            basis[m][k] = ((f - lo) / (c - lo))
                .min((hi - f) / (hi - c))
                .max(0.0);
        }
        // slaney area norm (librosa norm="slaney" default)
        let enorm = 2.0 / (hi - lo);
        for v in &mut basis[m] {
            *v *= enorm;
        }
    }
    basis
}

/// 16k mono f32 → log-mel (128, T). Center-padded reflect, hann win,
/// same as librosa.stft defaults used by rmvpe_onnx.
pub fn log_mel(audio: &[f32]) -> Vec<Vec<f32>> {
    // reflect-pad WIN/2 both ends
    let pad = WIN / 2;
    let mut sig = Vec::with_capacity(audio.len() + 2 * pad);
    for i in (1..=pad).rev() {
        sig.push(audio.get(i).copied().unwrap_or(0.0));
    }
    sig.extend_from_slice(audio);
    let n = audio.len();
    for i in 0..pad {
        sig.push(if n >= 2 { audio[n - 2 - i.min(n - 2)] } else { 0.0 });
    }
    let n_fr = 1 + (sig.len() - WIN) / HOP;
    let win: Vec<f32> = (0..WIN)
        .map(|i| 0.5 - 0.5
            * (2.0 * std::f32::consts::PI * i as f32 / WIN as f32).cos())
        .collect();
    let basis = mel_basis();
    let fft = rustfft::FftPlanner::new().plan_fft_forward(WIN);
    let mut buf = vec![Complex::new(0.0f32, 0.0); WIN];
    let mut scr = vec![Complex::new(0.0f32, 0.0); WIN];
    let mut mel = vec![vec![0.0f32; n_fr]; N_MELS];
    for fr in 0..n_fr {
        for i in 0..WIN {
            buf[i] = Complex::new(sig[fr * HOP + i] * win[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scr);
        for m in 0..N_MELS {
            let mut acc = 0.0f32;
            for k in 0..=WIN / 2 {
                acc += basis[m][k] * buf[k].norm();
            }
            mel[m][fr] = acc.max(CLAMP).ln();
        }
    }
    mel
}
