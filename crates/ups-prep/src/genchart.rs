//! Chart generation — port of genchart.py. Lyric lines (LRC preferred)
//! + ASR word timings + RMVPE pitch curve → media-timed .upsong
//! (BPM=15000: 1 beat = 1 ms, no quantization loss).

use std::collections::HashMap;
use ups_core::chart::{Chart, Note};

use crate::rmvpe::Curve;
use crate::wordmap::{map_line_words, norm_word, Span};

const GEN_BPM: f64 = 15000.0;
const MIN_WORD_MS: f64 = 120.0;
const MIN_CONF: f64 = 0.35;
const C2: f32 = 65.4064;

type Asr = [(String, f64)];                     // (norm_word, start_ms)

/// A held note can't outlast this — keeps instrumental melody absorbed
/// into voiced runs from stretching a word across a whole solo.
const MAX_HELD_MS: f64 = 8_000.0;
type Spans = HashMap<usize, Span>;

// ---- melody.rs ports ---------------------------------------------------

/// f0 curve → semitone positions (C2=0), NaN where unvoiced.
fn semitone_curve(c: &Curve, conf_thr: f32) -> Vec<f32> {
    (0..c.f0.len())
        .map(|i| {
            if c.f0[i] <= 0.0 || c.conf[i] < conf_thr {
                f32::NAN
            } else {
                12.0 * (c.f0[i] / C2).log2()
            }
        })
        .collect()
}

/// Dominant pitch per word window → (tone, conf); None = freestyle.
fn word_notes(c: &Curve, spans: &[(f64, f64)],
              conf_thr: f32) -> Vec<Option<(i64, f64)>> {
    let s = semitone_curve(c, conf_thr);
    let tms: Vec<f64> = c.times.iter().map(|t| t * 1000.0).collect();
    spans.iter().map(|&(a, b)| {
        let mut hist: HashMap<i64, usize> = HashMap::new();
        let mut total = 0usize;
        for i in 0..s.len() {
            if tms[i] >= a && tms[i] < b && !s[i].is_nan() {
                *hist.entry(s[i].round() as i64).or_default() += 1;
                total += 1;
            }
        }
        if total < 3 {
            return None;
        }
        let (tone, cnt) = hist.into_iter()
            .max_by_key(|(_, n)| *n).unwrap();
        Some((tone, cnt as f64 / total as f64))
    }).collect()
}

/// Krumhansl-Schmuckler key from the pitch-class histogram.
fn estimate_key(c: &Curve, conf_thr: f32) -> Option<String> {
    let s = semitone_curve(c, conf_thr);
    let vals: Vec<f32> = s.into_iter().filter(|v| !v.is_nan()).collect();
    if vals.len() < 200 {
        return None;
    }
    let mut hist = [0.0f64; 12];
    for v in &vals {
        hist[(v.round() as i64).rem_euclid(12) as usize] += 1.0;
    }
    let sum: f64 = hist.iter().sum();
    for h in &mut hist {
        *h /= sum;
    }
    const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09,
        2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
    const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53,
        2.54, 4.75, 3.98, 2.69, 3.34, 3.17];
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F",
        "F#", "G", "G#", "A", "A#", "B"];
    let corr = |a: &[f64; 12], b: &[f64; 12]| -> f64 {
        let (ma, mb) = (a.iter().sum::<f64>() / 12.0,
                        b.iter().sum::<f64>() / 12.0);
        let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
        for i in 0..12 {
            cov += (a[i] - ma) * (b[i] - mb);
            va += (a[i] - ma).powi(2);
            vb += (b[i] - mb).powi(2);
        }
        cov / (va * vb).sqrt().max(1e-12)
    };
    let mut best = (0.0f64, String::new());
    for r in 0..12 {
        let mut pm = [0.0; 12];
        let mut pn = [0.0; 12];
        for i in 0..12 {
            pm[i] = MAJOR[(i + r) % 12];
            pn[i] = MINOR[(i + r) % 12];
        }
        for (sc, name) in [(corr(&hist, &pm), format!("{} major", NAMES[r])),
                           (corr(&hist, &pn), format!("{} minor", NAMES[r]))] {
            if sc > best.0 {
                best = (sc, name);
            }
        }
    }
    (best.0 > 0.3).then_some(best.1)
}

