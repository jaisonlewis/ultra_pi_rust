//! ups-tv — standalone native karaoke screen. Plays one song with the
//! same renderer `ups-app --tv` uses for the queue.
//!
//! usage: ups-tv <library-root> <search>
//!        ups-tv <song-dir>

use std::path::PathBuf;
use std::sync::Arc;

use ups_core::catalog;
use ups_tv::{run, Driver, PlayItem};

struct OneShot {
    item: std::sync::Mutex<Option<PlayItem>>,
    done: std::sync::Mutex<bool>,
}

impl Driver for OneShot {
    fn current(&self) -> Option<PlayItem> {
        if *self.done.lock().unwrap() {
            return None;
        }
        self.item.lock().unwrap().clone()
    }
    fn item_done(&self, _id: i64, score: i64) {
        *self.done.lock().unwrap() = true;
        eprintln!("final score: {score}");
    }
    fn item_error(&self, _id: i64, msg: &str) {
        *self.done.lock().unwrap() = true;
        eprintln!("playback error: {msg}");
    }
}

fn resolve(args: &[String]) -> Option<PlayItem> {
    let path = PathBuf::from(&args[0]);
    let needle = args.get(1).cloned().unwrap_or_default().to_lowercase();
    let s = if path.is_dir() && needle.is_empty() {
        let parent = path.parent()?.to_path_buf();
        catalog::scan_folders(&parent)
            .into_iter()
            .find(|s| s.media.parent() == Some(path.as_path()))
    } else {
        catalog::scan(&path)
            .into_iter()
            .find(|s| {
                s.title.to_lowercase().contains(&needle)
                    || s.artist.to_lowercase().contains(&needle)
            })
    }?;
    let name = s.media.display().to_string().to_lowercase();
    let audio = s.inst.clone().unwrap_or_else(|| s.media.clone());
    // video from the played file — the karaoke mp4 carries its own
    // stream, and one file = one timeline = no drift
    let audio_only = ["mp3", "m4a", "ogg", "wav", "flac"]
        .iter()
        .any(|e| name.ends_with(e));
    Some(PlayItem {
        id: 1,
        audio: audio.clone(),
        video: if audio_only { None } else { Some(audio) },
        chart: s.chart,
        artist: s.artist,
        title: s.title,
        audio_only,
        singer: "you".into(),
        tracks: vec!["P1".into()],
        sync_offset: 0.0,
        lyrics_rom: std::env::var("UPS_ROMAJI")
            .map(|v| v != "0")
            .unwrap_or(false),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: ups-tv <root|dir> [search]");
        return;
    }
    let Some(item) = resolve(&args[1..]) else {
        eprintln!("song not found");
        return;
    };
    eprintln!("playing: {} — {}", item.artist, item.title);
    let _ = run(Arc::new(OneShot {
        item: std::sync::Mutex::new(Some(item)),
        done: std::sync::Mutex::new(false),
    }));
}
