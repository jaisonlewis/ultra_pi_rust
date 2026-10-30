//! ASR word timing via sherpa-onnx-offline.
//!
//! Engines, picked per song by detected language:
//!   parakeet-tdt-0.6b (en) — native per-token frame times, the
//!     sharpest English word onsets we have
//!   canary-180m-flash (en/es/de/fr) — a transducer built for singing;
//!     far better sung text than whisper but emits NO timestamps, so
//!     it runs chunked and words spread across each chunk window
//!   sense-voice (zh/en/ja/ko/yue) — fast CTC-ish encoder with
//!     per-token frame times; orthography on singing is rough but
//!     the onsets are real
//!   omnilingual-300M (≈1600 languages) — Meta CTC model; token
//!     timestamps for every language sherpa can't cover natively
//!     (hi/bn/ta/te/th/vi/…) — orthography drifts on singing, so it
//!     anchors lyric lines more than it transcribes
//!   whisper (multilingual) — auto-detects the spoken language and
//!     transcribes ~99 of them; emits segment boundaries rather than
//!     token times. Kept for language detection and as the fallback
//!     when no language-specific engine is installed.
//!
//! Routing: whisper-tiny runs a short detection pass over vocal
//! chunks first and votes a language. English lands on parakeet for
//! the tighter timings; es/de/fr on canary; zh/ja/ko/yue on
//! sensevoice; other languages on omnilingual when installed, else
//! whisper. No whisper → parakeet-only English assumption.

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

/// sherpa-onnx-whisper-* dirs under `models`, best quality first
/// (small > base > tiny). English-locked `*.en` dirs are skipped —
/// their tokens files name `tiny.en-*` etc. so they fail the
/// multilingual file check below.
fn whisper_dirs(models: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(models) {
        for e in rd.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str())
                .map(str::to_string) else { continue };
            let Some(size) = name
                .strip_prefix("sherpa-onnx-whisper-") else { continue };
            if size.ends_with(".en") {
                continue;
            }
            let parts = whisper_parts(&p, size);
            if parts.is_some() {
                out.push((size.to_string(), p));
            }
        }
    }
    let rank = |s: &str| match s { "small" => 0, "base" => 1, _ => 2 };
    out.sort_by_key(|(s, _)| rank(s));
    out.into_iter().map(|(_, p)| p).collect()
}

/// (encoder, decoder, tokens.txt) inside a whisper model dir —
/// int8 preferred, fp32 accepted.
fn whisper_parts(dir: &Path, size: &str)
        -> Option<(PathBuf, PathBuf, PathBuf)> {
    for enc in [
        format!("{size}-encoder.int8.onnx"),
        format!("{size}-encoder.onnx"),
    ] {
        let enc = dir.join(&enc);
        if !enc.exists() {
            continue;
        }
        let stem = enc.file_stem()?.to_str()?
            .strip_suffix("-encoder")?.to_string();
        let dec = dir.join(format!("{stem}-decoder.onnx"));
        let tok = dir.join(format!("{stem}-tokens.txt"));
        if dec.exists() && tok.exists() {
            return Some((enc, dec, tok));
        }
    }
    None
}

/// canary-180m-flash en/es/de/fr model dir (int8 or fp32 files).
fn canary_dir(models: &Path) -> Option<PathBuf> {
    for n in [
        "sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8",
        "sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr",
    ] {
        let d = models.join(n);
        if d.join("encoder.int8.onnx").exists()
            || d.join("encoder.onnx").exists()
        {
            return Some(d);
        }
    }
    None
}

/// sense-voice zh/en/ja/ko/yue model dir.
fn sensevoice_dir(models: &Path) -> Option<PathBuf> {
    if let Ok(rd) = std::fs::read_dir(models) {
        for e in rd.flatten() {
            let p = e.path();
            let Some(n) = p.file_name().and_then(|n| n.to_str())
                .map(str::to_string) else { continue };
            if n.starts_with("sherpa-onnx-sense-voice-")
                && p.join("model.int8.onnx").exists()
            {
                return Some(p);
            }
        }
    }
    None
}

/// omnilingual-asr 1600-language CTC model dir.
fn omni_dir(models: &Path) -> Option<PathBuf> {
    if let Ok(rd) = std::fs::read_dir(models) {
        for e in rd.flatten() {
            let p = e.path();
            let Some(n) = p.file_name().and_then(|n| n.to_str())
                .map(str::to_string) else { continue };
            if n.starts_with("sherpa-onnx-omnilingual-asr-")
                && p.join("model.int8.onnx").exists()
            {
                return Some(p);
            }
        }
    }
    None
}