/// Voiced-phrase runs (start_ms, end_ms) — pitch confidence can't be
/// fooled by instrumental sections the way ASR can.
fn voiced_runs(c: &Curve) -> Vec<(f64, f64)> {
    let mut runs = Vec::new();
    let (mut start, mut last_v) = (None::<f64>, 0.0f64);
    for i in 0..c.times.len() {
        let voiced = c.f0[i] > 0.0 && c.conf[i] >= 0.3;
        if voiced {
            if start.is_none() {
                start = Some(c.times[i]);
            }
            last_v = c.times[i];
        } else if let Some(s) = start {
            if c.times[i] - last_v > 0.25 {
                if last_v - s > 0.5 {
                    runs.push((s * 1000.0, last_v * 1000.0));
                }
                start = None;
            }
        }
    }
    if let Some(s) = start {
        if last_v - s > 0.5 {
            runs.push((s * 1000.0, last_v * 1000.0));
        }
    }
    runs
}

fn run_end(t: f64, runs: &[(f64, f64)]) -> f64 {
    runs.iter().find(|(a, b)| a - 300.0 <= t && t <= *b)
        .map(|(_, b)| *b).unwrap_or(t)
}

fn line_end(onset: f64, nxt: f64, ref_dur: f64,
            runs: &[(f64, f64)]) -> f64 {
    let mut end = (onset + 800.0).max(nxt - 100.0)
        .min(onset + ref_dur + 1500.0);
    let rend = runs.iter()
        .filter(|(a, b)| *a < nxt - 50.0 && *b > onset)
        .map(|(_, b)| *b).fold(None::<f64>, |m, b| {
            Some(m.map_or(b, |m| m.max(b)))
        });
    if let Some(r) = rend {
        // runs include instrumental melody (sax/guitar solos fool the
        // pitch tracker) — cap the extension so a solo can't stretch a
        // held note across the whole bridge
        end = end.max(r.min(nxt - 50.0)
                      .min(onset + ref_dur + 4000.0));
    }
    end
}

// ---- span builders -----------------------------------------------------

/// Lyric line + ASR match → (word, start_ms, end_ms) per word.
fn line_word_spans(line_text: &str, span: &Span,
                   asr: &Asr) -> Vec<(String, f64, f64)> {
    let raw: Vec<&str> = line_text.split_whitespace().collect();
    let keep: Vec<usize> = raw.iter().enumerate()
        .filter(|(_, w)| w.chars().any(|c| c.is_alphanumeric()))
        .map(|(i, _)| i).collect();
    let words: Vec<&str> = keep.iter().map(|&i| raw[i]).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut times: HashMap<usize, f64> = HashMap::new();
    for (op, &rp) in keep.iter().enumerate() {
        if let Some(&a) = span.words.get(&rp) {
            times.insert(op, asr[a].1);
        }
    }
    let known: Vec<usize> = {
        let mut k: Vec<usize> = times.keys().copied().collect();
        k.sort();
        k
    };
    for pos in 0..words.len() {
        if times.contains_key(&pos) {
            continue;
        }
        let prev = known.iter().rev().find(|&&k| k < pos).copied();
        let nxt = known.iter().find(|&&k| k > pos).copied();
        let t = match (prev, nxt) {
            (Some(p), Some(n)) => {
                let frac = (pos - p) as f64 / (n - p) as f64;
                times[&p] + frac * (times[&n] - times[&p])
            }
            (None, Some(n)) => {
                let lo = span.onset_ms;
                (times[&n] - (times[&n] - lo) * (n - pos) as f64
                 / n as f64).max(lo)
            }
            (Some(p), None) => times[&p] + 350.0 * (pos - p) as f64,
            (None, None) => span.onset_ms + pos as f64 * 300.0,
        };
        times.insert(pos, t);
    }
    let lo = span.onset_ms;
    let hi = (span.end_ms - 50.0).max(span.onset_ms + 50.0);
    let mut starts = Vec::with_capacity(words.len());
    for i in 0..words.len() {
        let t = times[&i].clamp(lo, hi);
        starts.push(t.max(starts.last().copied().unwrap_or(0.0)));
    }
    let ends: Vec<f64> = starts[1..].iter().copied()
        .chain(std::iter::once(((starts[starts.len() - 1] + 100.0)
                                .max(span.end_ms))
                               .min(starts[starts.len() - 1]
                                    + MAX_HELD_MS)))
        .collect();
    words.iter().zip(&starts).zip(&ends)
        .map(|((w, s), e)| (w.to_string(), *s, *e))
        .collect()
}

