//! Native TV renderer — a `Driver` trait abstracts where the playlist
//! comes from so the same render loop serves `ups-tv` standalone and
//! `ups-app --tv` (in-process kiosk, no browser).

pub mod render;
pub mod text;
pub mod video;
pub mod driver;

pub use driver::{Driver, PlayItem, run};
