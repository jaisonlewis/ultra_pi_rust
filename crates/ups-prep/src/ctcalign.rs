//! Lyrics→audio forced alignment on a CTC acoustic model.
//!
//! The ASR path *transcribes* the vocal stem and then fuzzy-matches
//! lyric words to what it heard — on sung audio that recall is ~60%
//! and TDT timestamps are emission times, late and coarse. Here we go
//! the other way: the lyric text is known, so we Viterbi-align its
//! token sequence against the model's per-frame posteriors. Words
//! inherit on/offsets from their tokens, and the path's per-word
//! posterior mass gives a confidence we can gate on.
//!
//! Between lines the trellis carries an optional wildcard (`<*>`: the
//! frame's best token, blank included) so audio with no lyrics (solos,
//! ad-libs, backing vocals) and lyrics with no audio (an LRC for a
//! longer edit) don't drag neighbouring words off position.
//!
//! Two backends, chosen per song language:
//!  * parakeet-tdt_ctc-110m — English FastConformer, BPE tokens over
//!    NeMo fbank, 80 ms frames. Far stronger on sung/processed English
//!    than the multilingual model (which hears "i walo e" in a
//!    reverb-drenched 80s verse).
//!  * omnilingual-300M-CTC — 1600 languages, character tokens on raw
//!    16 kHz audio, 20 ms frames. Covers every script we romanize.

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::rmvpe::ort_env;

pub const SR: usize = 16_000;
/// a word whose path posterior falls below this is "not heard" —
/// callers interpolate or drop it instead of trusting the frames
pub const MIN_WORD_CONF: f32 = 0.25;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Backend {
    /// BPE over fbank, 80 ms frames, blank = last id
    ParakeetCtc,
    /// chars over raw audio, 20 ms frames, blank = 0
    Omnilingual,
}

pub struct Aligner {
    pub backend: Backend,
    pub frame_ms: f64,
    sess: Session,
    /// token string -> id (chars for omnilingual, "▁piece"/"piece" for BPE)
    vocab: HashMap<String, usize>,
    blank: usize,
    /// explicit word-separator token (omnilingual's ' '), None for BPE
    space: Option<usize>,
}

/// Frame log-posteriors: `lp[t * vocab + k]`.
pub struct LogProbs {
    pub frames: usize,
    pub vocab: usize,
    pub lp: Vec<f32>,
}

impl LogProbs {
    #[inline]
    fn at(&self, t: usize, k: usize) -> f32 {
        self.lp[t * self.vocab + k]
    }
}

/// One aligned lyric word.
#[derive(Debug, Clone)]
pub struct AlignedWord {
    /// index into the caller's line list
    pub line: usize,
    /// word position within the line (over alphanumeric tokens)
    pub pos: usize,
    pub text: String,
    pub start_ms: f64,
    pub end_ms: f64,
    /// geometric-mean posterior of the word's token emissions
    pub conf: f32,
}

fn find_dir(models: &Path, prefix: &str) -> Option<PathBuf> {
    std::fs::read_dir(models).ok()?.flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str())
                  .is_some_and(|n| n.starts_with(prefix))
               && p.join("model.int8.onnx").exists()
               && p.join("tokens.txt").exists())
}

fn parakeet_dir(models: &Path) -> Option<PathBuf> {
    find_dir(models, "sherpa-onnx-nemo-parakeet_tdt_ctc")
}

fn omni_dir(models: &Path) -> Option<PathBuf> {
    find_dir(models, "sherpa-onnx-omnilingual-asr-")
}

/// Which backend `lang` (ISO-639-1, "" = unknown) would get, or None
/// when neither model is installed.
pub fn backend_for(models: &Path, lang: &str) -> Option<Backend> {
    let en = lang.is_empty() || lang == "en";
    if en && parakeet_dir(models).is_some() {
        Some(Backend::ParakeetCtc)
    } else if omni_dir(models).is_some() {
        Some(Backend::Omnilingual)
    } else if parakeet_dir(models).is_some() {
        // non-English song, no multilingual model: parakeet still
        // aligns romanized text passably
        Some(Backend::ParakeetCtc)
    } else {
        None
    }
}