/// No online lyrics → phrase lines from ASR word-pause gaps.
fn asr_lines(asr: &Asr, gap_ms: f64,
             runs: &[(f64, f64)]) -> (Vec<String>, Spans) {
    let (mut texts, mut spans) = (Vec::<String>::new(), Spans::new());
    let (mut cur, mut idxs) =
        (Vec::<(String, f64)>::new(), Vec::<usize>::new());
    for (i, (w, t)) in asr.iter().enumerate() {
        if !idxs.is_empty() && t - asr[*idxs.last().unwrap()].1 > gap_ms {
            texts.push(cur.iter().map(|x| x.0.clone())
                .collect::<Vec<_>>().join(" "));
            let li = texts.len() - 1;
            spans.insert(li, Span {
                onset_ms: cur[0].1,
                end_ms: run_end(cur.last().unwrap().1, runs),
                cov: 1.0,
                words: idxs.iter().copied().enumerate()
                    .map(|(p, a)| (p, a)).collect(),
                ..Default::default()
            });
            cur.clear();
            idxs.clear();
        }
        cur.push((w.clone(), *t));
        idxs.push(i);
    }
    if !cur.is_empty() {
        texts.push(cur.iter().map(|x| x.0.clone())
            .collect::<Vec<_>>().join(" "));
        let li = texts.len() - 1;
        spans.insert(li, Span {
            onset_ms: cur[0].1,
            end_ms: run_end(cur.last().unwrap().1, runs),
            cov: 1.0,
            words: idxs.iter().copied().enumerate()
                .map(|(p, a)| (p, a)).collect(),
            ..Default::default()
        });
    }
    // merge single-word micro-lines forward
    let (mut mt, mut ms) = (Vec::<String>::new(), Spans::new());
    for li in 0..texts.len() {
        let t = &texts[li];
        if !ms.is_empty() && t.split_whitespace().count() < 2 {
            let prev = mt.len() - 1;
            mt[prev] = format!("{} {}", mt[prev], t);
            let ps = ms.get_mut(&prev).unwrap();
            ps.end_ms = spans[&li].end_ms;
            let off = ps.words.len();
            for (p, a) in spans[&li].words.iter() {
                ps.words.insert(off + p, *a);
            }
        } else {
            mt.push(t.clone());
            ms.insert(mt.len() - 1, spans[&li].clone());
        }
    }
    (mt, ms)
}

/// Anchor deltas → media shift, or None when LRC can't be trusted.
fn lrc_shift(deltas: &[f64], synced: &[(i64, String)],
             asr: &Asr) -> Option<f64> {
    if deltas.len() < 2 {
        return None;
    }
    let times: Vec<f64> = synced.iter().map(|(ms, _)| *ms as f64).collect();
    let lws: Vec<Vec<String>> = synced.iter()
        .map(|(_, t)| t.split_whitespace()
            .map(norm_word).filter(|w| !w.is_empty()).collect())
        .collect();
    let score = |shift: f64| -> f64 {
        let onsets: Vec<f64> = times.iter().map(|t| t + shift).collect();
        let hits = asr.iter()
            .filter(|(_, t)| onsets.iter()
                .any(|o| (t - o).abs() <= 700.0))
            .count() as f64;
        let mut tot = 0.0;
        for (li, lw) in lws.iter().enumerate() {
            if lw.is_empty() {
                continue;
            }
            let lo = times[li] + shift - 250.0;
            let hi = if li + 1 < times.len() {
                times[li + 1] + shift
            } else {
                times[li] + shift + 4000.0
            };
            let mut p = 0usize;
            for (w, t) in asr {
                if *t >= lo && *t < hi && p < lw.len() && *w == lw[p] {
                    p += 1;
                }
            }
            tot += p as f64;
        }
        hits + tot
    };
    let mut sd = deltas.to_vec();
    sd.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut clusters: Vec<Vec<f64>> = vec![vec![sd[0]]];
    for &d in &sd[1..] {
        if d - clusters.last().unwrap().last().unwrap() <= 1500.0 {
            clusters.last_mut().unwrap().push(d);
        } else {
            clusters.push(vec![d]);
        }
    }
    let mut cands: Vec<f64> = clusters.iter()
        .map(|c| c[c.len() / 2]).collect();
    let mut s = -5000.0;
    while s < 60000.0 {
        cands.push(s);
        s += 1000.0;
    }
    let best = cands.iter().copied()
        .max_by(|a, b| score(*a).partial_cmp(&score(*b)).unwrap())?;
    let total_lw: usize = lws.iter().map(|l| l.len()).sum();
    if score(best) < (8usize).max((0.08 * total_lw as f64) as usize) as f64 {
        return None;
    }
    let mut f = best - 900.0;
    let mut fine = (f, f64::MIN);
    while f <= best + 900.0 {
        let sc = score(f);
        if sc > fine.1 {
            fine = (f, sc);
        }
        f += 100.0;
    }
    Some(fine.0)
}

