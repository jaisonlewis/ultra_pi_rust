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

// ---- NeMo fbank (parakeet / FastConformer CTC front-end) --------------

/// NeMo `AudioToMelSpectrogramPreprocessor` defaults as exported for
/// parakeet: pre-emphasis 0.97, 25 ms Hann in a 512 FFT, 10 ms hop,
/// centre reflect pad, power spectrum, 80 slaney-scale mel bins over
/// 20 Hz..7.6 kHz, `log(x + 2^-24)`, then per-feature mean/var normalisation
/// over the utterance. Returns (feat-major flat [80*T], T).
pub fn nemo_fbank(audio: &[f32]) -> (Vec<f32>, usize) {
    const N_FFT: usize = 512;
    const WINL: usize = 400;
    const HOPN: usize = 160;
    const NM: usize = 80;
    const GUARD: f32 = 5.960_464_5e-8; // 2^-24
    if audio.len() < HOPN {
        return (Vec::new(), 0);
    }
    // pre-emphasis
    let mut x = Vec::with_capacity(audio.len());
    x.push(audio[0]);
    for i in 1..audio.len() {
        x.push(audio[i] - 0.97 * audio[i - 1]);
    }
    // reflect pad n_fft/2
    let pad = N_FFT / 2;
    let n = x.len();
    let mut sig = Vec::with_capacity(n + 2 * pad);
    for i in (1..=pad).rev() {
        sig.push(x[i.min(n - 1)]);
    }
    sig.extend_from_slice(&x);
    for i in 0..pad {
        sig.push(x[n.saturating_sub(2 + i).min(n - 1)]);
    }
    let t = 1 + (sig.len() - N_FFT) / HOPN;
    // periodic hann of WINL, centred inside the FFT frame
    let off = (N_FFT - WINL) / 2;
    let mut win = vec![0.0f32; N_FFT];
    for i in 0..WINL {
        win[off + i] = 0.5 - 0.5
            * (2.0 * std::f32::consts::PI * i as f32 / WINL as f32).cos();
    }
    // slaney mel basis
    let n_bins = N_FFT / 2 + 1;
    let hz2mel = |f: f32| -> f32 {
        if f < 1000.0 { f / (200.0 / 3.0) }
        else { 15.0 + (f / 1000.0).ln() / (6.4f32.ln() / 27.0) }
    };
    let mel2hz = |m: f32| -> f32 {
        if m < 15.0 { m * (200.0 / 3.0) }
        else { 1000.0 * ((m - 15.0) * (6.4f32.ln() / 27.0)).exp() }
    };
    // sherpa's nemo front-end: low_freq=20, high_freq=-400 (nyquist-400)
    let (m0, m1) = (hz2mel(20.0), hz2mel(SR as f32 / 2.0 - 400.0));
    let fpts: Vec<f32> = (0..NM + 2)
        .map(|i| mel2hz(m0 + (m1 - m0) * i as f32 / (NM + 1) as f32))
        .collect();
    let freqs: Vec<f32> = (0..n_bins)
        .map(|k| k as f32 * SR as f32 / N_FFT as f32).collect();
    let mut basis = vec![vec![0.0f32; n_bins]; NM];
    for m in 0..NM {
        let (lo, c, hi) = (fpts[m], fpts[m + 1], fpts[m + 2]);
        let enorm = 2.0 / (hi - lo);
        for (k, &f) in freqs.iter().enumerate() {
            basis[m][k] = (((f - lo) / (c - lo)).min((hi - f) / (hi - c)))
                .max(0.0) * enorm;
        }
    }
    let fft = rustfft::FftPlanner::new().plan_fft_forward(N_FFT);
    let mut buf = vec![Complex::new(0.0f32, 0.0); N_FFT];
    let mut scr = vec![Complex::new(0.0f32, 0.0); N_FFT];
    let mut feat = vec![0.0f32; NM * t];
    for fr in 0..t {
        for i in 0..N_FFT {
            buf[i] = Complex::new(sig[fr * HOPN + i] * win[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scr);
        for m in 0..NM {
            let mut acc = 0.0f32;
            for k in 0..n_bins {
                acc += basis[m][k] * buf[k].norm_sqr();
            }
            feat[m * t + fr] = (acc + GUARD).ln();
        }
    }
    // per-feature normalisation (unbiased std, as torch)
    for m in 0..NM {
        let row = &mut feat[m * t..(m + 1) * t];
        let mean = row.iter().sum::<f32>() / t as f32;
        let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>()
            / (t.max(2) - 1) as f32;
        let sd = var.sqrt() + 1e-5;
        for v in row.iter_mut() {
            *v = (*v - mean) / sd;
        }
    }
    (feat, t)
}