pub fn have_models(models: &Path) -> bool {
    asr_bin(models).is_some()
        && (model_dir(models).is_some()
            || !whisper_dirs(models).is_empty()
            || canary_dir(models).is_some()
            || sensevoice_dir(models).is_some()
            || omni_dir(models).is_some())
}

/// Whisper multilingual model present under `models`.
pub fn have_whisper(models: &Path) -> bool {
    asr_bin(models).is_some() && !whisper_dirs(models).is_empty()
}

/// A transcribed word with its onset, media-time ms.
pub struct AsrWord {
    pub word: String,
    pub start_ms: f64,
    pub end_ms: f64,
}

/// Transcription result — words plus the language whisper heard.
pub struct AsrOut {
    pub words: Vec<AsrWord>,
    /// whisper-reported language code (en/hi/es/…); parakeet runs
    /// report "en" — it's all it knows
    pub lang: Option<String>,
    pub engine: &'static str,
}

/// Last '{'-led line of sherpa output is the result JSON.
fn result_json(out: &std::process::Output) -> Result<serde_json::Value> {
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout),
                       String::from_utf8_lossy(&out.stderr));
    let json = text.lines().rev()
        .find(|l| l.trim_start().starts_with('{'))
        .context("no recognition json")?;
    serde_json::from_str(json).context("parse asr json")
}

/// Group sherpa's token+timestamp arrays into words. CTC engines
/// (parakeet/omnilingual) and sensevoice all emit a token per
/// character/subword plus a frame time each; a token starting a new
/// word begins with ' ' or '▁'. Word start = first token's onset.
fn tokens_to_words(tokens: &[serde_json::Value],
                   tss: &[serde_json::Value],
                   durs: &[serde_json::Value],
                   offset_ms: f64) -> Vec<AsrWord> {
    let mut words: Vec<AsrWord> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let tok = t.as_str().unwrap_or("");
        let t0 = tss.get(i).and_then(|x| x.as_f64())
            .unwrap_or(0.0) * 1000.0 + offset_ms;
        let dur = durs.get(i).and_then(|d| d.as_f64())
            .unwrap_or(0.16) * 1000.0;
        // strip <|lang|>/<|tag|> special tokens sensevoice emits
        if tok.starts_with('<') || tok.trim().is_empty()
            && !tok.contains(' ') {
            continue;
        }
        let new_word = tok.starts_with([' ', '▁']) && !words.is_empty();
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
    words
}

/// Transcribe a wav via parakeet (any rate — the recognizer
/// resamples). Feed it the vocal stem when separated.
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
    let v = result_json(&out)?;

    let tokens = v["tokens"].as_array().cloned().unwrap_or_default();
    let tss = v["timestamps"].as_array().cloned().unwrap_or_default();
    let durs = v["durations"].as_array().cloned().unwrap_or_default();
    if tokens.is_empty() || tss.len() != tokens.len() {
        bail!("asr returned no token timestamps");
    }
    Ok(tokens_to_words(&tokens, &tss, &durs, 0.0))
}

/// One whisper decode of a ≤CHUNK_SECS wav — sherpa whisper only
/// transcribes its first ~30s window, so callers chunk.
fn whisper_once(wav: &Path, models: &Path, lang: Option<&str>)
        -> Result<(Vec<AsrWord>, Option<String>)> {
    let bin = asr_bin(models)
        .context("sherpa-onnx-offline not on PATH or <models>/bin/")?;
    let mdir = whisper_dirs(models).into_iter().next()
        .context("no whisper model under <models>")?;
    let size = mdir.file_name().and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("sherpa-onnx-whisper-"))
        .context("whisper dir name")?;
    let (enc, dec, tok) = whisper_parts(&mdir, size)
        .context("whisper model files missing")?;
    let mut cmd = Command::new(&bin);
    cmd.arg(format!("--whisper-encoder={}", enc.display()))
        .arg(format!("--whisper-decoder={}", dec.display()))
        .arg(format!("--tokens={}", tok.display()))
        .arg("--whisper-enable-segment-timestamps=true")
        .arg("--num-threads=4");
    if let Some(l) = lang {
        cmd.arg(format!("--whisper-language={l}"));
    }
    let out = cmd.arg(wav).output().context("run whisper")?;
    let v = result_json(&out)?;
    let lang = v["lang"].as_str().map(str::to_string);

    let segs = v["segment_texts"].as_array().cloned().unwrap_or_default();
    let stss = v["segment_timestamps"].as_array().cloned()
        .unwrap_or_default();
    let sdur = v["segment_durations"].as_array().cloned()
        .unwrap_or_default();
    let mut words: Vec<AsrWord> = Vec::new();
    for (i, st) in segs.iter().enumerate() {
        let text = st.as_str().unwrap_or("");
        let t0 = stss.get(i).and_then(|x| x.as_f64())
            .unwrap_or(0.0) * 1000.0;
        let dur = sdur.get(i).and_then(|x| x.as_f64())
            .unwrap_or(0.0) * 1000.0;
        let ws: Vec<&str> = text.split_whitespace().collect();
        if ws.is_empty() {
            continue;
        }
        let step = dur / ws.len() as f64;
        for (j, w) in ws.iter().enumerate() {
            words.push(AsrWord {
                word: w.to_string(),
                start_ms: t0 + step * j as f64,
                end_ms: t0 + step * (j + 1) as f64,
            });
        }
    }
    Ok((words, lang))
}