/// Un-anchored lines get an estimated span between anchored neighbours.
fn interp_spans(spans: &mut Spans, texts: &[String],
                lrc_times: Option<&[f64]>, asr: &Asr) {
    let anchored: Vec<usize> = {
        let mut v: Vec<usize> = spans.keys().copied().collect();
        v.sort();
        v
    };
    if anchored.is_empty() {
        return;
    }
    for li in 0..texts.len() {
        if spans.contains_key(&li) {
            continue;
        }
        let prev = anchored.iter().rev().find(|&&a| a < li).copied();
        let nxt = anchored.iter().find(|&&a| a > li).copied();
        let onset = match (prev, nxt) {
            (Some(p), Some(n)) => {
                let media_gap =
                    spans[&n].onset_ms - spans[&p].end_ms;
                let frac = match lrc_times {
                    Some(lt) => {
                        let lgap = lt[n] - lt[p];
                        if lgap > 30000.0 && media_gap < 15000.0 {
                            continue; // verse cut from this media
                        }
                        if lgap > 0.0 {
                            (lt[li] - lt[p]) / lgap
                        } else {
                            0.0
                        }
                    }
                    None => (li - p) as f64 / (n - p) as f64,
                };
                let frac = frac.clamp(0.0, 1.0);
                spans[&p].onset_ms + frac
                    * (spans[&n].onset_ms - spans[&p].onset_ms)
            }
            (None, Some(n)) => {
                let gap = lrc_times.map(|lt| lt[n] - lt[li])
                    .unwrap_or((n - li) as f64 * 2500.0);
                (spans[&n].onset_ms - gap.max(400.0)).max(0.0)
            }
            (Some(p), None) => {
                let gap = lrc_times.map(|lt| lt[li] - lt[p])
                    .unwrap_or((li - p) as f64 * 2500.0);
                spans[&p].end_ms + 400.0_f64.max(gap.min(30000.0))
            }
            (None, None) => continue,
        };
        let hi = nxt.map(|n| spans[&n].onset_ms).unwrap_or(onset + 4000.0);
        let win: Vec<usize> = (0..asr.len())
            .filter(|&i| asr[i].1 >= onset - 200.0 && asr[i].1 < hi)
            .collect();
        let lw_n = texts[li].split_whitespace()
            .filter(|w| !norm_word(w).is_empty()).count();
        let words = if lw_n > 0 {
            (0..lw_n.min(win.len()))
                .map(|p| (p, win[p * win.len() / lw_n])).collect()
        } else {
            HashMap::new()
        };
        spans.insert(li, Span {
            onset_ms: onset,
            end_ms: onset + 2500.0,
            cov: 0.0,
            words,
            interp: true,
            ..Default::default()
        });
    }
}

/// Global offset placing LRC line onsets on heard-word onsets.
fn onset_shift(lrc_ms: &[f64], heard_ms: &[f64],
               deltas: &[f64]) -> Option<f64> {
    if heard_ms.is_empty() || lrc_ms.len() < 3 {
        return None;
    }
    let mut ons = heard_ms.to_vec();
    ons.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let score = |s: f64| -> f64 {
        let v: Vec<f64> = lrc_ms.iter()
            .map(|l| {
                let d = ons.iter()
                    .map(|o| (o - (l + s)).abs())
                    .fold(f64::MAX, f64::min);
                (1.0 - d / 600.0).clamp(0.0, 1.0)
            })
            .collect();
        let mut sc = v.iter().sum::<f64>();
        for i in 0..v.len().saturating_sub(1) {
            sc += v[i].min(v[i + 1]) * 3.0;
        }
        sc += deltas.iter()
            .map(|dd| 2.0 * (1.0 - (dd - s).abs() / 1500.0).max(0.0))
            .sum::<f64>();
        let mut covered = vec![false; ons.len()];
        for (i, l) in lrc_ms.iter().enumerate() {
            let lo = l + s - 400.0;
            let hi = if i + 1 < lrc_ms.len() {
                lrc_ms[i + 1] + s
            } else {
                l + s + 5000.0
            };
            for (j, o) in ons.iter().enumerate() {
                if *o >= lo && *o < hi {
                    covered[j] = true;
                }
            }
        }
        sc - covered.iter().filter(|c| !**c).count() as f64 * 2.0
    };
    let mut s = -15000.0;
    let mut best = (s, score(s));
    while s <= 90000.0 {
        let sc = score(s);
        if sc > best.1 {
            best = (s, sc);
        }
        s += 200.0;
    }
    if best.1 < 10.0 {
        return None;
    }
    let mut f = best.0 - 400.0;
    let mut fine = best;
    while f <= best.0 + 400.0 {
        let sc = score(f);
        if sc > fine.1 {
            fine = (f, sc);
        }
        f += 50.0;
    }
    Some(fine.0)
}

