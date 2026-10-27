//! Live singing session — the beat clock and tick loop from
//! `ultrapistar/sing.py` (GPL-3.0), including the pitch-contour
//! autosync (the optional Whistle speech-model fallback is Python-only
//! — when the pitch solve can't lock, we settle instead).
//!
//! Flow: `Session::tick(position_ms, tones)` whenever the media clock
//! advances — it walks every chart beat between ticks, feeds each
//! track's latest detected pitch into its `Scorer`, and returns the
//! score state for display.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::chart::Chart;
use crate::scoring::{LineEvent, Scorer, TickEvent};

/// automatic solve waits for this much continuous sung audio
pub const AUTOSYNC_AFTER_MS: f64 = 8000.0;
/// community GAPs target media with ~90s lead-ins — search wide
pub const AUTOSYNC_WINDOW_MS: i64 = 150000;
pub const AUTOSYNC_MIN_SAMPLES: usize = 60;
/// rolling re-solve cadence/window (arrangement gaps, tempo wander)
const ROLL_CHECK_SAMPLES: usize = 350;
const ROLL_WINDOW: usize = 400;
const ROLL_WINDOW_MS: i64 = 45000;
/// host-triggered resync collects this much fresh audio before solving
pub const RESYNC_COLLECT_MS: f64 = 10000.0;

pub struct Session {
    pub chart: Chart,
    pub difficulty: i64,
    /// ms per chart beat (upstream: 15000/BPM — "beats" are quarter of
    /// the notated BPM step in UltraStar units)
    pub beat_ms: f64,
    /// media ms of chart beat 0 (GAP header + any sync nudge)
    pub base_ms: f64,
    /// per-track scorers ("P1", "P2", ...) — first key is the lead
    pub scorers: BTreeMap<String, Scorer>,
    pub last_beat: i64,
    /// recent sung tones per track (media_ms, tone_abs) — feeds autosync
    pub tone_log: BTreeMap<String, Vec<(f64, i64)>>,
    pub autosync_done: bool,
    pitch_attempted: bool,
    /// media-ms deadline: manual resync forces a solve once crossed
    resync_until: Option<f64>,
    roll_next: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlayerTick {
    pub score: i64,
    pub golden: i64,
    pub line: i64,
    pub streak: i64,
}

pub struct TickResult {
    pub beat: i64,
    pub ms: f64,
    /// per-track scoreboard: track -> {score, golden, line, streak}
    pub players: BTreeMap<String, PlayerTick>,
    /// latest hit event on the lead track (drives the hit flash)
    pub last_hit: Option<TickEvent>,
    /// finished-line events on the lead track (rating pops)
    pub line_events: Vec<LineEvent>,
    /// expected chart pitch per track at this beat (lane highlight)
    pub targets: BTreeMap<String, i64>,
}

impl Session {
    /// `tracks` in performance order — the first is the lead whose hits
    /// and line ratings drive the shared overlays.
    pub fn new(chart: Chart, tracks: &[String], difficulty: i64,
               line_bonus: bool) -> Self {
        let beat_ms = if chart.bpm() > 0.0 { 15000.0 / chart.bpm() } else { 0.0 };
        let scorers = tracks
            .iter()
            .map(|t| (t.clone(), Scorer::new(&chart, t, difficulty, line_bonus)))
            .collect();
        Session {
            base_ms: chart.gap_ms(),
            chart,
            difficulty,
            beat_ms,
            scorers,
            last_beat: -1,
            tone_log: BTreeMap::new(),
            autosync_done: false,
            pitch_attempted: false,
            resync_until: None,
            roll_next: None,
        }
    }

    /// Single-track convenience (ups-play / simple sessions).
    pub fn solo(chart: Chart, track: &str, difficulty: i64,
                line_bonus: bool) -> Self {
        Self::new(chart, &[track.to_string()], difficulty, line_bonus)
    }

    /// Live chart->media sync nudge.
    pub fn set_sync_offset(&mut self, offset_ms: f64) {
        self.base_ms = self.chart.gap_ms() + offset_ms;
    }

    pub fn beat_at(&self, position_ms: f64) -> i64 {
        if self.beat_ms <= 0.0 {
            return -1;
        }
        ((position_ms - self.base_ms) / self.beat_ms) as i64
    }