/// sherpa-whisper works on a ≤30s window — feed it CHUNK_SECS slices
/// and re-offset the results. Language comes from a majority vote
/// across chunks (whisper detects per call); `lang` pins it instead.
/// `detect_only` stops after the first chunk — the caller just wants
/// the language before deciding which engine to use.
const CHUNK_SECS: usize = 25;

fn transcribe_whisper(media: &Path, models: &Path,
                      lang: Option<&str>, detect_only: bool)
        -> Result<(Vec<AsrWord>, Option<String>)> {
    let samples = crate::ff::decode_f32(media, 16000, 1)?;
    let chunk_n = CHUNK_SECS * 16000;
    let mut words: Vec<AsrWord> = Vec::new();
    let mut votes: Vec<String> = Vec::new();
    let mut detect_lang: Option<String> = None;
    let tmp = std::env::temp_dir();
    for (ci, slice) in samples.chunks(chunk_n).enumerate() {
        // skip near-silent chunks — whisper hallucinates on them
        let energy: f32 = slice.iter()
            .map(|s| s * s).sum::<f32>() / slice.len() as f32;
        let cw = tmp.join(format!(
            "ups-wsp-{}-{ci}.wav", std::process::id()));
        crate::ff::write_wav(&cw, slice, 16000, 1)?;
        let r = whisper_once(&cw, models, lang.or(detect_lang.as_deref()));
        let _ = std::fs::remove_file(&cw);
        let Ok((mut w, l)) = r else { break };
        if energy < 1e-5 && w.len() < 8 {
            if detect_only && ci >= 3 {
                break;
            }
            continue;
        }
        if let Some(l) = l {
            votes.push(l.clone());
            if detect_lang.is_none() && !l.is_empty() {
                detect_lang = Some(l);
            }
        }
        let off = (ci * CHUNK_SECS * 1000) as f64;
        for w in w.iter_mut() {
            w.start_ms += off;
            w.end_ms += off;
        }
        words.extend(w);
        // detection pass: stop once the language shows up (or a few
        // chunks in — late vocals shouldn't stall detection)
        if detect_only && (detect_lang.is_some() || ci >= 3) {
            break;
        }
    }
    let lang = votes.iter()
        .max_by_key(|v| votes.iter().filter(|x| x == v).count())
        .cloned()
        .or(detect_lang);
    Ok((words, lang))
}

