//! Live singing session — the beat clock and tick loop from
//! `ultrapistar/sing.py` (GPL-3.0), minus the autosync/voice-model
//! machinery (prep-station charts already carry corrected GAPs).
//!
//! Flow: `Session::tick(position_ms, tone_abs)` whenever the media clock
//! advances — it walks every chart beat between ticks, feeds the latest
//! detected pitch into the `Scorer`, and returns the score state for
//! display.

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
    pub scorer: Scorer,
    pub last_beat: i64,
    /// recent sung tones (media_ms, tone_abs) for display/autosync later
    pub tone_log: Vec<(f64, i64)>,
}

pub struct TickResult {
    pub beat: i64,
    pub ms: f64,
    pub score: i64,
    pub streak: i64,
    pub last_hit: Option<TickEvent>,
    pub line_events: Vec<LineEvent>,
    /// expected chart pitch at this beat (pitch-lane display / autotune)
    pub target: Option<i64>,
}

impl Session {
    pub fn new(chart: Chart, track: &str, difficulty: i64,
               line_bonus: bool) -> Self {
        let beat_ms = if chart.bpm() > 0.0 { 15000.0 / chart.bpm() } else { 0.0 };
        let scorer = Scorer::new(&chart, track, difficulty, line_bonus);
        Session {
            base_ms: chart.gap_ms(),
            chart,
            difficulty,
            beat_ms,
            scorer,
            last_beat: -1,
            tone_log: Vec::new(),
        }
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

    /// Advance scoring to `position_ms` of media time, feeding
    /// `tone_abs` (latest mic pitch 0..45, or None) into every beat.
    pub fn tick(&mut self, position_ms: f64,
                tone_abs: Option<i64>) -> TickResult {
        let beat = self.beat_at(position_ms).max(self.last_beat);
        if let Some(t) = tone_abs.filter(|&t| t >= 0) {
            self.tone_log.push((position_ms, t));
            if self.tone_log.len() > 4000 {
                self.tone_log.drain(..1000);
            }
        }
        let mut last_hit = None;
        let mut line_events = Vec::new();
        while self.last_beat < beat {
            self.last_beat += 1;
            if let Some(ev) = self.scorer.tick(self.last_beat, tone_abs) {
                if let Some(le) = &ev.line {
                    line_events.push(le.clone());
                }
                last_hit = Some(ev);
            }
        }
        // expected chart pitch at this beat
        let target = self
            .scorer
            .lines
            .iter()
            .flatten()
            .find(|n| {
                n.start <= beat && beat < n.start + n.length
                    && n.kind != "freestyle"
            })
            .map(|n| n.pitch);
        TickResult {
            beat,
            ms: position_ms,
            score: self.scorer.state.total(),
            streak: self.scorer.state.streak,
            last_hit,
            line_events,
            target,
        }
    }

    /// Final score map — call when playback ends.
    pub fn finish(&mut self) -> crate::scoring::FinishSummary {
        self.scorer.finish()
    }
}