    /// Advance scoring to `position_ms` of media time, feeding each
    /// track's latest tone (0..45 or None) into its scorer.
    pub fn tick(&mut self, position_ms: f64,
                tones: &BTreeMap<String, Option<i64>>) -> TickResult {
        let beat = self.beat_at(position_ms).max(self.last_beat);
        let lead = self.scorers.keys().next().cloned();
        for (track, tone) in tones {
            if let Some(t) = tone.filter(|&t| t >= 0) {
                let log = self.tone_log.entry(track.clone()).or_default();
                log.push((position_ms, t));
                if log.len() > 4000 {
                    log.drain(..1000);
                }
            }
        }
        let mut last_hit = None;
        let mut line_events = Vec::new();
        while self.last_beat < beat {
            self.last_beat += 1;
            for (track, scorer) in self.scorers.iter_mut() {
                let tone = tones.get(track).copied().flatten();
                if let Some(ev) = scorer.tick(self.last_beat, tone) {
                    if Some(track) == lead.as_ref() {
                        if let Some(le) = &ev.line {
                            line_events.push(le.clone());
                        }
                        last_hit = Some(ev);
                    }
                }
            }
        }
        // expected chart pitch per track at this beat
        let mut targets = BTreeMap::new();
        for (track, scorer) in &self.scorers {
            for note in scorer.lines.iter().flatten() {
                if note.start <= beat && beat < note.start + note.length
                    && note.kind != "freestyle"
                {
                    targets.insert(track.clone(), note.pitch);
                    break;
                }
            }
        }
        TickResult {
            beat,
            ms: position_ms,
            players: self
                .scorers
                .iter()
                .map(|(t, s)| {
                    (t.clone(), PlayerTick {
                        score: s.state.total(),
                        golden: s.state.score_golden_int(),
                        line: s.state.score_line_int(),
                        streak: s.state.streak,
                    })
                })
                .collect(),
            last_hit,
            line_events,
            targets,
        }
    }

    /// Host-triggered resync — drops prior sync state and collects
    /// RESYNC_COLLECT_MS of fresh room audio before re-solving.
    pub fn request_resync(&mut self, position_ms: f64) {
        self.autosync_done = false;
        self.pitch_attempted = false;
        self.roll_next = None;
        self.resync_until = Some(position_ms + RESYNC_COLLECT_MS);
        for log in self.tone_log.values_mut() {
            log.clear();
        }
    }

    /// maybe_autosync port — solve chart->media offset from sung tones.
    /// Some({delta_ms, offset_ms, score, method}) when an offset change
    /// is justified, {delta_ms:0} when settled, None while collecting.
    pub fn maybe_autosync(&mut self, position_ms: f64) -> Option<Value> {
        if self.beat_ms <= 0.0 {
            return None;
        }
        if self.autosync_done {
            return self.roll_check();
        }
        if self.pitch_attempted {
            return None;   // no whistle fallback — pitch gets one shot
        }
        let (track, samples) = self
            .tone_log
            .iter()
            .max_by_key(|(_, l)| l.len())
            .map(|(t, l)| (t.clone(), l.clone()))
            .unwrap_or_default();
        let manual = self
            .resync_until
            .map(|u| position_ms >= u)
            .unwrap_or(false);
        if manual {
            self.resync_until = None;
        }
        let ready = !track.is_empty()
            && samples.len() >= AUTOSYNC_MIN_SAMPLES
            && samples.last().map(|s| s.0 - samples[0].0).unwrap_or(0.0)
                >= AUTOSYNC_AFTER_MS;
        if !(ready || manual) {
            return None;
        }
        self.pitch_attempted = true;
        if !track.is_empty() && samples.len() >= 15 {
            let (delta, score, base_score, cov) =
                self.solve_offset(&track, &samples, AUTOSYNC_WINDOW_MS);
            // far shifts need stronger proof than small drift fixes —
            // a confident-but-wrong lock ratchets the offset away
            let need = if delta.abs() > 20000 { 0.6 } else { 0.5 };
            if delta.abs() > 40
                && score >= need
                && cov >= 0.4
                && score >= base_score + 0.1
            {
                self.autosync_done = true;
                self.roll_next =
                    Some(samples.len() + ROLL_CHECK_SAMPLES);
                let new_off =
                    self.base_ms - self.chart.gap_ms() + delta as f64;
                return Some(json!({
                    "delta_ms": delta, "score": (score * 1000.0).round() / 1000.0,
                    "method": "pitch", "offset_ms": new_off as i64,
                }));
            }
            if base_score >= 0.5 {
                self.autosync_done = true;
                return Some(json!({
                    "delta_ms": 0,
                    "score": (base_score * 1000.0).round() / 1000.0,
                    "method": "pitch",
                }));
            }
        }
        // couldn't lock — Python would try the speech model; settle
        self.autosync_done = true;
        Some(json!({"delta_ms": 0, "method": "pitch"}))
    }