/// CTC-family engines (sensevoice / omnilingual) have no 30s
/// window, but whole-file runs of the big model blow up in memory
/// and wall-time — chunked like canary, with each token's frame
/// time shifted by its chunk offset (native word onsets, unlike
/// canary's spread).
fn transcribe_ctc(media: &Path, models: &Path, kind: &str,
                  lang: Option<&str>) -> Result<Vec<AsrWord>> {
    let bin = asr_bin(models)
        .context("sherpa-onnx-offline not on PATH or <models>/bin/")?;
    let args: Vec<String> = match kind {
        "sensevoice" => {
            let d = sensevoice_dir(models)
                .context("sense-voice model dir missing")?;
            vec![
                format!("--sense-voice-model={}",
                        d.join("model.int8.onnx").display()),
                format!("--tokens={}", d.join("tokens.txt").display()),
                "--sense-voice-use-itn=true".to_string(),
                format!("--sense-voice-language={}",
                        lang.unwrap_or("auto")),
            ]
        }
        "omnilingual" => {
            let d = omni_dir(models)
                .context("omnilingual-asr model dir missing")?;
            vec![
                format!("--omnilingual-asr-model={}",
                        d.join("model.int8.onnx").display()),
                format!("--tokens={}", d.join("tokens.txt").display()),
            ]
        }
        _ => bail!("unknown ctc engine {kind}"),
    };

    let samples = crate::ff::decode_f32(media, 16000, 1)?;
    let chunk_n = CHUNK_SECS * 16000;
    let tmp = std::env::temp_dir();
    let mut words: Vec<AsrWord> = Vec::new();
    for (ci, slice) in samples.chunks(chunk_n).enumerate() {
        let energy: f32 = slice.iter()
            .map(|s| s * s).sum::<f32>() / slice.len() as f32;
        if energy < 1e-5 {
            continue;
        }
        let cw = tmp.join(format!(
            "ups-ctc-{}-{ci}.wav", std::process::id()));
        crate::ff::write_wav(&cw, slice, 16000, 1)?;
        let out = Command::new(&bin)
            .args(&args)
            .arg("--num-threads=4")
            .arg(&cw)
            .output()
            .context("run sherpa ctc engine")?;
        let _ = std::fs::remove_file(&cw);
        let Ok(v) = result_json(&out) else { continue };
        let tokens = v["tokens"].as_array().cloned().unwrap_or_default();
        let tss = v["timestamps"].as_array().cloned()
            .unwrap_or_default();
        let durs = v["durations"].as_array().cloned()
            .unwrap_or_default();
        if tokens.is_empty() || tss.len() != tokens.len() {
            continue;
        }
        let off = (ci * CHUNK_SECS * 1000) as f64;
        words.extend(tokens_to_words(&tokens, &tss, &durs, off));
    }
    Ok(words)
}

/// Canary transcribes singing far better than whisper but emits no
/// timestamps — run it per CHUNK_SECS slice and spread each chunk's
/// words over its window; the lyric aligner only needs ordering +
/// coarse times since line timing comes from the LRC sources.
fn transcribe_canary(media: &Path, models: &Path, lang: &str)
        -> Result<Vec<AsrWord>> {
    let bin = asr_bin(models)
        .context("sherpa-onnx-offline not on PATH or <models>/bin/")?;
    let d = canary_dir(models).context("canary model dir missing")?;
    let pick = |n: &str| {
        let i8 = d.join(format!("{n}.int8.onnx"));
        if i8.exists() { i8 } else { d.join(format!("{n}.onnx")) }
    };
    let enc = pick("encoder");
    let dec = pick("decoder");
    if !enc.exists() || !dec.exists() {
        bail!("canary encoder/decoder missing");
    }
    let samples = crate::ff::decode_f32(media, 16000, 1)?;
    let chunk_n = CHUNK_SECS * 16000;
    let tmp = std::env::temp_dir();
    let mut words: Vec<AsrWord> = Vec::new();
    for (ci, slice) in samples.chunks(chunk_n).enumerate() {
        let energy: f32 = slice.iter()
            .map(|s| s * s).sum::<f32>() / slice.len() as f32;
        if energy < 1e-5 {
            continue; // silent window — canary hallucinates on it
        }
        let cw = tmp.join(format!(
            "ups-cny-{}-{ci}.wav", std::process::id()));
        crate::ff::write_wav(&cw, slice, 16000, 1)?;
        let out = Command::new(&bin)
            .arg(format!("--canary-encoder={}", enc.display()))
            .arg(format!("--canary-decoder={}", dec.display()))
            .arg(format!("--tokens={}", d.join("tokens.txt").display()))
            .arg(format!("--canary-src-lang={lang}"))
            .arg(format!("--canary-tgt-lang={lang}"))
            .arg("--num-threads=4")
            .arg(&cw)
            .output()
            .context("run canary")?;
        let _ = std::fs::remove_file(&cw);
        let Ok(v) = result_json(&out) else { continue };
        let text = v["text"].as_str().unwrap_or("");
        let ws: Vec<&str> = text.split_whitespace().collect();
        if ws.is_empty() {
            continue;
        }
        let t0 = (ci * CHUNK_SECS * 1000) as f64;
        let step = (CHUNK_SECS * 1000) as f64 / ws.len() as f64;
        for (j, w) in ws.iter().enumerate() {
            words.push(AsrWord {
                word: w.to_string(),
                start_ms: t0 + step * j as f64,
                end_ms: t0 + step * (j + 1) as f64,
            });
        }
    }
    Ok(words)
}

