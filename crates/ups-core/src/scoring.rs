//! Scoring engine — Rust port of UltraStar WorldParty's sing-update
//! logic (`src/base/UNote.pas` + line bonus from
//! `src/screens/controllers/UScreenSingController.pas`, GPL-3.0),
//! via `ultrapistar/scoring.py`.
//!
//! Faithful to upstream:
//!   * each beat with a covering non-freestyle chart note is scoreable
//!   * the sung tone is octave-normalized within +-6 semitones of target
//!   * hit tolerance = 2 - difficulty halftones (Easy=2, Medium=1, Hard=0);
//!     rap notes always count as hits
//!   * points per hit beat = MaxSongPoints / ScoreValue * ScoreFactor[type]
//!   * with line bonus enabled MaxSongPoints is 9000 and up to 1000 more
//!     is distributed per phrase by line perfection
//!   * golden points accumulate separately and are rounded complementary
//!     to the normal score so the total caps at 10000

use crate::chart::{Chart, Note};
use std::collections::HashMap;

pub const MAX_SONG_SCORE: f64 = 10000.0;
pub const MAX_SONG_LINE_BONUS: f64 = 1000.0;
pub const MAX_LINE_RATING: i64 = 8;

pub fn score_factor(kind: &str) -> f64 {
    match kind {
        "freestyle" => 0.0,
        "golden" | "rap-golden" => 2.0,
        _ => 1.0,
    }
}

#[derive(Debug, Clone)]
pub struct PlayerNote {
    pub start: i64,
    pub length: i64,
    pub tone: i64,
    pub hit: bool,
    pub kind: String,
    pub perfect: bool,
}

#[derive(Debug, Default)]
pub struct PlayerScore {
    pub score: f64,        // normal + rap note points
    pub score_golden: f64, // golden note points
    pub score_line: f64,   // accumulated line bonuses
    pub score_last: f64,   // score+golden at last phrase end
    pub notes: Vec<PlayerNote>,
    pub last_sentence_perfect: bool,
    pub streak: i64,
    pub max_streak: i64,
}

