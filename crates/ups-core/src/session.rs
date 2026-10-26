//! Live singing session — the beat clock and tick loop from
//! `ultrapistar/sing.py` (GPL-3.0), minus the autosync/voice-model
//! machinery (prep-station charts already carry corrected GAPs).
//!
//! Flow: `Session::tick(position_ms, tones)` whenever the media clock
//! advances — it walks every chart beat between ticks, feeds each
//! track's latest detected pitch into its `Scorer`, and returns the
//! score state for display.

use std::collections::BTreeMap;

use crate::chart::Chart;
use crate::scoring::{LineEvent, Scorer, TickEvent};

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
    /// recent sung tones (media_ms, tone_abs) for display/autosync later
    pub tone_log: Vec<(f64, i64)>,
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
            tone_log: Vec::new(),
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
                self.tone_log.push((position_ms, t));
                if self.tone_log.len() > 4000 {
                    self.tone_log.drain(..1000);
                }
            }
            let _ = track;
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

    /// Final per-track score map — call when playback ends.
    /// {track: FinishSummary} like Python's {"tracks": {t: s.finish()}}.
    pub fn finish(&mut self) -> BTreeMap<String, crate::scoring::FinishSummary> {
        self.scorers
            .iter_mut()
            .map(|(t, s)| (t.clone(), s.finish()))
            .collect()
    }
}
