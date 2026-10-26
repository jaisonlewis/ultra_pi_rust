//! Vocal separation via the sherpa-onnx source-separation binary —
//! same ONNX models as Python's sherpa-onnx API, shelled out like
//! ffmpeg. Binary comes from sherpa-onnx releases (win/linux/mac).

use anyhow::{bail, Context, Result};
use rustfft::{num_complex::Complex, Fft};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::ff;

const SR: u32 = 44100;

/// Locate the separation binary: PATH, then `<models>/bin/`.
fn sep_bin(models: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "sherpa-onnx-offline-source-separation.exe"
    } else {
        "sherpa-onnx-offline-source-separation"
    };
    let local = models.join("bin").join(name);
    if local.exists() {
        return Some(local);
    }
    // PATH lookup via a cheap probe
    Command::new(name)
        .arg("--help")
        .output()
        .ok()
        .map(|_| PathBuf::from(name))
}

/// True when the spleeter int8 pair + binary are both available.
pub fn have_models(models: &Path) -> bool {
    models.join("sherpa-onnx-spleeter-2stems-int8/vocals.int8.onnx")
        .exists()
        && models.join(
            "sherpa-onnx-spleeter-2stems-int8/accompaniment.int8.onnx")
            .exists()
        && sep_bin(models).is_some()
}

/// Run spleeter on `media` → (vocals_stereo_f32, accompaniment) at 44.1k,
/// each (2, n) channel-major.
pub fn separate_stems(media: &Path, models: &Path)
                      -> Result<(Vec<Vec<f32>>, Vec<Vec<f32>>)> {
    let bin = sep_bin(models)
        .context("sherpa-onnx-offline-source-separation not found — \
                  put it on PATH or <models>/bin/")?;
    let dir = models.join("sherpa-onnx-spleeter-2stems-int8");
    let tmp = std::env::temp_dir();
    let pid = std::process::id();
    let in_wav = tmp.join(format!("ups-src-{pid}.wav"));
    let voc_wav = tmp.join(format!("ups-voc-{pid}.wav"));
    let acc_wav = tmp.join(format!("ups-acc-{pid}.wav"));
    // spleeter wants 44.1k stereo wav input
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(media)
        .args(["-ar", "44100", "-ac", "2"])
        .arg(&in_wav)
        .status()?;
    if !st.success() {
        bail!("ffmpeg -> wav failed");
    }
    let st = Command::new(&bin)
        .arg(format!("--spleeter-vocals={}",
                     dir.join("vocals.int8.onnx").display()))
        .arg(format!("--spleeter-accompaniment={}",
                     dir.join("accompaniment.int8.onnx").display()))
        .arg("--num-threads=3")
        .arg(format!("--input-wav={}", in_wav.display()))
        .arg(format!("--output-vocals-wav={}", voc_wav.display()))
        .arg(format!("--output-accompaniment-wav={}",
                     acc_wav.display()))
        .status()
        .context("run source separation")?;
    let _ = std::fs::remove_file(&in_wav);
    if !st.success() {
        bail!("source separation failed");
    }
    let (voc_i, _, vc) = ff::read_wav_f32(&voc_wav)?;
    let (acc_i, _, ac) = ff::read_wav_f32(&acc_wav)?;
    let _ = std::fs::remove_file(&voc_wav);
    let _ = std::fs::remove_file(&acc_wav);
    Ok((ff::deinterleave(&voc_i, vc), ff::deinterleave(&acc_i, ac)))
}

/// accompaniment + rescued low end + a faint guide vocal — port of
/// Python's karaoke_mix. Spleeter int8 dumps center-panned music into
/// the vocal stem; everything below `bass_hz` is leaked instruments and
/// returns wholesale, `guide` fraction brings back the rest quietly.
pub fn karaoke_mix(vocals: &[Vec<f32>], accomp: &[Vec<f32>],
                   guide: f32, bass_hz: f32) -> Vec<f32> {
    let n = vocals[0].len();
    let fft: Arc<dyn Fft<f32>> =
        rustfft::FftPlanner::new().plan_fft_forward(n);
    let ifft: Arc<dyn Fft<f32>> =
        rustfft::FftPlanner::new().plan_fft_inverse(n);
    let mut inter = Vec::with_capacity(n * 2);
    let mut buf = vec![Complex::new(0.0f32, 0.0); n];
    let mut spec = vec![Complex::new(0.0f32, 0.0); n];
    for ch in 0..2 {
        for (i, &s) in vocals[ch].iter().enumerate() {
            buf[i] = Complex::new(s, 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut spec);
        // smooth rolloff — a brick-wall notch rings on transients
        let mut low = spec.clone();
        for (k, c) in low.iter_mut().enumerate() {
            let f = k as f32 * SR as f32 / n as f32;
            let w = ((bass_hz * 1.4 - f) / (bass_hz * 0.8))
                .clamp(0.0, 1.0);
            *c *= w;
        }
        let mut low_t = low.clone();
        ifft.process_with_scratch(&mut low_t, &mut spec);
        let scale = 1.0 / n as f32;
        for i in 0..n {
            let lo = low_t[i].re * scale;
            let v = vocals[ch][i];
            let a = accomp[ch].get(i).copied().unwrap_or(0.0);
            inter.push(a + lo + guide * (v - lo));
        }
    }
    // deinterleaved push was per-channel — fix ordering below
    // (channels appended; caller expects interleaved)
    let mut out = vec![0.0f32; n * 2];
    for ch in 0..2 {
        for i in 0..n {
            out[i * 2 + ch] = inter[ch * n + i];
        }
    }
    let peak = out.iter().fold(0.0f32, |p, s| p.max(s.abs()));
    if peak > 1.0 {
        for s in &mut out {
            *s /= peak;
        }
    }
    out
}

/// vocal stem → 16k mono f32 via ffmpeg resample (librosa-equivalent).
pub fn mono16(vocals: &[Vec<f32>]) -> Result<Vec<f32>> {
    let tmp = std::env::temp_dir()
        .join(format!("ups-v44-{}.wav", std::process::id()));
    let inter: Vec<f32> = (0..vocals[0].len())
        .flat_map(|i| [vocals[0][i], vocals[1][i]])
        .collect();
    ff::write_wav(&tmp, &inter, SR, 2)?;
    let out = ff::decode_f32(&tmp, 16000, 1);
    let _ = std::fs::remove_file(&tmp);
    out
}
