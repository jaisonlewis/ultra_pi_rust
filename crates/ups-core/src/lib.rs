//! ups-core — the language-agnostic engine shared by every Ultra Pi
//! Star frontend (Pi, Switch, desktop). Ports of the Python reference
//! implementation under `ultrapistar/`; GPL-3.0.

pub mod catalog;
pub mod chart;
pub mod layout;
pub mod pitch;
pub mod scoring;
pub mod session;

pub use catalog::{scan, read_manifest, scan_folders, SongRec};
pub use chart::{load_chart, parse_chart, Chart, Note, Phrase};
pub use layout::{slug, song_folder, song_reldir};
pub use pitch::{freq_to_tone_abs, PitchDetector, PitchResult};
pub use scoring::{Scorer, TickEvent};
