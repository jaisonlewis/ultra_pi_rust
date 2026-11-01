use rodio::Source;
use std::path::Path;
use ups_core::decode::StreamingSource;
use ups_core::decode::open_audio;

// Regression for the ABBA Super Trouper failure: a 96kHz AAC-LC
// stream inside an HEVC mp4 whose mp4a sample-entry rate field is 0
// (the real rate lives in the esds). Symphonia reported
// rate=Some(0), the old track filter rejected it, audio decode died
// with "no audio track" and the song skipped.
#[test]
fn decodes_96k_inst_mp4() {
    let p = Path::new(r"I:\tmp\ups-preview\yt-BshxCIjNEjY.inst.mp4");
    if !p.exists() {
        eprintln!("fixture absent, skipping");
        return;
    }
    let src = StreamingSource::open(p).expect("open");
    assert_eq!(src.channels(), 2);
    assert_eq!(src.sample_rate(), 96000);
    let mut n = 0u64;
    let t0 = std::time::Instant::now();
    for s in src {
        let _ = s;
        n += 1;
    }
    eprintln!(
        "done: {n} samples = {:.1}s @ 96000, decode took {:.1}s",
        n as f64 / 96_000.0 / 2.0,
        t0.elapsed().as_secs_f64()
    );
    assert!(n > 96_000, "decoder yielded {n} samples — silent song");
}

#[test]
fn ffmpeg_source_decodes() {
    let p = Path::new(r"I:\tmp\ups-preview\yt-BshxCIjNEjY.inst.mp4");
    if !p.exists() {
        eprintln!("fixture absent, skipping");
        return;
    }
    let s = ups_core::decode::FfmpegSource::open(p)
        .expect("ffmpeg source");
    let n = s.take(400_000).count();
    assert_eq!(n, 400_000, "ffmpeg pipe underflowed");
}

#[test]
fn open_audio_falls_back() {
    let p = Path::new(r"I:\tmp\ups-preview\yt-BshxCIjNEjY.inst.mp4");
    if !p.exists() {
        eprintln!("fixture absent, skipping");
        return;
    }
    let (s, tag) = open_audio(p).expect("open_audio");
    eprintln!("decoder: {tag}");
    let n = s.take(200_000).count();
    assert_eq!(n, 200_000, "{tag} underflowed");
}