/// Which engine transcribes `lang` given installed model dirs.
/// Returns the route name for AsrOut.engine.
fn pick_engine(lang: &str, models: &Path) -> &'static str {
    match lang {
        "en" => {
            if model_dir(models).is_some() {
                "parakeet"
            } else if sensevoice_dir(models).is_some() {
                "sensevoice"
            } else if canary_dir(models).is_some() {
                "canary"
            } else {
                "whisper"
            }
        }
        "es" | "de" | "fr" => {
            if canary_dir(models).is_some() { "canary" } else {
                "whisper"
            }
        }
        "zh" | "ja" | "ko" | "yue" => {
            if sensevoice_dir(models).is_some() { "sensevoice" } else {
                "whisper"
            }
        }
        _ => {
            if omni_dir(models).is_some() { "omnilingual" } else {
                "whisper"
            }
        }
    }
}

/// Run the routed engine over `wav`. `lang` is the detected code.
fn run_engine(engine: &str, wav: &Path, models: &Path,
              lang: &str) -> Result<Vec<AsrWord>> {
    match engine {
        "parakeet" => transcribe(wav, models),
        "canary" => transcribe_canary(wav, models, lang),
        "sensevoice" =>
            transcribe_ctc(wav, models, "sensevoice", Some(lang)),
        "omnilingual" =>
            transcribe_ctc(wav, models, "omnilingual", None),
        _ => transcribe_whisper(wav, models, Some(lang), false)
            .map(|(w, _)| w),
    }
}

/// Transcribe for charting — vocal stem first (cleaner input), raw
/// media when the stem won't transcribe. Whisper does the language
/// detection pass; the detected code routes to the sharpest engine
/// installed for that language (canary/sensevoice/omnilingual/
/// parakeet/whisper). Returns words plus language + engine so the
/// caller can warn on uncovered languages.
pub fn transcribe_media(media: &Path, stem: Option<&Path>,
                        models: &Path) -> Result<AsrOut> {
    let stem_p = stem.filter(|p| p.exists());
    let tmp = std::env::temp_dir()
        .join(format!("ups-asr-{}.wav", std::process::id()));
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(s) = stem_p {
        candidates.push(s.to_path_buf());
    }

    // language detection over the vocal candidates — whisper-tiny's
    // detection pass costs a few chunked calls and answers
    // "which language?" even when its transcription would be weak
    let mut lang: Option<String> = None;
    if have_whisper(models) {
        for cand in &candidates {
            if let Ok((_, Some(l))) =
                transcribe_whisper(cand, models, None, true)
            {
                lang = Some(l);
                break;
            }
        }
        if lang.is_none() {
            if let Ok(wav) = decode16(media, &tmp) {
                if let Ok((_, l)) =
                    transcribe_whisper(&wav, models, None, true)
                {
                    lang = l;
                }
            }
        }
    }
    let lang = lang.unwrap_or_else(|| "en".to_string());
    let engine = pick_engine(&lang, models);

    // stem first, then the decoded mix — first candidate yielding a
    // usable word count wins
    if let Some(s) = stem_p {
        if let Ok(w) = run_engine(engine, s, models, &lang) {
            if w.len() >= 12 {
                let _ = std::fs::remove_file(&tmp);
                return Ok(AsrOut { words: w, lang: Some(lang),
                                   engine });
            }
        }
    }
    match decode16(media, &tmp) {
        Ok(p) => {
            let r = run_engine(engine, &p, models, &lang);
            let _ = std::fs::remove_file(&tmp);
            r.map(|words| AsrOut { words, lang: Some(lang), engine })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            // no decodable audio at all — stem result or nothing
            if let Some(s) = stem_p {
                if let Ok(w) = run_engine(engine, s, models, &lang) {
                    return Ok(AsrOut { words: w, lang: Some(lang),
                                       engine });
                }
            }
            Err(e)
        }
    }
}

fn decode16(media: &Path, tmp: &Path) -> Result<PathBuf> {
    let mono = crate::ff::decode_f32(media, 16000, 1)?;
    crate::ff::write_wav(tmp, &mono, 16000, 1)?;
    Ok(tmp.to_path_buf())
}