/// LRC+shift predicts each line; onset snaps to nearest heard word
/// (or voiced run) — port of _sung_spans.
fn sung_spans(spans: &mut Spans, texts: &[String], times: &[f64],
              shift: f64, heard_ms: &[f64], asr: &Asr,
              runs: &[(f64, f64)]) {
    let mut ons = heard_ms.to_vec();
    ons.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rons: Vec<f64> = {
        let mut v: Vec<f64> = runs.iter().map(|(a, _)| *a).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    };
    let mut prev = -1e9f64;
    let mut placed: Vec<(usize, f64)> = Vec::new();
    let mut dropped: Vec<(usize, f64, Option<Span>)> = Vec::new();
    for (li, lms) in times.iter().enumerate() {
        let pred = lms + shift;
        let cands: Vec<f64> = ons.iter().copied()
            .filter(|o| *o >= prev && (o - pred).abs() <= 3000.0)
            .collect();
        let onset = if let Some(w) = cands.iter().copied()
            .min_by(|a, b| (a - pred).abs()
                .partial_cmp(&(b - pred).abs()).unwrap()) {
            let hold = runs.iter()
                .find(|(a, b)| *a >= prev && *a <= w + 300.0
                      && *b >= w - 50.0)
                .map(|(a, _)| *a);
            hold.unwrap_or(w)
        } else {
            let rc: Vec<f64> = rons.iter().copied()
                .filter(|o| *o >= prev && (o - pred).abs() <= 3000.0)
                .collect();
            if let Some(r) = rc.iter().copied()
                .min_by(|a, b| (a - pred).abs()
                    .partial_cmp(&(b - pred).abs()).unwrap()) {
                r
            } else {
                // no heard onset near the stamp: community LRCs often
                // time repeated sections wrong (chorus 2 stamped at
                // chorus 3's slot etc.) — when the line's own ASR
                // match anchored it far from the stamp, trust the ASR
                let asr_on = spans.get(&li)
                    .filter(|s| s.words.len() >= 3
                                && s.onset_ms >= prev
                                && (s.onset_ms - pred).abs() > 3000.0)
                    .map(|s| s.onset_ms);
                match asr_on {
                    Some(o) => o,
                    None => {
                        let near = ons.iter().any(|o| *o >= prev
                            && (o - pred).abs() <= 4000.0);
                        if !near {
                            dropped.push((li, pred,
                                          spans.remove(&li)));
                            continue;
                        }
                        pred
                    }
                }
            }
        };
        prev = onset + 100.0;
        placed.push((li, onset));
    }
    let mut pl_onsets: Vec<f64> = placed.iter().map(|p| p.1).collect();
    pl_onsets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for (li, pred, sp) in dropped {
        let pos = pl_onsets.partition_point(|&o| o < pred);
        if pos > 0 && pos < pl_onsets.len() {
            placed.push((li, pred));
        } else if let Some(s) = &sp {
            if s.words.len() >= 3 {
                let o = s.onset_ms;
                spans.insert(li, s.clone());
                placed.push((li, o));
            } else {
                continue;
            }
        } else {
            continue;
        }
        pl_onsets = placed.iter().map(|p| p.1).collect();
        pl_onsets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    }
    placed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    for &(li, onset) in &placed {
        let lrc_dur = if li + 1 < times.len() {
            times[li + 1] - times[li]
        } else {
            6000.0
        };
        let nxt = placed.iter().map(|p| p.1)
            .filter(|o| *o > onset)
            .fold(f64::MAX, f64::min);
        let nxt = if nxt == f64::MAX { onset + lrc_dur } else { nxt };
        let end = line_end(onset, nxt, lrc_dur, runs);
        let mut words: HashMap<usize, usize> =
            spans.get(&li).map(|s| s.words.clone()).unwrap_or_default();
        words.retain(|_, a| {
            let t = asr[*a].1;
            t >= onset - 300.0 && t < end
        });
        let used: std::collections::HashSet<usize> =
            words.values().copied().collect();
        let win: Vec<usize> = (0..asr.len())
            .filter(|i| asr[*i].1 >= onset - 300.0 && asr[*i].1 < end
                    && !used.contains(i))
            .collect();
        let lw_n = texts[li].split_whitespace()
            .filter(|w| !norm_word(w).is_empty()).count();
        let free_pos: Vec<usize> =
            (0..lw_n).filter(|p| !words.contains_key(p)).collect();
        for (p, ai) in free_pos.into_iter().zip(win) {
            words.insert(p, ai);
        }
        spans.insert(li, Span {
            onset_ms: onset,
            end_ms: end,
            cov: words.len() as f64 / lw_n.max(1) as f64,
            words,
            ..Default::default()
        });
    }
}