impl PlayerScore {
    /// complementary-rounded display values (upstream ScoreInt etc.)
    pub fn score_int(&self) -> i64 {
        (self.score / 10.0).round() as i64 * 10
    }
    pub fn score_golden_int(&self) -> i64 {
        if (self.score_int() as f64) < self.score {
            (self.score_golden / 10.0).ceil() as i64 * 10
        } else {
            (self.score_golden / 10.0).floor() as i64 * 10
        }
    }
    pub fn score_line_int(&self) -> i64 {
        (self.score_line / 10.0).floor() as i64 * 10
    }
    pub fn total(&self) -> i64 {
        self.score_int() + self.score_golden_int() + self.score_line_int()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TickEvent {
    pub beat: i64,
    pub hit: Option<bool>,
    pub tone: Option<i64>,
    pub kind: Option<String>,
    pub score: Option<i64>,
    pub line: Option<LineEvent>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LineEvent {
    pub line: usize,
    pub perfection: f64,
    pub rating: i64,
    pub score: i64,
}

/// Scores one player against one chart track, one beat at a time.
///
/// Call `tick(beat, tone_abs)` for every chart beat while a verified song
/// plays, `phrase_end` is implicit via the line_end bookkeeping, then read
/// `state` for the live score.
pub struct Scorer {
    pub lines: Vec<Vec<Note>>,
    pub range: i64,
    pub line_bonus: bool,
    pub max_song_points: f64,
    pub score_value: f64,
    pub num_lines: usize,
    /// beat -> (line_idx, note_idx)
    beat_map: HashMap<i64, (usize, usize)>,
    line_max: Vec<f64>,
    line_end: Vec<i64>,
    pub state: PlayerScore,
    pending_line: Option<usize>,
    /// notes not yet past their last beat, oldest first: (line,note), any_hit
    note_eval: Vec<((usize, usize), bool)>,
}

impl Scorer {
    pub fn new(chart: &Chart, track: &str, difficulty: i64, line_bonus: bool) -> Self {
        let lines = chart.lines(track);
        let range = 2 - difficulty.clamp(0, 2);
        let max_song_points = if line_bonus {
            MAX_SONG_SCORE - MAX_SONG_LINE_BONUS
        } else {
            MAX_SONG_SCORE
        };
        let score_value: f64 = lines
            .iter()
            .flatten()
            .map(|n| n.length as f64 * score_factor(&n.kind))
            .sum();
        let num_lines = lines.iter().filter(|l| !l.is_empty()).count();
        let mut beat_map = HashMap::new();
        let mut line_max = Vec::new();
        let mut line_end = Vec::new();
        for (li, line) in lines.iter().enumerate() {
            line_max.push(
                line.iter()
                    .map(|n| n.length as f64 * score_factor(&n.kind))
                    .sum(),
            );
            line_end.push(
                line.last().map(|n| n.start + n.length - 1).unwrap_or(-1),
            );
            for (ni, n) in line.iter().enumerate() {
                if n.kind == "freestyle" || n.length <= 0 {
                    continue;
                }
                for b in n.start..n.start + n.length {
                    beat_map.insert(b, (li, ni));
                }
            }
        }
        Scorer {
            lines,
            range,
            line_bonus,
            max_song_points,
            score_value,
            num_lines,
            beat_map,
            line_max,
            line_end,
            state: PlayerScore::default(),
            pending_line: None,
            note_eval: Vec::new(),
        }
    }

    pub fn points_per_beat(&self, note: &Note) -> f64 {
        if self.score_value == 0.0 {
            return 0.0;
        }
        self.max_song_points / self.score_value * score_factor(&note.kind)
    }

    /// Port of the UNote.pas sing loop for one player/beat.
    /// `tone_abs` = detector's absolute tone (0..45) or None when no pitch.
    pub fn tick(&mut self, beat: i64, tone_abs: Option<i64>) -> Option<TickEvent> {
        let mut line_event = None;
        self.eval_notes(beat);
        let Some(&(line_idx, note_idx)) = self.beat_map.get(&beat) else {
            // line bonus can still fire on noteless beats between phrases
            if let Some(pl) = self.pending_line {
                if beat > self.line_end[pl] {
                    line_event = self.apply_line_bonus(pl);
                    self.pending_line = None;
                }
            }
            return line_event.map(|le| TickEvent {
                beat,
                hit: None,
                tone: None,
                kind: None,
                score: None,
                line: Some(le),
            });
        };
        let note = self.lines[line_idx][note_idx].clone();
        if self
            .note_eval
            .last()
            .map(|&(k, _)| k != (line_idx, note_idx))
            .unwrap_or(true)
        {
            self.note_eval.push(((line_idx, note_idx), false));
        }

        if let Some(pl) = self.pending_line {
            if beat > self.line_end[pl] {
                line_event = self.apply_line_bonus(pl);
                self.pending_line = None;
            }
        }
        if beat == self.line_end[line_idx] {
            self.pending_line = Some(line_idx);
        }

        let Some(tone_abs) = tone_abs.filter(|&t| t >= 0) else {
            return Some(TickEvent {
                beat,
                hit: Some(false),
                tone: None,
                kind: None,
                score: None,
                line: line_event,
            });
        };

        // octave-normalize sung tone within +-6 of the target note
        let mut tone = tone_abs;
        while tone - note.pitch > 6 {
            tone -= 12;
        }
        while tone - note.pitch < -6 {
            tone += 12;
        }

        let is_hit = (note.pitch - tone).abs() <= self.range
            || matches!(note.kind.as_str(), "rap" | "rap-golden");
        let display_tone = if is_hit { note.pitch } else { tone };
        if let Some(last) = self.note_eval.last_mut() {
            last.1 = last.1 || is_hit;
        }

        if is_hit {
            let pts = self.points_per_beat(&note);
            if matches!(note.kind.as_str(), "golden" | "rap-golden") {
                self.state.score_golden += pts;
            } else {
                self.state.score += pts;
            }
        }

        // record/extend the player's note (upstream: extend when same tone
        // and contiguous)
        match self.state.notes.last_mut() {
            Some(last)
                if last.tone == display_tone && last.start + last.length == beat =>
            {
                last.length += 1;
                last.hit = last.hit || is_hit;
            }
            _ => self.state.notes.push(PlayerNote {
                start: beat,
                length: 1,
                tone: display_tone,
                hit: is_hit,
                kind: note.kind.clone(),
                perfect: is_hit && note.start == beat,
            }),
        }

        Some(TickEvent {
            beat,
            hit: Some(is_hit),
            tone: Some(display_tone),
            kind: Some(note.kind.clone()),
            score: Some(self.state.total()),
            line: line_event,
        })
    }

    /// Settle completed notes into the streak counter (SingStar-style
    /// combo: any note with at least one hit beat keeps the streak).
    fn eval_notes(&mut self, beat: i64) {
        while let Some(&((li, ni), hit)) = self.note_eval.first() {
            let n = &self.lines[li][ni];
            if n.start + n.length > beat {
                break;
            }
            self.note_eval.remove(0);
            self.state.streak = if hit { self.state.streak + 1 } else { 0 };
            self.state.max_streak = self.state.max_streak.max(self.state.streak);
        }
    }

    /// Port of UScreenSingController.OnSentenceEnd line bonus.
    fn apply_line_bonus(&mut self, line_idx: usize) -> Option<LineEvent> {
        if !self.line_bonus || self.num_lines == 0 {
            return None;
        }
        let current = self.state.score + self.state.score_golden;
        let line_score = current - self.state.score_last;
        let max_line = if self.score_value != 0.0 {
            self.max_song_points / self.score_value * self.line_max[line_idx]
        } else {
            0.0
        };
        let perfection = if max_line <= 2.0 {
            1.0
        } else {
            (line_score / (max_line - 2.0)).clamp(0.0, 1.0)
        };
        self.state.score_line +=
            (MAX_SONG_LINE_BONUS / self.num_lines as f64) * perfection;
        self.state.score_last = current;
        self.state.last_sentence_perfect = perfection >= 1.0;
        Some(LineEvent {
            line: line_idx,
            perfection,
            rating: (perfection * MAX_LINE_RATING as f64).round() as i64,
            score: self.state.total(),
        })
    }

    /// Flush any trailing phrase bonus; returns final per-track summary.
    pub fn finish(&mut self) -> FinishSummary {
        if let Some(pl) = self.pending_line.take() {
            self.apply_line_bonus(pl);
        }
        while let Some((_, hit)) = self.note_eval.first().copied() {
            self.note_eval.remove(0);
            self.state.streak = if hit { self.state.streak + 1 } else { 0 };
            self.state.max_streak = self.state.max_streak.max(self.state.streak);
        }
        let s = &self.state;
        FinishSummary {
            score: s.total(),
            score_normal: s.score_int(),
            score_golden: s.score_golden_int(),
            score_line: s.score_line_int(),
            notes: s.notes.len(),
            hits: s.notes.iter().filter(|n| n.hit).count(),
            perfect: s.notes.iter().filter(|n| n.perfect).count(),
            max_streak: s.max_streak,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FinishSummary {
    pub score: i64,
    pub score_normal: i64,
    pub score_golden: i64,
    pub score_line: i64,
    pub notes: usize,
    pub hits: usize,
    pub perfect: usize,
    pub max_streak: i64,
}