pub fn have_model(models: &Path) -> bool {
    parakeet_dir(models).is_some() || omni_dir(models).is_some()
}

impl Aligner {
    pub fn load(models: &Path, backend: Backend) -> Result<Self> {
        let dir = match backend {
            Backend::ParakeetCtc => parakeet_dir(models),
            Backend::Omnilingual => omni_dir(models),
        }.context("aligner model dir")?;
        let sess = Session::builder(ort_env())?
            .commit_from_file(dir.join("model.int8.onnx"))
            .context("load aligner model")?;
        let txt = std::fs::read_to_string(dir.join("tokens.txt"))?;
        let mut vocab = HashMap::new();
        let mut max_id = 0usize;
        for line in txt.lines() {
            // "<tok> <id>" — the token itself may be a bare space
            let Some((tok, n)) = line.rsplit_once(' ') else { continue };
            let Ok(n) = n.parse::<usize>() else { continue };
            max_id = max_id.max(n);
            let tok = if tok.is_empty() { " " } else { tok };
            vocab.insert(tok.to_string(), n);
        }
        let (blank, space, frame_ms) = match backend {
            Backend::ParakeetCtc => (
                vocab.get("<blk>").copied().unwrap_or(max_id), None, 80.0),
            Backend::Omnilingual => (
                0, vocab.get(" ").copied(), 20.0),
        };
        Ok(Aligner { backend, frame_ms, sess, vocab, blank, space })
    }

    /// 16 kHz mono f32 → frame log-posteriors over the whole stem.
    /// Windowed (30 s, 4 s overlap, trimmed edges) so attention stays
    /// in budget on long stems; CTC posteriors are frame-local so the
    /// stitch is lossless.
    pub fn log_probs(&mut self, audio16: &[f32]) -> Result<LogProbs> {
        const WIN_S: f64 = 30.0;
        const OVERLAP_S: f64 = 4.0;
        let stride = match self.backend {
            Backend::ParakeetCtc => 1280,       // 160 hop × 8 subsampling
            Backend::Omnilingual => 320,
        };
        let win = (WIN_S * SR as f64) as usize;
        let ov = (OVERLAP_S * SR as f64) as usize;
        let hop = win - ov;
        let edge_fr = ov / 2 / stride;
        let mut lp: Vec<f32> = Vec::new();
        let mut vocab = 0usize;
        let mut frames = 0usize;
        let mut start = 0usize;
        loop {
            let end = (start + win).min(audio16.len());
            let chunk = &audio16[start..end];
            if chunk.len() < stride * 4 {
                break;
            }
            let (rows, t, v) = self.run_chunk(chunk)?;
            vocab = v;
            let first = if start == 0 { 0 } else { edge_fr.min(t) };
            let last = if end == audio16.len() { t }
                       else { t.saturating_sub(edge_fr) };
            let base = start / stride;
            for fr in first..last {
                let abs = base + fr;
                if abs < frames {
                    continue;
                }
                while frames < abs {
                    let row: Vec<f32> = if lp.is_empty() { vec![0.0; v] }
                        else { lp[lp.len() - v..].to_vec() };
                    lp.extend(row);
                    frames += 1;
                }
                lp.extend_from_slice(&rows[fr * v..(fr + 1) * v]);
                frames += 1;
            }
            if end == audio16.len() {
                break;
            }
            start += hop;
        }
        Ok(LogProbs { frames, vocab, lp })
    }