/// Words under MIN_WORD_MS merge forward into the next word.
fn merge_short(spans: &mut Vec<(String, f64, f64)>) {
    let mut i = 0;
    while i < spans.len() {
        let (w, s, e) = spans[i].clone();
        if e - s < MIN_WORD_MS && i + 1 < spans.len() {
            let (nw, _, ne) = spans[i + 1].clone();
            spans[i + 1] = (format!("{w} {nw}"), s, ne);
        }
        i += 1;
    }
    let mut out: Vec<(String, f64, f64)> = Vec::new();
    let mut i = 0;
    while i < spans.len() {
        let (w, s, e) = &spans[i];
        if e - s < MIN_WORD_MS && i + 1 < spans.len() {
            // merged already forward — drop
        } else {
            out.push((w.clone(), *s, *e));
        }
        i += 1;
    }
    *spans = out;
}

/// LRC line times → media phrase onsets, preserving rhythm shape.
fn phrase_map(times: &[f64], onsets: &[f64]) -> Option<Vec<f64>> {
    let (n, m) = (times.len(), onsets.len());
    if n < 3 || m < n {
        return None;
    }
    let mut dp: HashMap<(usize, usize), (f64, Option<usize>)> =
        HashMap::new();
    for j in 0..m {
        dp.insert((0, j), (0.0, None));
    }
    for i in 1..n {
        let g = (times[i] - times[i - 1]).max(200.0);
        for j in i..m {
            let mut best: Option<(f64, usize)> = None;
            for pj in i - 1..j {
                let Some((pc, _)) = dp.get(&(i - 1, pj)) else {
                    continue;
                };
                let pg = (onsets[j] - onsets[pj]).max(200.0);
                let cost = pc + (pg / g).ln().abs();
                if best.is_none_or(|(b, _)| cost < b) {
                    best = Some((cost, pj));
                }
            }
            if let Some((c, pj)) = best {
                dp.insert((i, j), (c, Some(pj)));
            }
        }
    }
    let end = (n - 1..m)
        .min_by(|a, b| dp.get(&(n - 1, *a)).map(|x| x.0)
            .unwrap_or(1e9)
            .partial_cmp(&dp.get(&(n - 1, *b)).map(|x| x.0)
                .unwrap_or(1e9)).unwrap())?;
    let (cost, _) = *dp.get(&(n - 1, end))?;
    if cost / n as f64 > 0.7 {
        return None;
    }
    let mut path = vec![end];
    let mut j = end;
    for i in (1..n).rev() {
        j = dp[&(i, j)].1?;
        path.push(j);
    }
    path.reverse();
    Some(path.iter().map(|&k| onsets[k]).collect())
}

// ---- the generator ------------------------------------------------------

pub struct GenMeta {
    pub lyrics_kind: Option<String>,
    pub lrc_shift_ms: Option<f64>,
    pub lrc_onset_only: bool,
    pub lines: usize,
    pub interp_lines: usize,
    pub asr_lines: usize,
    pub words: usize,
    pub asr_words: usize,
    pub matched: usize,
    pub total: usize,
}

