//! Chart→media alignment via chroma cross-correlation — port of
//! align.py. Global FFT correlate finds where chart beat-0 sits in the
//! media; per-phrase local matches reveal arrangement cuts; in-phrase
//! tempo probes give the media's tempo ratio.

use anyhow::Result;
use rustfft::num_complex::Complex;
use std::path::Path;
use ups_core::chart::{Chart, Note};

use crate::ff;

const RATE: u32 = 22050;
const HOP: usize = 512;              // ~23 ms per chroma frame
const WIN: usize = 4096;             // ~186 ms analysis window
const LOCAL_SEARCH_S: f64 = 45.0;
const MIN_SCORE: f32 = 0.22;
const PROBE_RATIOS: [f64; 5] = [0.92, 0.96, 1.0, 1.04, 1.08];

/// (12, frames) log-magnitude pitch-class energy, row-normalized.
fn chromagram(audio: &[f32]) -> Vec<Vec<f32>> {
    let n = if audio.len() > WIN {
        1 + (audio.len() - WIN) / HOP
    } else {
        0
    };
    if n == 0 {
        return vec![vec![]; 12];
    }
    // hann window
    let win: Vec<f32> = (0..WIN)
        .map(|i| {
            0.5 - 0.5
                * (2.0 * std::f32::consts::PI * i as f32
                    / (WIN - 1) as f32)
                .cos()
        })
        .collect();
    let fft = rustfft::FftPlanner::new().plan_fft_forward(WIN);
    // pitch-class bin per rfft bin in 60..2000 Hz
    let bins: Vec<(usize, usize)> = (0..=WIN / 2)
        .enumerate()
        .filter_map(|(k, _)| {
            let f = k as f32 * RATE as f32 / WIN as f32;
            if f > 60.0 && f < 2000.0 {
                let midi = (69.0
                    + 12.0 * (f / 440.0).log2())
                    .round() as i64;
                Some((k, midi.rem_euclid(12) as usize))
            } else {
                None
            }
        })
        .collect();
    let mut c = vec![vec![0.0f32; n]; 12];
    let mut buf = vec![Complex::new(0.0f32, 0.0); WIN];
    let mut scratch = vec![Complex::new(0.0f32, 0.0); WIN];
    for fr in 0..n {
        for (i, w) in win.iter().enumerate() {
            buf[i] = Complex::new(audio[fr * HOP + i] * w, 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);
        let mut row = [0.0f32; 12];
        for &(k, pc) in &bins {
            row[pc] += (buf[k].norm() * 100.0).ln_1p();
        }
        let mx = row.iter().fold(0.0f32, |m, v| m.max(*v)) + 1e-6;
        for pc in 0..12 {
            c[pc][fr] = row[pc] / mx;
        }
    }
    c
}

/// Expected chroma rows for a note list at frame resolution →
/// ((12, n_fr), first_beat).
fn template(notes: &[&Note], beat_ms: f64) -> (Vec<Vec<f32>>, i64) {
    let s = notes.iter().map(|n| n.start).min().unwrap_or(0);
    let e = notes.iter().map(|n| n.start + n.length).max().unwrap_or(0);
    let fpb = beat_ms / (HOP as f64 / RATE as f64 * 1000.0);
    let n_fr = ((e - s) as f64 * fpb).ceil().max(4.0) as usize;
    let mut t = vec![vec![0.0f32; n_fr]; 12];
    for note in notes {
        if note.kind == "freestyle" {
            continue;
        }
        let a = ((note.start - s) as f64 * fpb).round() as usize;
        let b = ((note.start + note.length - s) as f64 * fpb)
            .round() as usize;
        let pc = note.pitch.rem_euclid(12) as usize;
        for fr in a..b.min(n_fr) {
            t[pc][fr] = 1.0;
        }
    }
    (t, s)
}

/// Normalized cross-correlation of a (12,w) template over (12,n)
/// chroma; result length n-w+1.
fn corr(template: &[Vec<f32>], chroma: &[Vec<f32>]) -> Vec<f32> {
    let w = template[0].len();
    let n = chroma[0].len();
    if w == 0 || n < w {
        return Vec::new();
    }
    let size = (n + w - 1).next_power_of_two();
    let fft = rustfft::FftPlanner::new().plan_fft_forward(size);
    let ifft = rustfft::FftPlanner::new().plan_fft_inverse(size);
    let mut conv = vec![0.0f32; size];
    let mut a = vec![Complex::new(0.0f32, 0.0); size];
    let mut b = vec![Complex::new(0.0f32, 0.0); size];
    let mut scr = vec![Complex::new(0.0f32, 0.0); size];
    for pc in 0..12 {
        for (i, &v) in chroma[pc].iter().enumerate() {
            a[i] = Complex::new(v, 0.0);
        }
        for i in n..size {
            a[i] = Complex::new(0.0, 0.0);
        }
        for (i, &v) in template[pc].iter().rev().enumerate() {
            b[i] = Complex::new(v, 0.0);
        }
        for i in w..size {
            b[i] = Complex::new(0.0, 0.0);
        }
        fft.process_with_scratch(&mut a, &mut scr);
        fft.process_with_scratch(&mut b, &mut scr);
        for i in 0..size {
            a[i] *= b[i];
        }
        ifft.process_with_scratch(&mut a, &mut scr);
        for (i, c) in a.iter().enumerate() {
            conv[i] += c.re / size as f32;
        }
    }
    let norm = template
        .iter()
        .flat_map(|r| r.iter())
        .filter(|&&v| v > 0.0)
        .count()
        .max(1) as f32;
    conv[w - 1..n].iter().map(|v| v / norm).collect()
}

/// Best template position near pred_fr → (shift_frames, score).
fn match_at(template: &[Vec<f32>], chroma: &[Vec<f32>],
            pred_fr: f64, radius_fr: usize) -> (f64, f32) {
    let w = template[0].len();
    let n = chroma[0].len();
    let lo = (pred_fr as i64 - radius_fr as i64).max(0) as usize;
    let hi = ((pred_fr as i64 + radius_fr as i64) as usize)
        .min(n.saturating_sub(w));
    if hi <= lo {
        return (0.0, 0.0);
    }
    let sub: Vec<Vec<f32>> = chroma.iter()
        .map(|r| r[lo..(hi + w).min(n)].to_vec())
        .collect();
    let scores = corr(template, &sub);
    if scores.is_empty() {
        return (0.0, 0.0);
    }
    let best = scores.iter().fold(0.0f32, |m, v| m.max(*v));
    if best < MIN_SCORE {
        return (0.0, 0.0);
    }
    // among near-tied peaks prefer the one nearest the prediction
    let mut peaks: Vec<usize> = (1..scores.len().saturating_sub(1))
        .filter(|&i| {
            scores[i] > scores[i - 1] && scores[i] >= scores[i + 1]
        })
        .collect();
    peaks.retain(|&i| scores[i] >= best - 0.03);
    if peaks.is_empty() {
        let (ai, _) = scores.iter().enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        peaks.push(ai);
    }
    let k = peaks.iter()
        .min_by(|a, b| {
            ((*a + lo) as f64 - pred_fr).abs()
                .partial_cmp(&((*b + lo) as f64 - pred_fr).abs())
                .unwrap()
        })
        .copied()
        .unwrap();
    (k as f64 + lo as f64 - pred_fr, scores[k])
}

pub struct AlignResult {
    pub offset_ms: i64,
    pub tempo: f64,
    /// (beat, offset_ms) sustained-shift marks
    pub warp: Vec<(i64, i64)>,
    pub phrases: usize,
    pub matched: usize,
    pub peak: f32,
}

/// Align `chart` to `media` — None when alignment can't be trusted.
pub fn align_chart(chart: &Chart, media: &Path, track: Option<&str>)
                   -> Result<Option<AlignResult>> {
    let track = match track {
        Some(t) => t.to_string(),
        None => chart.tracks.iter()
            .max_by_key(|(_, v)| v.len())
            .map(|(k, _)| k.clone())
            .unwrap_or_default(),
    };
    let Some(notes) = chart.tracks.get(&track) else {
        return Ok(None);
    };
    let audio = ff::decode_f32(media, RATE, 1)?;
    if audio.len() < RATE as usize * 5 {
        return Ok(None);
    }
    let chroma = chromagram(&audio);
    let hop_ms = HOP as f64 / RATE as f64 * 1000.0;
    let beat_ms = if chart.bpm() > 0.0 {
        15000.0 / chart.bpm()
    } else {
        0.0
    };
    if beat_ms <= 0.0 {
        return Ok(None);
    }
    let lines: Vec<Vec<Note>> = chart.lines(&track);
    let lines: Vec<Vec<&Note>> = lines.iter()
        .map(|ln| ln.iter().collect())
        .filter(|ln: &Vec<&Note>| !ln.is_empty())
        .collect();
    if lines.is_empty() {
        return Ok(None);
    }

    // --- stage 1: candidate global offsets at nominal tempo ---
    let all: Vec<&Note> = notes.iter().collect();
    let (tpl, s0) = template(&all, beat_ms);
    if tpl[0].len() as f32 >= chroma[0].len() as f32 * 0.98 {
        return Ok(None); // media shorter than the chart
    }
    let c = corr(&tpl, &chroma);
    let peak = c.iter().fold(0.0f32, |m, v| m.max(*v));
    // strong local maxima, ≥4 s apart, best first
    let mut pk: Vec<usize> = (1..c.len().saturating_sub(1))
        .filter(|&i| c[i] >= c[i - 1] && c[i] > c[i + 1])
        .filter(|&i| c[i] >= peak * 0.55)
        .collect();
    pk.sort_by(|a, b| c[*b].partial_cmp(&c[*a]).unwrap());
    let mut cand_k: Vec<usize> = Vec::new();
    for i in pk {
        if cand_k.iter()
            .all(|&c0| (i as i64 - c0 as i64).abs() as f64 * hop_ms > 4000.0) {
            cand_k.push(i);
        }
        if cand_k.len() >= 6 {
            break;
        }
    }
    if cand_k.is_empty() {
        let (ai, _) = c.iter().enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        cand_k.push(ai);
    }
    let cand_offs: Vec<f64> = cand_k.iter()
        .map(|&k| k as f64 * hop_ms - chart.gap_ms() - s0 as f64 * beat_ms)
        .collect();

    // --- stage 1.5: pick the base whose early phrases land sanely ---
    let tight = (12_000.0 / hop_ms) as usize;
    let probe_lines = &lines[..lines.len().min(12)];
    let consistency = |off: f64| -> (usize, usize) {
        let (mut ok, mut tot) = (0usize, 0usize);
        for ln in probe_lines {
            let s = ln.iter().map(|n| n.start).min().unwrap_or(0);
            let pred = chart.gap_ms() + s as f64 * beat_ms + off;
            if pred < 3000.0 {
                continue;
            }
            tot += 1;
            let (t, _s) = template(ln, beat_ms);
            let (shift_fr, sc) =
                match_at(&t, &chroma, pred / hop_ms, tight);
            if sc >= 0.35 && (shift_fr * hop_ms).abs() <= 8000.0 {
                ok += 1;
            }
        }
        (ok, tot)
    };
    let scored: Vec<(usize, usize)> =
        cand_offs.iter().map(|&o| consistency(o)).collect();
    let best_i = scored.iter().enumerate()
        .max_by_key(|(_, s)| *s).map(|(i, _)| i).unwrap();
    let base_off = cand_offs[best_i];
    if scored[best_i].0 < 2 {
        return Ok(None);
    }

    // --- stage 2: per-phrase local match + in-phrase tempo probe ---
    let radius = (LOCAL_SEARCH_S * 1000.0 / hop_ms) as usize;
    let mut matches: Vec<(i64, f64, f32)> = Vec::new();
    let mut phrase_ratios: Vec<f64> = Vec::new();
    for ln in &lines {
        let s = ln.iter().map(|n| n.start).min().unwrap_or(0);
        let pred_ms = chart.gap_ms() + s as f64 * beat_ms + base_off;
        let mut best: Option<(f64, f32, f64)> = None;
        for r in PROBE_RATIOS {
            let (t, _s) = template(ln, beat_ms * r);
            let (shift_fr, sc) =
                match_at(&t, &chroma, pred_ms / hop_ms, radius);
            if sc > 0.0
                && best.as_ref().is_none_or(|(_, bsc, _)| sc > *bsc) {
                best = Some((pred_ms + shift_fr * hop_ms, sc, r));
            }
        }
        if let Some((obs, sc, r)) = best {
            matches.push((s, obs, sc));
            phrase_ratios.push(r);
        }
    }
    if matches.len() < 3 {
        return Ok(None);
    }

    // --- pace: median in-phrase tempo ratio ---
    let s_arr: Vec<f64> =
        matches.iter().map(|m| m.0 as f64).collect();
    let o_arr: Vec<f64> =
        matches.iter().map(|m| m.1).collect();
    let mut order: Vec<usize> = (0..matches.len()).collect();
    order.sort_by(|a, b| s_arr[*a].partial_cmp(&s_arr[*b]).unwrap());
    let mut pr = phrase_ratios.clone();
    pr.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut ratio = pr[pr.len() / 2];
    if !(0.5..2.0).contains(&ratio) {
        ratio = 1.0;
    }
    let m_coef = ratio * beat_ms;

    // per-phrase offset under the fitted pace; sustained runs only
    let offs: Vec<f64> = (0..matches.len())
        .map(|i| o_arr[i] - (chart.gap_ms() + s_arr[i] * m_coef))
        .collect();
    let seq: Vec<(f64, f64)> =
        order.iter().map(|&i| (s_arr[i], offs[i])).collect();
    let mut early: Vec<f64> =
        order[..order.len().min(3)].iter().map(|&i| offs[i]).collect();
    early.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let base_off = early[early.len() / 2];

    let mut warp: Vec<(i64, i64)> = Vec::new();
    let mut cur = base_off;
    let (mut i, n) = (0usize, seq.len());
    while i < n {
        let off = seq[i].1;
        if (off - cur).abs() <= 1500.0 {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && (seq[j].1 - off).abs() <= 1500.0 {
            j += 1;
        }
        if j - i >= 4 || (j == n && j - i >= 2) {
            warp.push((seq[i].0 as i64, off as i64));
            cur = off;
        }
        i = j;
    }
    Ok(Some(AlignResult {
        offset_ms: base_off as i64,
        tempo: (ratio * 1000.0).round() / 1000.0,
        warp,
        phrases: lines.len(),
        matched: matches.len(),
        peak,
    }))
}

/// Bake an alignment into the chart as a native media-timed .upsong:
/// every note becomes absolute media-ms (BPM=15000) so the player
/// needs no warp machinery — identical to a generated chart.
pub fn bake(chart: &Chart, res: &AlignResult) -> Chart {
    let beat_ms = if chart.bpm() > 0.0 {
        15000.0 / chart.bpm()
    } else {
        1.0
    } * res.tempo;
    let warp_at = |beat: i64| -> f64 {
        let mut off = res.offset_ms as f64;
        for &(wb, wo) in &res.warp {
            if beat >= wb {
                off = wo as f64;
            }
        }
        off
    };
    let mut out = chart.clone();
    for notes in out.tracks.values_mut() {
        for n in notes.iter_mut() {
            let start_ms =
                chart.gap_ms() + n.start as f64 * beat_ms + warp_at(n.start);
            let len_ms = n.length as f64 * beat_ms;
            n.start = start_ms.round() as i64;
            n.length = len_ms.round().max(1.0) as i64;
        }
    }
    for brk in out.breaks.values_mut() {
        for b in brk.iter_mut() {
            *b = (chart.gap_ms() + *b as f64 * beat_ms
                  + warp_at(*b)).round() as i64;
        }
    }
    out.headers.insert("BPM".into(), "15000".into());
    out.headers.insert("GAP".into(), "0".into());
    out.source = serde_json::json!({
        "repo": "aligned",
        "offset_ms": res.offset_ms,
        "tempo": res.tempo,
        "matched": res.matched,
        "phrases": res.phrases,
        "peak": res.peak,
    });
    out
}
