//! Parity tests against the Python reference behavior.

use ups_core::chart::{self, Chart, Note};
use ups_core::layout::{slug, song_reldir};
use ups_core::pitch::{freq_to_tone_abs, PitchDetector};
use ups_core::scoring::Scorer;

fn demo_chart() -> Chart {
    // two 4-beat notes in one line, then a break, one note in line 2
    let mut c = Chart::new();
    c.headers.insert("ARTIST".into(), "Test".into());
    c.headers.insert("TITLE".into(), "Song".into());
    c.headers.insert("BPM".into(), "120".into());
    let notes = vec![
        { let mut n = Note::default2("normal", 0, 4, 12, "hel"); n.word_break = true; n },
        Note::default2("normal", 4, 4, 12, "lo"),
        Note::default2("golden", 12, 4, 14, "world"),
    ];
    c.tracks.insert("P1".into(), notes);
    c.breaks.insert("P1".into(), vec![8]);
    c
}

trait Default2 {
    fn default2(kind: &str, s: i64, l: i64, p: i64, t: &str) -> Note;
}
impl Default2 for Note {
    fn default2(kind: &str, s: i64, l: i64, p: i64, t: &str) -> Note {
        Note {
            kind: kind.into(), start: s, length: l, pitch: p,
            text: t.into(), word_break: false, alt: String::new(),
        }
    }
}

#[test]
fn slug_matches_python() {
    assert_eq!(slug("David Bowie", ""), "david_bowie");
    assert_eq!(slug("Is There Life on Mars?", ""), "is_there_life_on_mars");
    assert_eq!(slug("", ""), "");
    assert_eq!(slug("", "yt-abc"), "yt-abc");
    assert_eq!(song_reldir("R.E.M.", "Losing My Religion", ""),
               "r_e_m/losing_my_religion");
    // unicode letters survive
    assert_eq!(slug("トゥー・マッチ", ""), "トゥー_マッチ");
}

#[test]
fn perfect_run_scores_max() {
    let c = demo_chart();
    let mut sc = Scorer::new(&c, "P1", 0, true);
    for beat in 0..=16 {
        let tone = sc.lines.iter().flatten()
            .find(|n| beat >= n.start && beat < n.start + n.length)
            .map(|n| n.pitch);
        sc.tick(beat, tone);
    }
    let f = sc.finish();
    assert_eq!(f.score, 10000);
    // player sung-notes merge on contiguous same-tone beats —
    // 2 sung notes covering 3 chart notes (upstream behavior)
    assert_eq!(f.hits, 2);
    assert_eq!(f.perfect, 2);
}

#[test]
fn octave_normalize_counts_hits() {
    // singing an octave off still scores (+-6 semitone normalize)
    let c = demo_chart();
    let mut sc = Scorer::new(&c, "P1", 0, true);
    let ev = sc.tick(0, Some(24)); // target 12, sung 24 -> octave up
    assert_eq!(ev.unwrap().hit, Some(true));
}

#[test]
fn misses_score_zero() {
    let c = demo_chart();
    let mut sc = Scorer::new(&c, "P1", 0, true);
    for beat in 0..8 {
        sc.tick(beat, Some(40)); // way off
    }
    assert!(sc.state.score == 0.0);
}

#[test]
fn pitch_pure_tone() {
    // 440Hz sine -> tone_abs for A4 (index 33 = 65.4064 * 2^(33/12) = 440)
    let mut det = PitchDetector::new(44100, 2);
    let pcm: Vec<i16> = (0..4096)
        .map(|i| (16000.0 * (i as f64 * 440.0 * std::f64::consts::TAU / 44100.0).sin()) as i16)
        .collect();
    det.feed(&pcm);
    let r = det.analyze();
    assert!(r.valid);
    assert_eq!(r.tone_abs, 33); // A4
    assert_eq!(r.tone_str(), "A4");
    assert_eq!(freq_to_tone_abs(440.0), 33);
}

#[test]
fn silence_is_invalid() {
    let mut det = PitchDetector::new(44100, 2);
    det.feed(&vec![0i16; 4096]);
    assert!(!det.analyze().valid);
}

#[test]
fn txt_parser_word_breaks() {
    let dir = std::env::temp_dir().join(format!("ups-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let txt = dir.join("t.txt");
    std::fs::write(&txt, "#ARTIST:ABBA\n#TITLE:Dancing Queen\n#BPM:100\n#GAP:0\n: 0 4 60 Dan\n: 4 4 62 ~cing\n- 8\n: 12 4 60 Queen\nE\n").unwrap();
    let c = chart::parse_chart(&txt);
    assert_eq!(c.artist(), "ABBA");
    let notes = &c.tracks["P1"];
    assert_eq!(notes.len(), 3);
    assert!(notes[0].word_break);
    assert!(!notes[1].word_break);      // '~' glues
    assert_eq!(notes[1].text, "cing");
    // phrase breaks split lines but do NOT set word_break — upstream
    assert!(!notes[2].word_break);
    assert_eq!(c.breaks["P1"], vec![8]);
    assert_eq!(c.lines("P1").len(), 2);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn romanize_kana_and_cyrillic() {
    let mut c = demo_chart();
    c.tracks.get_mut("P1").unwrap()[0].text = "センチメンタルな恋".into();
    c.tracks.get_mut("P1").unwrap()[1].text = "キラキラ".into();
    ups_core::romanize::romanize_chart(&mut c);
    let ns = &c.tracks["P1"];
    assert_eq!(ns[0].alt, "senchimentaru na koi");
    assert_eq!(ns[1].alt, "kirakira");
    assert_eq!(c.headers.get("ALTSCRIPT").map(|s| s.as_str()), Some("romaji"));
}

#[test]
fn romanize_script_detection() {
    assert_eq!(ups_core::romanize::script_of("革命道中"), "zh");
    assert_eq!(ups_core::romanize::script_of("かな革命"), "jp");
    assert_eq!(ups_core::romanize::script_of("사랑해"), "kr");
    assert_eq!(ups_core::romanize::script_of("Москва"), "cyr");
    assert_eq!(ups_core::romanize::script_of("hello"), "latin");
}