/// Build a media-timed chart. `asr` = (norm_word, start_ms) pairs.
/// `synced`/`plain` from LRCLIB; `curve` from rmvpe::extract_curve.
pub fn generate(artist: &str, title: &str,
                synced: &[(i64, String)], plain: &[String],
                asr: &Asr, curve: Option<&Curve>,
                meta: &mut GenMeta) -> Option<Chart> {
    if asr.len() < 20 {
        return None;
    }
    let runs: Vec<(f64, f64)> = curve.map(voiced_runs).unwrap_or_default();
    let (mut line_texts, mut spans, mut lyrics_kind) =
        (Vec::<String>::new(), Spans::new(), None::<String>);
    let mut cands: Vec<(Vec<String>, Option<Vec<f64>>, &str)> =
        Vec::new();
    if !synced.is_empty() {
        cands.push((synced.iter().map(|(_, t)| t.clone()).collect(),
                    Some(synced.iter().map(|(ms, _)| *ms as f64)
                        .collect()),
                    "lrc"));
    }
    if !plain.is_empty() {
        cands.push((plain.to_vec(), None, "plain"));
    }
    for (texts, times, kind) in cands {
        let sp = map_line_words(asr, &texts);
        let matched: usize =
            sp.values().map(|s| s.words.len()).sum();
        let total: usize = texts.iter()
            .map(|t| t.split_whitespace()
                .filter(|w| !norm_word(w).is_empty()).count())
            .sum::<usize>().max(1);
        meta.matched = matched;
        meta.total = total;
        if matched >= 8.max((0.07 * total as f64) as usize) {
            line_texts = texts;
            spans = sp;
            lyrics_kind = Some(kind.to_string());
            if let Some(times) = &times {
                let heard: Vec<f64> = asr.iter().map(|(_, t)| *t).collect();
                let deltas: Vec<f64> = spans.keys()
                    .map(|li| spans[li].onset_ms - times[*li])
                    .collect();
                let shift = onset_shift(times, &heard, &deltas)
                    .or_else(|| lrc_shift(&deltas, synced, asr));
                if let Some(shift) = shift {
                    sung_spans(&mut spans, &line_texts, times, shift,
                               &heard, asr, &runs);
                    meta.lrc_shift_ms = Some(shift);
                } else {
                    interp_spans(&mut spans, &line_texts,
                                 Some(times), asr);
                }
            }
            break;
        }
    }
    // text-match failed — try onset-structure anchoring on synced
    if lyrics_kind.is_none() && !synced.is_empty() {
        let times: Vec<f64> =
            synced.iter().map(|(ms, _)| *ms as f64).collect();
        let heard: Vec<f64> = if !runs.is_empty() {
            runs.iter().map(|(a, _)| *a).collect()
        } else {
            asr.iter().map(|(_, t)| *t).collect()
        };
        let texts: Vec<String> =
            synced.iter().map(|(_, t)| t.clone()).collect();
        if let Some(mapped) = phrase_map(&times, &heard) {
            for (li, on) in mapped.iter().enumerate() {
                let dur = if li + 1 < times.len() {
                    times[li + 1] - times[li]
                } else {
                    6000.0
                };
                let nxt = mapped.get(li + 1).copied().unwrap_or(on + dur);
                spans.insert(li, Span {
                    onset_ms: *on,
                    end_ms: line_end(*on, nxt, dur, &runs),
                    words: HashMap::new(),
                            ..Default::default()
                });
            }
            if spans.len() >= 3 {
                line_texts = texts;
                lyrics_kind = Some("lrc".into());
                meta.lrc_onset_only = true;
            } else {
                spans.clear();
            }
        } else if let Some(shift) = onset_shift(&times, &heard, &[]) {
            sung_spans(&mut spans, &texts, &times, shift,
                       &heard, asr, &runs);
            if spans.len() >= 3 {
                line_texts = texts;
                lyrics_kind = Some("lrc".into());
                meta.lrc_shift_ms = Some(shift);
                meta.lrc_onset_only = true;
            } else {
                spans.clear();
            }
        }
    }
    // gap-fill: unclaimed words just outside line windows get claimed
    let mut used: std::collections::HashSet<usize> = spans.values()
        .flat_map(|s| s.words.values().copied()).collect();
    if !spans.is_empty() {
        let mut wins: Vec<(f64, f64)> = spans.values()
            .map(|s| (s.onset_ms - 1800.0, s.end_ms + 1800.0))
            .collect();
        wins.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (i, (_, t)) in asr.iter().enumerate() {
            if !used.contains(&i)
                && wins.iter().any(|(a, b)| *t >= *a && *t < *b) {
                used.insert(i);
            }
        }
    }
    let left_idx: Vec<usize> =
        (0..asr.len()).filter(|i| !used.contains(i)).collect();
    // ASR gap-fill: words no lyric line claimed become lines of their
    // own — LRC sources routinely drop ad-libs/bridge lines the singer
    // actually sang, and empty gaps mid-song read as missing lyrics.
    // When a lyric source matched, demand >=4 words per generated line
    // so stray ASR hallucinations during instrumentals stay out.
    let have_lyrics = lyrics_kind.is_some();
    if left_idx.len() >= 3
        && (!have_lyrics || spans.len() < 3 || used.len() < asr.len() / 2
            || left_idx.len() >= 4) {
        let sub: Vec<(String, f64)> =
            left_idx.iter().map(|&i| asr[i].clone()).collect();
        let (t2, s2) = asr_lines(&sub, 1300.0, &runs);
        let mut base = line_texts.len();
        for (li, mut sp) in s2 {
            if have_lyrics && t2[li].split_whitespace().count() < 4 {
                continue;
            }
            line_texts.push(t2[li].clone());
            sp.asr_native = true;
            sp.words = sp.words.iter()
                .map(|(p, a)| (*p, left_idx[*a])).collect();
            spans.insert(base, sp);
            base += 1;
        }
    }
    if spans.len() < 3 {
        return None;
    }

    // word spans per matched line, onset order
    let mut all_words: Vec<(String, f64, f64, usize)> = Vec::new();
    let mut line_breaks: Vec<f64> = Vec::new();
    let mut order: Vec<usize> = spans.keys().copied().collect();
    order.sort_by(|a, b| spans[a].onset_ms
        .partial_cmp(&spans[b].onset_ms).unwrap());
    for li in order {
        let mut ws = line_word_spans(&line_texts[li], &spans[&li], asr);
        merge_short(&mut ws);
        if ws.is_empty() {
            continue;
        }
        for (w, s, e) in &ws {
            all_words.push((w.clone(), *s, *e, li));
        }
        line_breaks.push(ws.last().unwrap().2);
    }
    if all_words.len() < 15 {
        return None;
    }
    all_words.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    // one voice per track — notes can't overlap. Two words sharing a
    // start came from rival lines claiming the same window; keep the
    // better-anchored one (real lyric match > interpolated > asr),
    // and clamp any remaining tail-over-head overlap
    let mut merged: Vec<(String, f64, f64, usize)> = Vec::new();
    let rank = |li: usize, spans: &Spans| {
        let s = &spans[&li];
        (s.asr_native || s.interp, -(s.words.len() as i64))
    };
    for w in all_words.into_iter() {
        let mut keep = true;
        while keep {
            let Some(last) = merged.last() else { break };
            if (w.1 - last.1).abs() >= 60.0 || last.3 == w.3 {
                break;
            }
            if rank(w.3, &spans) < rank(last.3, &spans) {
                merged.pop();
            } else {
                keep = false;
            }
        }
        if keep {
            merged.push(w);
        }
    }
    for i in 0..merged.len().saturating_sub(1) {
        let nx = merged[i + 1].1;
        if merged[i].2 > nx {
            merged[i].2 = nx;
        }
    }
    all_words = merged;

    // pitch per word
    let wn: Vec<Option<(i64, f64)>> = match curve {
        Some(c) => word_notes(
            c,
            &all_words.iter().map(|(_, s, e, _)| (*s, *e))
                .collect::<Vec<_>>(),
            0.5),
        None => vec![None; all_words.len()],
    };

    let mut chart = Chart::new();
    chart.headers = [
        ("ARTIST".to_string(), artist.to_string()),
        ("TITLE".into(), title.to_string()),
        ("BPM".into(), format!("{}", GEN_BPM as i64)),
        ("GAP".into(), "0".into()),
        ("CREATOR".into(), "ultrapistar-genchart".into()),
    ].into_iter().collect();
    let mut p1 = Vec::new();
    for ((w, s, e, _li), p) in all_words.iter().zip(&wn) {
        let (kind, tone) = match p {
            Some((tone, conf)) if *conf >= MIN_CONF =>
                ("normal", *tone),
            _ => ("freestyle", 0),
        };
        p1.push(Note {
            kind: kind.into(),
            start: s.round() as i64,
            length: (e - s).round().max(50.0) as i64,
            pitch: tone,
            text: w.clone(),
            word_break: true,
            alt: String::new(),
        });
    }
    chart.tracks.insert("P1".into(), p1);
    ups_core::romanize::romanize_chart(&mut chart);
    chart.breaks.insert(
        "P1".into(),
        line_breaks[..line_breaks.len().saturating_sub(1)]
            .iter().map(|b| b.round() as i64).collect());
    chart.source = serde_json::json!({
        "type": "generated",
        "extractor": "rmvpe",
        "lrc": lyrics_kind.as_deref() == Some("lrc"),
        "lyrics": lyrics_kind,
    });
    meta.lyrics_kind = lyrics_kind.clone();
    meta.lines = spans.len();
    meta.interp_lines = spans.values().filter(|s| s.interp).count();
    meta.asr_lines = spans.values().filter(|s| s.asr_native).count();
    meta.words = all_words.len();
    meta.asr_words = asr.len();
    if let Some(c) = curve {
        if let Some(k) = estimate_key(c, 0.5) {
            chart.source["key"] = serde_json::json!(k);
        }
    }
    if chart.validate().is_empty() {
        Some(chart)
    } else {
        None
    }
}
