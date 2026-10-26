//! ups-cli — inspect a prepared Ultra Pi Star library from Rust.

use std::path::PathBuf;
use ups_core::{catalog, chart, scoring};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        usage();
        return;
    };
    match cmd.as_str() {
        "scan" => cmd_scan(args.next().map(PathBuf::from)),
        "audit" => cmd_audit(args.next().map(PathBuf::from)),
        "info" => cmd_info(args.next().map(PathBuf::from)),
        _ => usage(),
    }
}

fn usage() {
    eprintln!(
        "ups-cli — Ultra Pi Star (rust)\n\n\
         usage:\n\
         \x20 ups scan <library root>     list every song\n\
         \x20 ups audit <library root>    report incomplete entries\n\
         \x20 ups info <chart.upsong>     chart summary"
    );
}

fn cmd_scan(root: Option<PathBuf>) {
    let Some(root) = root else { return usage() };
    let songs = catalog::scan(&root);
    for s in &songs {
        let badge = if s.is_complete() { "" } else { "  [incomplete]" };
        println!("{} - {}{}", s.artist, s.title, badge);
    }
    println!("{} songs", songs.len());
}

fn cmd_audit(root: Option<PathBuf>) {
    let Some(root) = root else { return usage() };
    let songs = catalog::scan(&root);
    let mut bad = 0;
    for s in &songs {
        let mut issues = Vec::new();
        let media = &s.media;
        let stem = media.file_stem().unwrap_or_default()
            .to_string_lossy().to_string();
        let d = &s.dir;
        if !d.join(format!("{stem}.inst.mp4")).is_file() {
            issues.push("no inst");
        }
        if !d.join(format!("{stem}.vocals.wav")).is_file()
            && !d.join(format!("{stem}.vocals.mp3")).is_file()
        {
            issues.push("no vocals stem");
        }
        if !d.join(format!("{stem}.analysis.json")).is_file() {
            issues.push("no analysis sidecar");
        }
        if s.art.is_none() {
            issues.push("no artwork");
        }
        // chart must exist and parse with notes
        let ch = s.chart.clone().filter(|p| p.is_file());
        match ch {
            None => issues.push("no chart"),
            Some(p) => {
                let c = chart::load_chart(&p);
                if !c.errors.is_empty() {
                    issues.push("chart unreadable");
                } else if c.tracks.values().all(|t| t.is_empty()) {
                    issues.push("chart has no notes");
                } else if c
                    .phrases()
                    .values()
                    .flatten()
                    .all(|ph| ph.text.trim().is_empty())
                {
                    issues.push("lyrics blank");
                }
            }
        }
        if !issues.is_empty() {
            bad += 1;
            println!("{} - {}: {}", s.artist, s.title, issues.join(", "));
        }
    }
    println!("{} songs — {} with problems", songs.len(), bad);
}

fn cmd_info(path: Option<PathBuf>) {
    let Some(path) = path else { return usage() };
    let c = chart::load_chart(&path);
    println!("{} - {}", c.artist(), c.title());
    println!("bpm {}  gap {}ms  duet {}", c.bpm(), c.gap_ms(), c.is_duet());
    for (t, notes) in &c.tracks {
        println!("  {}: {} notes, {} lines", t, notes.len(),
                 c.lines(t).len());
    }
    for e in c.validate() {
        println!("  ! {}", e);
    }
    // smoke-test the scorer: perfect run over P1
    let mut sc = scoring::Scorer::new(&c, "P1", 0, true);
    let end = c.tracks.get("P1").and_then(|n| n.last())
        .map(|n| n.start + n.length).unwrap_or(0);
    for beat in 0..=end {
        let tone = sc
            .lines
            .iter()
            .flatten()
            .find(|n| beat >= n.start && beat < n.start + n.length)
            .map(|n| n.pitch);
        sc.tick(beat, tone);
    }
    let f = sc.finish();
    println!("perfect run: {} pts ({} hits, {} perfect, max streak {})",
             f.score, f.hits, f.perfect, f.max_streak);
}
