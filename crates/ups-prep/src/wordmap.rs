//! Lyric↔ASR word alignment — port of songmap.py's map_line_words.
//! A minimal Ratcliff-Obershelp matcher (difflib's get_matching_blocks)
//! aligns the full ASR stream against the full lyric stream.

use std::collections::HashMap;
use unicode_categories::UnicodeCategories;
use unicode_normalization::UnicodeNormalization;

/// Lowercase alphanumerics — common form for lyric/ASR matching.
/// Unicode-aware: NFC, letters/digits/apostrophes/combining marks.
pub fn norm_word(w: &str) -> String {
    w.to_lowercase().nfc()
        .filter(|c| {
            c.is_alphanumeric() || *c == '\''
                || c.is_mark_nonspacing() || c.is_mark_spacing_combining()
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct Block {
    a: usize,
    b: usize,
    size: usize,
}

/// Longest contiguous match of a[alo..ahi] inside b[blo..bhi].
fn longest_match(a: &[String], b: &[String],
                 alo: usize, ahi: usize,
                 blo: usize, bhi: usize) -> Block {
    // b-word → positions index
    let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, w) in b.iter().enumerate().take(bhi).skip(blo) {
        b2j.entry(w.as_str()).or_default().push(j);
    }
    let mut best = Block { a: alo, b: blo, size: 0 };
    // newj2len[j] = longest a-ending-at-i / b-ending-at-j run
    let mut prev: HashMap<usize, usize> = HashMap::new();
    for i in alo..ahi {
        let mut cur: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(a[i].as_str()) {
            for &j in js {
                let len = prev.get(&j.wrapping_sub(1))
                    .copied().unwrap_or(0) + 1;
                cur.insert(j, len);
                if len > best.size {
                    best = Block { a: i + 1 - len, b: j + 1 - len, size: len };
                }
            }
        }
        prev = cur;
    }
    best
}

/// difflib get_matching_blocks — recursive on the unmatched gaps.
fn matching_blocks(a: &[String], b: &[String]) -> Vec<Block> {
    let mut out = Vec::new();
    let mut stack = vec![(0usize, a.len(), 0usize, b.len())];
    while let Some((alo, ahi, blo, bhi)) = stack.pop() {
        let blk = longest_match(a, b, alo, ahi, blo, bhi);
        if blk.size == 0 {
            continue;
        }
        out.push(blk);
        if alo < blk.a && blo < blk.b {
            stack.push((alo, blk.a, blo, blk.b));
        }
        if blk.a + blk.size < ahi && blk.b + blk.size < bhi {
            stack.push((blk.a + blk.size, ahi, blk.b + blk.size, bhi));
        }
    }
    out.sort_by_key(|b| b.a);
    out
}


/// A lyric line's span on the ASR stream.
#[derive(Debug, Clone, Default)]
pub struct Span {
    pub onset_ms: f64,
    pub end_ms: f64,
    pub cov: f64,
    /// lyric word position → asr index
    pub words: HashMap<usize, usize>,
    /// interpolated between anchored neighbours
    pub interp: bool,
    /// built from raw ASR phrasing (no lyric record)
    pub asr_native: bool,
}

/// Whole-stream word alignment → {line_idx: Span}.
/// Only lines with ≥1 anchored word appear.
pub fn map_line_words(asr: &[(String, f64)],
                      line_texts: &[String]) -> HashMap<usize, Span> {
    let aw: Vec<String> = asr.iter().map(|(w, _)| w.clone()).collect();
    // bw[i] = normalized lyric word; lw[i] = (line_idx, word_pos)
    let (mut lw, mut bw) = (Vec::new(), Vec::new());
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for (li, text) in line_texts.iter().enumerate() {
        for (p, w) in text.split_whitespace().enumerate() {
            let n = norm_word(w);
            if !n.is_empty() {
                lw.push((li, p));
                bw.push(n);
                *counts.entry(li).or_default() += 1;
            }
        }
    }
    let mut spans: HashMap<usize, Span> = HashMap::new();
    if aw.is_empty() || bw.is_empty() {
        return spans;
    }
    for blk in matching_blocks(&aw, &bw) {
        for i in 0..blk.size {
            let (li, p) = lw[blk.b + i];
            let t = asr[blk.a + i].1;
            let sp = spans.entry(li).or_default();
            sp.words.insert(p, blk.a + i);
            if sp.words.len() == 1 {
                sp.onset_ms = t;
                sp.end_ms = t;
            } else {
                sp.onset_ms = sp.onset_ms.min(t);
                sp.end_ms = sp.end_ms.max(t);
            }
        }
    }
    for (li, sp) in spans.iter_mut() {
        sp.cov = sp.words.len() as f64
            / *counts.get(li).unwrap_or(&1).max(&1) as f64;
    }
    spans
}

/// Norm the asr stream once — [(norm_word, ms)].
pub fn norm_asr(raw: &[(String, f64, f64)]) -> Vec<(String, f64)> {
    raw.iter()
        .filter_map(|(w, s, _)| {
            let n = norm_word(w);
            (!n.is_empty()).then(|| (n, *s))
        })
        .collect()
}