    /// Rolling re-solve on the recent sung window — catches karaoke
    /// videos whose arrangement steps the true gap mid-song, and tempo
    /// mismatches that wander it. Same confidence gate as the first
    /// solve, plus the current offset must score materially worse.
    fn roll_check(&mut self) -> Option<Value> {
        let (track, log) = self
            .tone_log
            .iter()
            .max_by_key(|(_, l)| l.len())?;
        let next = self.roll_next?;
        if log.len() < next {
            return None;
        }
        self.roll_next = Some(log.len() + ROLL_CHECK_SAMPLES);
        let samples: Vec<(f64, i64)> =
            log.iter().rev().take(ROLL_WINDOW).cloned().collect();
        let (delta, score, base_score, cov) =
            self.solve_offset(track, &samples, ROLL_WINDOW_MS);
        if delta.abs() > 100
            && score >= 0.5
            && cov >= 0.4
            && score >= base_score + 0.1
        {
            let new_off = self.base_ms - self.chart.gap_ms() + delta as f64;
            return Some(json!({
                "delta_ms": delta, "offset_ms": new_off as i64,
                "score": (score * 1000.0).round() / 1000.0,
                "method": "roll",
            }));
        }
        None
    }

    /// _solve_offset port — score sung (media_ms, tone) samples against
    /// the expected melody at every candidate base offset inside the
    /// window. Three-tier search: coarse pass on a subsample, near-tie
    /// preference for the smallest shift (melodic aliasing), then
    /// refine + fine around the winner.
    fn solve_offset(&self, track: &str, samples: &[(f64, i64)],
                    window_ms: i64) -> (i64, f64, f64, f64) {
        let Some(scorer) = self.scorers.get(track) else {
            return (0, 0.0, 0.0, 0.0);
        };
        let notes: Vec<&crate::chart::Note> =
            scorer.lines.iter().flatten().collect();
        if notes.is_empty() {
            return (0, 0.0, 0.0, 0.0);
        }
        let lo = notes.iter().map(|n| n.start).min().unwrap();
        let hi = notes.iter().map(|n| n.start + n.length).max().unwrap();
        let mut exp = vec![-1i64; (hi - lo + 1) as usize];
        for n in notes {
            if n.kind == "freestyle" {
                continue;
            }
            for b in n.start..n.start + n.length {
                exp[(b - lo) as usize] = n.pitch;
            }
        }
        let n_pts = samples.len().max(1) as f64;
        let score = |base: f64, pts: &[(f64, i64)]| -> (f64, f64) {
            let (mut hits, mut cov) = (0usize, 0usize);
            for (ms, tone) in pts {
                let b = ((ms - base) / self.beat_ms) as i64 - lo;
                if b >= 0 && (b as usize) < exp.len() && exp[b as usize] >= 0
                {
                    cov += 1;
                    let d = (exp[b as usize] - tone).abs() % 12;
                    if d.min(12 - d) <= 2 {
                        hits += 1;   // +-2 semitones, any octave
                    }
                }
            }
            (hits as f64 / n_pts, cov as f64 / n_pts)
        };
        let metric = |base: f64, pts: &[(f64, i64)]| -> (f64, f64) {
            let (s, c) = score(base, pts);
            if c >= 0.3 { (s, c) } else { (0.0, c) }
        };
        let base0 = self.base_ms;
        let (base_score, _) = score(base0, samples);
        let (mut best_d, mut best_s, mut best_c) = {
            let (s, c) = metric(base0, samples);
            (0i64, s, c)
        };
        let coarse: Vec<(f64, i64)> = if samples.len() > 60 {
            samples.iter().step_by(3).cloned().collect()
        } else {
            samples.to_vec()
        };
        let mut near_best = Vec::new();
        let mut d = -window_ms;
        while d <= window_ms {
            let (s, c) = metric(base0 + d as f64, &coarse);
            near_best.push((s, d));
            if s > best_s {
                best_d = d;
                best_s = s;
                best_c = c;
            }
            d += 250;
        }
        let tie: Vec<i64> = near_best
            .iter()
            .filter(|(s, _)| *s >= best_s - 0.03 && *s >= 0.5)
            .map(|(_, d)| *d)
            .collect();
        if !tie.is_empty() {
            best_d = tie.iter().min_by_key(|d| d.abs()).copied().unwrap();
            let (s, c) = score(base0 + best_d as f64, samples);
            best_s = s;
            best_c = c;
        }
        for (step, span) in [(25i64, 250i64), (5i64, 25i64)] {
            let mut d = (-window_ms).max(best_d - span);
            while d <= window_ms.min(best_d + span) {
                let (s, c) = metric(base0 + d as f64, samples);
                if s > best_s {
                    best_d = d;
                    best_s = s;
                    best_c = c;
                }
                d += step;
            }
        }
        (best_d, best_s, base_score, best_c)
    }

    /// Final per-track score map — call when playback ends.
    /// {track: FinishSummary} like Python's {"tracks": {t: s.finish()}}.
    pub fn finish(&mut self) -> BTreeMap<String, crate::scoring::FinishSummary> {
        self.scorers
            .iter_mut()
            .map(|(t, s)| (t.clone(), s.finish()))
            .collect()
    }
}
