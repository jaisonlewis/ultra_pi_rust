//! RMVPE pitch extraction — ONNX inference + cents-domain decode,
//! port of rmvpe_onnx.RMVPE.predict.

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::mel::{self, HOP, SR};

const N_CLASS: usize = 360;
const CENTS_OFFSET: f32 = 1997.3794084376191;

/// (times_s, f0_hz, conf) per ~10ms frame.
pub struct Curve {
    pub times: Vec<f64>,
    pub f0: Vec<f32>,
    pub conf: Vec<f32>,
}

pub fn have_model(models: &Path) -> bool {
    models.join("rmvpe.onnx").exists()
}

fn model_path(models: &Path) -> PathBuf {
    models.join("rmvpe.onnx")
}

/// 16k mono f32 → RMVPE curve.
pub fn extract_curve(audio16: &[f32], models: &Path) -> Result<Curve> {
    // ort allows exactly one Environment per process — share it
    static ENV: OnceLock<ort::environment::Environment> =
        OnceLock::new();
    let env = ENV.get_or_init(|| {
        ort::init().with_name("ups-prep").build()
            .expect("ort environment")
    });
    let mut sess = Session::builder(env)?
        .commit_from_file(model_path(models))
        .context("load rmvpe.onnx")?;
    let mel = mel::log_mel(audio16);
    let n_fr = mel[0].len();
    if n_fr == 0 {
        return Ok(Curve { times: vec![], f0: vec![], conf: vec![] });
    }
    // pad frames to a multiple of 32 (model's internal chunk)
    let n_pad = 32 * ((n_fr - 1) / 32 + 1) - n_fr;
    let t_tot = n_fr + n_pad;
    let mut flat = Vec::with_capacity(128 * t_tot);
    for m in 0..128 {
        flat.extend_from_slice(&mel[m]);
        flat.extend(std::iter::repeat(0.0f32).take(n_pad));
    }
    let input = Tensor::from_array(([1usize, 128, t_tot], flat))?;
    let out = sess.run(ort::inputs![input])?;
    let (shape, data) = out[0].try_extract_tensor::<f32>()?;
    let (_b, _t, _c) = (shape[0], shape[1], shape[2]);
    let act: &[f32] = data;                     // (1, T_pad, 360)

    // cents mapping: 20*i + OFFSET, padded 4 each side
    let mut cents_map = vec![0.0f32; N_CLASS + 8];
    for i in 0..N_CLASS {
        cents_map[i + 4] = 20.0 * i as f32 + CENTS_OFFSET;
    }
    let hop_s = HOP as f64 / SR as f64;
    let mut curve = Curve {
        times: Vec::with_capacity(n_fr),
        f0: Vec::with_capacity(n_fr),
        conf: Vec::with_capacity(n_fr),
    };
    for fr in 0..n_fr {
        let row = &act[fr * N_CLASS..(fr + 1) * N_CLASS];
        let center = row.iter().enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i).unwrap_or(0);
        let conf = row[center];
        // local average of ±4 bins in cents domain
        let c = center + 4;
        let mut wsum = 0.0f32;
        let mut csum = 0.0f32;
        for k in c - 4..=c + 4 {
            // np.pad(constant=0): salience reads 0 outside [4, 4+360)
            let s = if (4..4 + N_CLASS).contains(&k) {
                row[k - 4]
            } else {
                0.0
            };
            wsum += s;
            csum += s * cents_map[k];
        }
        let cents = if wsum > 0.0 { csum / wsum } else { CENTS_OFFSET };
        curve.times.push(fr as f64 * hop_s);
        curve.f0.push(10.0 * 2f32.powf(cents / 1200.0));
        curve.conf.push(conf);
    }
    Ok(curve)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diag_rmvpe() {
        let models = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../models");
        let wav = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../ups-test/unknown - yt-dQw4w9WgXcQ/yt-dQw4w9WgXcQ.vocals.wav");
        if !model_path(&models).exists() || !wav.exists() {
            eprintln!("skip: rmvpe.onnx/test wav absent");
            return;
        }
        let (a, sr, ch) = crate::ff::read_wav_f32(&wav).unwrap();
        println!("audio {} samples sr={} ch={}", a.len(), sr, ch);
        let mel = crate::mel::log_mel(&a[..16000 * 60.min(a.len() / 16000)]);
        println!("mel {}x{} m[0][..4]={:?} m[60][..4]={:?} m[100][..4]={:?}",
                 mel.len(), mel[0].len(),
                 &mel[0][..4], &mel[60][..4], &mel[100][..4]);
        let c = extract_curve(&a, &models).unwrap();
        let mut confs: Vec<f32> = c.conf.clone();
        confs.sort_by(|x, y| x.partial_cmp(y).unwrap());
        println!("frames={} conf p50={:.3} p90={:.3} max={:.3}",
                 c.times.len(), confs[confs.len() / 2],
                 confs[confs.len() * 9 / 10], confs.last().copied().unwrap_or(0.0));
        let voiced = c.f0.iter().zip(&c.conf)
            .filter(|(f, cf)| **f > 0.0 && **cf >= 0.3).count();
        println!("voiced={}", voiced);
    }
}