    /// One window → (log-softmaxed rows, frames, vocab).
    fn run_chunk(&mut self, chunk: &[f32]) -> Result<(Vec<f32>, usize, usize)> {
        let out = match self.backend {
            Backend::Omnilingual => {
                let input = Tensor::from_array(
                    ([1usize, chunk.len()], chunk.to_vec()))?;
                self.sess.run(ort::inputs![input])?
            }
            Backend::ParakeetCtc => {
                let (feat, t) = crate::mel::nemo_fbank(chunk);
                let x = Tensor::from_array(([1usize, 80, t], feat))?;
                let len = Tensor::from_array(([1usize], vec![t as i64]))?;
                self.sess.run(ort::inputs![x, len])?
            }
        };
        let (shape, data) = out[0].try_extract_tensor::<f32>()?;
        let (t, v) = (shape[1] as usize, shape[2] as usize);
        let mut rows = vec![0f32; t * v];
        for fr in 0..t {
            let row = &data[fr * v..(fr + 1) * v];
            let m = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let lse = m + row.iter().map(|x| (x - m).exp()).sum::<f32>().ln();
            for k in 0..v {
                rows[fr * v + k] = row[k] - lse;
            }
        }
        Ok((rows, t, v))
    }

    /// Lyric word → model token ids (empty when nothing maps).
    pub fn tokenize(&self, word: &str) -> Vec<usize> {
        match self.backend {
            Backend::Omnilingual => word.chars()
                .flat_map(align_char)
                .filter_map(|c| self.vocab.get(&c.to_string()).copied())
                .collect(),
            Backend::ParakeetCtc => {
                // greedy longest-match sentencepiece over the cased
                // vocab; try the word as written, then lower-case,
                // keep whichever needs fewer pieces (fewer = the
                // tokenizer's own segmentation more often)
                let clean: String = word.chars()
                    .filter(|c| c.is_alphanumeric() || *c == '\u{27}')
                    .collect();
                if clean.is_empty() {
                    return Vec::new();
                }
                let a = self.bpe_greedy(&clean);
                let b = self.bpe_greedy(&clean.to_lowercase());
                match (a, b) {
                    (Some(a), Some(b)) => if b.len() < a.len() { b } else { a },
                    (Some(a), None) => a,
                    (None, Some(b)) => b,
                    (None, None) => Vec::new(),
                }
            }
        }
    }

