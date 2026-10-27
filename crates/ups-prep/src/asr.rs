//! ASR word timing via sherpa-onnx-offline + NeMo Parakeet TDT.
//! TDT emits per-token frame times natively — real word onsets, no
//! DTW/attention post-processing. BPE tokens group into words on
//! the leading-space boundary.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn asr_bin(models: &Path) -> Option<PathBuf> {
    crate::models::find_bin(models, if cfg!(windows) {
        "sherpa-onnx-offline.exe"
    } else {
        "sherpa-onnx-offline"
    })
}

/// Model dir holding encoder/decoder/joiner int8 ONNX + tokens.txt.
fn model_dir(models: &Path) -> Option<PathBuf> {
    let d = models.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8");
    d.join("encoder.int8.onnx").exists().then_some(d)
}

pub fn have_models(models: &Path) -> bool {
    asr_bin(models).is_some() && model_dir(models).is_some()
}

/// A transcribed word with its onset, media-time ms.
pub struct AsrWord {
    pub word: String,
    pub start_ms: f64,
    pub end_ms: f64,
}

/// Transcribe a wav (any rate — the recognizer resamples) → words
/// with onsets. Feed it the vocal stem when separated.
pub fn transcribe(wav: &Path, models: &Path) -> Result<Vec<AsrWord>> {
    let bin = asr_bin(models)
        .context("sherpa-onnx-offline not on PATH or <models>/bin/")?;
    let mdir = model_dir(models)
        .context("parakeet-tdt-0.6b-v3-int8 model dir missing")?;
    let out = Command::new(&bin)
        .arg(format!("--encoder={}",
                     mdir.join("encoder.int8.onnx").display()))
        .arg(format!("--decoder={}",
                     mdir.join("decoder.int8.onnx").display()))
        .arg(format!("--joiner={}",
                     mdir.join("joiner.int8.onnx").display()))
        .arg(format!("--tokens={}",
                     mdir.join("tokens.txt").display()))
        .arg("--num-threads=4")
        .arg(wav)
        .output()
        .context("run sherpa-onnx-offline")?;
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout),
                       String::from_utf8_lossy(&out.stderr));
    // last line starting with '{' is the JSON result
    let json = text.lines().rev()
        .find(|l| l.trim_start().starts_with('{'))
        .context("no recognition json")?;
    let v: serde_json::Value =
        serde_json::from_str(json).context("parse asr json")?;

    let tokens = v["tokens"].as_array().cloned().unwrap_or_default();
    let tss = v["timestamps"].as_array().cloned().unwrap_or_default();
    let durs = v["durations"].as_array().cloned().unwrap_or_default();
    if tokens.is_empty() || tss.len() != tokens.len() {
        bail!("asr returned no token timestamps");
    }
    // group BPE tokens into words — a token beginning a word starts
    // with ' ' (or '▁'); word start = first token's onset
    let mut words: Vec<AsrWord> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let tok = t.as_str().unwrap_or("");
        let t0 = tss[i].as_f64().unwrap_or(0.0) * 1000.0;
        let dur = durs.get(i).and_then(|d| d.as_f64())
            .unwrap_or(0.16) * 1000.0;
        let new_word = tok.starts_with([' ', '▁']) && i > 0;
        if words.is_empty() || new_word {
            words.push(AsrWord {
                word: tok.trim_start_matches([' ', '▁']).to_string(),
                start_ms: t0,
                end_ms: t0 + dur,
            });
        } else {
            let w = words.last_mut().unwrap();
            w.word.push_str(tok);
            w.end_ms = t0 + dur;
        }
    }
    words.retain(|w| !w.word.trim().is_empty());
    Ok(words)
}

/// Transcribe for charting — vocal stem first (cleaner word onsets),
/// raw media when the stem transcribes to nothing (parakeet can choke
/// on spleeter's phasey artifacts; full audio stays decodable).
pub fn transcribe_media(media: &Path, stem: Option<&Path>,
                        models: &Path) -> Result<Vec<AsrWord>> {
    if let Some(stem) = stem {
        if stem.exists() {
            match transcribe(stem, models) {
                Ok(w) if w.len() >= 20 => return Ok(w),
                _ => {}
            }
        }
    }
    let tmp = std::env::temp_dir()
        .join(format!("ups-asr-{}.wav", std::process::id()));
    let mono = crate::ff::decode_f32(media, 16000, 1)?;
    crate::ff::write_wav(&tmp, &mono, 16000, 1)?;
    let r = transcribe(&tmp, models);
    let _ = std::fs::remove_file(&tmp);
    r
}