    fn bpe_greedy(&self, word: &str) -> Option<Vec<usize>> {
        let s = format!("\u{2581}{word}");
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let mut best: Option<(usize, usize)> = None;   // (len, id)
            for j in (i + 1..=chars.len()).rev() {
                let piece: String = chars[i..j].iter().collect();
                if let Some(&id) = self.vocab.get(&piece) {
                    best = Some((j - i, id));
                    break;
                }
            }
            match best {
                Some((len, id)) => { out.push(id); i += len; }
                None => {
                    // unknown char: skip it (the model has no evidence
                    // for it anyway); a lone "▁" has its own piece
                    if chars[i] == '\u{2581}' {
                        return None;
                    }
                    i += 1;
                }
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// Lines of lyric text → aligned words.
    pub fn align(&self, lp: &LogProbs, lines: &[String]) -> Vec<AlignedWord> {
        let mut words: Vec<WordSpec> = Vec::new();
        for (li, line) in lines.iter().enumerate() {
            let toks: Vec<&str> = line.split_whitespace()
                .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
                .collect();
            for (pos, w) in toks.iter().enumerate() {
                let ids = self.tokenize(w);
                if !ids.is_empty() {
                    words.push(WordSpec { line: li, pos, text: w.to_string(), ids });
                }
            }
        }
        viterbi(lp, &words, self.blank, self.space, self.frame_ms)
    }
}

struct WordSpec {
    line: usize,
    pos: usize,
    text: String,
    ids: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Blank,
    Tok,
    /// optional wildcard between lines — absorbs unlyriced audio
    Star,
}

#[derive(Clone, Copy, Debug)]
struct St {
    kind: Kind,
    tok: usize,
    /// index into words for Tok states (usize::MAX = separator)
    word: usize,
}

fn viterbi(lp: &LogProbs, words: &[WordSpec], blank: usize,
           space: Option<usize>, frame_ms: f64) -> Vec<AlignedWord> {
    // ---- state chain:  * b [t b t b ...] (sp b) ... * b --------------
    let mut states: Vec<St> = Vec::new();
    let push_blank = |st: &mut Vec<St>| {
        st.push(St { kind: Kind::Blank, tok: blank, word: usize::MAX });
    };
    let push_star = |st: &mut Vec<St>| {
        st.push(St { kind: Kind::Star, tok: usize::MAX, word: usize::MAX });
    };
    push_star(&mut states);
    push_blank(&mut states);
    let mut cur_line = usize::MAX;
    for (wi, w) in words.iter().enumerate() {
        if w.line != cur_line && cur_line != usize::MAX {
            push_star(&mut states);
            push_blank(&mut states);
        }
        cur_line = w.line;
        for (ti, &t) in w.ids.iter().enumerate() {
            if ti > 0 {
                push_blank(&mut states);
            }
            states.push(St { kind: Kind::Tok, tok: t, word: wi });
        }
        if let Some(sp) = space {
            push_blank(&mut states);
            states.push(St { kind: Kind::Tok, tok: sp, word: usize::MAX });
        }
        push_blank(&mut states);
    }
    push_star(&mut states);
    push_blank(&mut states);
    let n = states.len();
    let t_n = lp.frames;
    if words.is_empty() || t_n == 0 {
        return Vec::new();
    }

    // ---- star = universal absorber: blank at face value, or the ----
    // best real token discounted so lyric tokens win when they fit
    let star: Vec<f32> = (0..t_n).map(|t| {
        let row = &lp.lp[t * lp.vocab..(t + 1) * lp.vocab];
        let best_nb = row.iter().enumerate().filter(|(k, _)| *k != blank)
            .map(|(_, v)| *v).fold(f32::NEG_INFINITY, f32::max);
        (best_nb - 1.0).max(row[blank])
    }).collect();
    let emit = |t: usize, s: usize| -> f32 {
        match states[s].kind {
            Kind::Blank => lp.at(t, blank),
            Kind::Tok => lp.at(t, states[s].tok),
            Kind::Star => star[t],
        }
    };

    // ---- predecessors: self, s-1, and skips over optional states ----
    // (blanks, stars, separators) — never landing identical tokens
    // adjacent, which CTC would collapse
    let skippable = |s: usize| -> bool {
        matches!(states[s].kind, Kind::Blank | Kind::Star)
            || (states[s].kind == Kind::Tok && Some(states[s].tok) == space)
    };
    let same_tok = |a: usize, b: usize| -> bool {
        states[a].kind == Kind::Tok && states[b].kind == Kind::Tok
            && states[a].tok == states[b].tok
    };
    let preds: Vec<Vec<usize>> = (0..n).map(|s| {
        let mut v = vec![s];
        if s >= 1 { v.push(s - 1); }
        for k in 2..=4 {
            if s < k || !(1..k).all(|j| skippable(s - j)) { break; }
            if !same_tok(s, s - k) { v.push(s - k); }
        }
        v
    }).collect();

    // ---- Viterbi, u8 backpointers (pred index) ------------------------
    let neg = f32::NEG_INFINITY;
    let mut score = vec![neg; n];
    let mut next = vec![neg; n];
    let mut bp: Vec<u8> = vec![0; n * t_n];
    for s in 0..n.min(3) {
        if s == 0 || (1..s).all(skippable) {
            score[s] = emit(0, s);
        }
    }
    for t in 1..t_n {
        for s in 0..n {
            let mut best = neg;
            let mut bi = 0u8;
            for (i, &p) in preds[s].iter().enumerate() {
                if score[p] > best {
                    best = score[p];
                    bi = i as u8;
                }
            }
            next[s] = if best == neg { neg } else { best + emit(t, s) };
            bp[t * n + s] = bi;
        }
        std::mem::swap(&mut score, &mut next);
    }
    let mut s = (n.saturating_sub(3)..n)
        .filter(|s| score[*s] > neg)
        .max_by(|a, b| score[*a].partial_cmp(&score[*b]).unwrap())
        .unwrap_or(n - 1);
    let mut path = vec![0usize; t_n];
    for t in (0..t_n).rev() {
        path[t] = s;
        if t > 0 {
            s = preds[s][bp[t * n + s] as usize];
        }
    }

    // ---- words off the path -------------------------------------------
    let mut first = vec![usize::MAX; words.len()];
    let mut last = vec![0usize; words.len()];
    let mut lp_sum = vec![0f32; words.len()];
    let mut n_tok = vec![0usize; words.len()];
    let mut prev_state = usize::MAX;
    for t in 0..t_n {
        let si = path[t];
        let st = &states[si];
        if st.kind == Kind::Tok && st.word != usize::MAX {
            let w = st.word;
            if first[w] == usize::MAX { first[w] = t; }
            last[w] = t;
            if si != prev_state {              // each token once
                lp_sum[w] += lp.at(t, st.tok);
                n_tok[w] += 1;
            }
        }
        prev_state = si;
    }
    words.iter().enumerate().filter(|(wi, _)| first[*wi] != usize::MAX)
        .map(|(wi, w)| AlignedWord {
            line: w.line, pos: w.pos, text: w.text.clone(),
            start_ms: first[wi] as f64 * frame_ms,
            end_ms: (last[wi] + 1) as f64 * frame_ms,
            conf: if n_tok[wi] > 0 {
                (lp_sum[wi] / n_tok[wi] as f32).exp()
            } else { 0.0 },
        })
        .collect()
}

/// Map a lyric character onto the char model's alphabet: lower-case,
/// NFD, keep letters/digits/apostrophe and combining marks (Indic
/// vowel signs are their own tokens), drop punctuation.
fn align_char(c: char) -> Vec<char> {
    use unicode_categories::UnicodeCategories;
    use unicode_normalization::UnicodeNormalization;
    if c.is_alphanumeric() || c == '\u{27}' {
        let s: String = c.to_lowercase().collect();
        s.nfd().filter(|ch| {
            ch.is_alphanumeric() || *ch == '\u{27}' || ch.is_mark_nonspacing()
        }).collect()
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(backend: Backend, wav: &Path, lines: &[&str]) -> Vec<AlignedWord> {
        let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
        let mut al = Aligner::load(&models, backend).unwrap();
        let a16 = crate::ff::decode_f32(wav, SR as u32, 1).unwrap();
        let lp = al.log_probs(&a16).unwrap();
        let w = al.align(&lp, &lines.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        for x in &w {
            println!("{:>6.0}-{:<6.0} {:.2} {}", x.start_ms, x.end_ms, x.conf, x.text);
        }
        w
    }

    #[test]
    fn omni_test_wav() {
        let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
        let Some(dir) = omni_dir(&models) else { return };
        let w = run(Backend::Omnilingual, &dir.join("test_wavs/en.wav"), &[
            "Ask not what your country can do for you",
            "ask what you can do for your country",
        ]);
        assert_eq!(w.len(), 17);
        assert!(w.iter().all(|x| x.conf > 0.3));
        assert!(w.windows(2).all(|p| p[1].start_ms >= p[0].end_ms - 1.0));
    }

    #[test]
    fn parakeet_test_wav() {
        let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
        let Some(dir) = parakeet_dir(&models) else { return };
        let w = run(Backend::ParakeetCtc, &dir.join("test_wavs/0.wav"), &[
            "Well I don\u{27}t wish to see it any more observed Phoebe turning away her eyes",
            "It is certainly very like the old portrait",
        ]);
        assert_eq!(w.len(), 23, "aligned {}", w.len());
        let mean = w.iter().map(|x| x.conf).sum::<f32>() / w.len() as f32;
        println!("mean conf {mean:.2}");
        assert!(mean > 0.4, "mean conf {mean}");
        assert!(w.windows(2).all(|p| p[1].start_ms >= p[0].end_ms - 1.0));
    }
}
