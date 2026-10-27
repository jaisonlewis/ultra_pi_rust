//! ups-prep — portable song-prep station. Takes a YouTube URL or a
//! local media file and produces a stick-format song folder:
//!   <Library>/Artist - Title/<stem>.mp4        media (loudnormed)
//!                            <stem>.inst.mp4   karaoke video (vocal cut)
//!                            <stem>.vocals.wav 16k vocal stem
//!                            <stem>.upsong     chart (imported/generated)
//!                            <stem>.jpg        artwork
//!                            <stem>.analysis.json  pipeline sidecar
//!
//! Tools on PATH: yt-dlp, ffmpeg, ffprobe. Optional: the sherpa-onnx
//! source-separation binary + spleeter int8 models (see --models).

mod align;
mod artwork;
mod asr;
mod ff;
mod genchart;
mod lyrics;
mod mel;
mod rmvpe;
mod separate;
mod wordmap;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Args {
    source: String,
    library: PathBuf,
    models: PathBuf,
    chart: Option<PathBuf>,
    artist: Option<String>,
    title: Option<String>,
    guide: f32,
    no_separate: bool,
    /// `batch` mode — regenerate charts across a whole library.
    batch: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = std::env::args().skip(1);
    let mut args = Args {
        source: String::new(),
        library: PathBuf::from("."),
        models: std::env::var("UPS_MODELS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("models")),
        chart: None,
        artist: None,
        title: None,
        guide: 0.15,
        no_separate: false,
        batch: false,
    };
    while let Some(arg) = a.next() {
        match arg.as_str() {
            "batch" => args.batch = true,
            "--library" => args.library = a.next().context("--library")?.into(),
            "--models" => args.models = a.next().context("--models")?.into(),
            "--chart" => args.chart = Some(a.next().context("--chart")?.into()),
            "--artist" => args.artist = Some(a.next().context("--artist")?),
            "--title" => args.title = Some(a.next().context("--title")?),
            "--guide" => args.guide = a.next().context("--guide")?.parse()?,
            "--no-separate" => args.no_separate = true,
            "-h" | "--help" => {
                println!(
                    "ups-prep <youtube-url | media-file> [opts]\n\
                     ups-prep batch --library DIR   regenerate charts\n\
                     \x20 --library DIR    stick root (default .)\n\
                     \x20 --models DIR     model dir (default ./models or \
                     $UPS_MODELS_DIR)\n\
                     \x20 --chart FILE     import a .txt/.upsong chart\n\
                     \x20 --artist NAME    override artist\n\
                     \x20 --title NAME     override title\n\
                     \x20 --guide F        guide-vocal level 0..1 \
                     (default 0.15)\n\
                     \x20 --no-separate    skip vocal separation\n\n\
                     batch re-runs ASR + RMVPE + genchart on every song\n\
                     whose .upsong was machine-generated (CREATOR\n\
                     ultrapistar-genchart) or missing; imported charts\n\
                     are left alone. Reuses the stored transcript when\n\
                     analysis.json already holds a parakeet pass."
                );
                std::process::exit(0);
            }
            s if s.starts_with("--") => bail!("unknown flag {s}"),
            s => args.source = s.to_string(),
        }
    }
    if !args.batch && args.source.is_empty() {
        bail!("need a youtube url or media file");
    }
    Ok(args)
}

fn stage(label: &str) {
    println!("==> {label}");
}

struct MediaInfo {
    path: PathBuf,
    artist: String,
    title: String,
    thumb_url: Option<String>,
}

/// Download via yt-dlp into `dir`, or pass through a local file.
fn fetch_media(args: &Args, dir: &Path) -> Result<MediaInfo> {
    let src = args.source.trim();
    if !src.contains("://") && !src.starts_with("www.") {
        let p = PathBuf::from(src);
        if !p.exists() {
            bail!("no such file: {src}");
        }
        return Ok(MediaInfo {
            artist: args.artist.clone().unwrap_or_default(),
            title: args.title.clone().unwrap_or_else(|| {
                p.file_stem().unwrap_or_default()
                    .to_string_lossy().into_owned()
            }),
            thumb_url: None,
            path: p,
        });
    }
    stage("downloading");
    let out_tmpl = dir.join("yt-%(id)s.%(ext)s");
    let out = Command::new("yt-dlp")
        .args([
            "--extractor-args", "youtube:player_client=android",
            "-f", "best[ext=mp4]/bestvideo[ext=mp4]+bestaudio/best",
            "--print-json", "-o",
        ])
        .arg(&out_tmpl)
        .arg(src)
        .output()
        .context("spawn yt-dlp")?;
    if !out.status.success() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&out.stderr)
            .lines().last().unwrap_or("failed"));
    }
    let info: serde_json::Value =
        serde_json::from_slice(&out.stdout)
            .context("yt-dlp --print-json")?;
    let path = PathBuf::from(
        info["requested_downloads"][0]["filepath"]
            .as_str()
            .or(info["filepath"].as_str())
            .or(info["_filename"].as_str())
            .context("yt-dlp filepath")?,
    );
    let yt_title = info["title"].as_str().unwrap_or("").to_string();
    let channel = info["channel"].as_str()
        .or(info["uploader"].as_str()).unwrap_or("");
    let (artist, title) = split_artist_title(channel, &yt_title);
    Ok(MediaInfo {
        path,
        thumb_url: info["thumbnail"].as_str().map(String::from),
        artist: args.artist.clone().unwrap_or(artist),
        title: args.title.clone().unwrap_or(title),
    })
}

fn split_artist_title(channel: &str, yt_title: &str) -> (String, String) {
    for sep in [" - ", " – ", " — "] {
        if let Some((a, t)) = yt_title.split_once(sep) {
            return (a.trim().into(),
                    lyrics::yt_candidates("", t)
                        .first().map(|x| x.1.clone())
                        .unwrap_or_else(|| t.trim().into()));
        }
    }
    (channel.trim().into(), yt_title.trim().into())
}

fn main() -> Result<()> {
    let args = parse_args()?;
    if args.batch {
        return run_batch(&args);
    }

    // provisional dir — download first so yt metadata names it
    let dl_dir = std::env::temp_dir().join(format!(
        "ups-prep-{}", std::process::id()));
    std::fs::create_dir_all(&dl_dir)?;
    let info = fetch_media(&args, &dl_dir)?;

    let artist = if info.artist.is_empty() { "unknown".into() }
        else { info.artist.clone() };
    let title = if info.title.is_empty() {
        info.path.file_stem().unwrap_or_default()
            .to_string_lossy().into_owned()
    } else { info.title.clone() };
    println!("song: {artist} — {title}");

    // final folder in the library
    let folder = args.library
        .join(ups_core::layout::song_folder(&artist, &title, "song"));
    std::fs::create_dir_all(&folder)?;
    let stem = info.path.file_stem().unwrap_or_default()
        .to_string_lossy().into_owned();
    let media = folder.join(info.path.file_name().unwrap());
    if media != info.path {
        std::fs::copy(&info.path, &media)?;
    }
    let stem_path = |ext: &str| media.with_extension(ext);
    let _ = stem.clone();

    // phase 0 — loudness
    stage("normalizing loudness");
    if let Err(e) = ff::normalize(&media) {
        eprintln!("loudnorm skipped: {e}");
    }

    // phase 1 — stems → karaoke track
    if !args.no_separate && separate::have_models(&args.models) {
        let inst = stem_path("inst.mp4");
        let voc = stem_path("vocals.wav");
        if !inst.exists() || !voc.exists() {
            stage("separating vocals");
            match separate::separate_stems(&media, &args.models) {
                Ok((vocals, accomp)) => {
                    stage("building karaoke track");
                    let mix = separate::karaoke_mix(
                        &vocals, &accomp, args.guide, 150.0);
                    ff::karaoke_mp4(&media, &mix, &inst)?;
                    stage("writing vocal stem");
                    let mono = separate::mono16(&vocals)?;
                    ff::write_wav(&voc, &mono, 16000, 1)?;
                    stage("normalizing karaoke track");
                    let _ = ff::normalize(&inst);
                }
                Err(e) => eprintln!("separation failed: {e}"),
            }
        }
    } else if !args.no_separate {
        eprintln!("separation models/binary not found under {} — \
                   skipping vocal cut", args.models.display());
    }

    // ASR — word onsets for chart generation + lyric anchoring.
    // Vocal stem first; full media when the stem won't transcribe.
    let mut asr_words: Vec<asr::AsrWord> = Vec::new();
    if asr::have_models(&args.models) {
        stage("transcribing (asr)");
        let voc = stem_path("vocals.wav");
        let stem = voc.exists().then_some(voc.as_path());
        match asr::transcribe_media(&media, stem, &args.models) {
            Ok(w) => {
                println!("asr: {} words", w.len());
                asr_words = w;
            }
            Err(e) => eprintln!("asr failed: {e}"),
        }
    }

    // lyrics — gather candidate records, keep the one whose text best
    // matches what the ASR actually heard (LRCLIB stubs and same-title
    // different songs lose on word coverage, not on ordering)
    stage("fetching lyrics");
    let dur = ff::duration(&media).ok();
    let mut recs: Vec<lyrics::LyricRecord> = Vec::new();
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let push_rec = |r: Option<lyrics::LyricRecord>,
                        recs: &mut Vec<lyrics::LyricRecord>,
                        seen: &mut std::collections::HashSet<String>| {
        if let Some(r) = r {
            let key = format!("{}|{}", r.artist_name, r.track_name);
            if seen.insert(key) {
                recs.push(r);
            }
        }
    };
    for (a, t) in lyrics::yt_candidates(&artist, &title) {
        push_rec(lyrics::fetch(&a, &t, dur), &mut recs, &mut seen);
    }
    let q = format!("{artist} {title}").trim().to_string();
    for r in lyrics::search(&q, dur) {
        push_rec(Some(r), &mut recs, &mut seen);
    }
    let asr_norm: Vec<(String, f64)> = wordmap::norm_asr(
        &asr_words.iter()
            .map(|w| (w.word.clone(), w.start_ms, w.end_ms))
            .collect::<Vec<_>>());
    let rec = recs.into_iter()
        .map(|r| {
            let lines: Vec<String> = if r.synced.is_empty() {
                r.plain.clone()
            } else {
                r.synced.iter().map(|(_, t)| t.clone()).collect()
            };
            let sp = wordmap::map_line_words(&asr_norm, &lines);
            let used: usize = sp.values()
                .map(|s| s.words.len()).sum();
            (used, r)
        })
        .max_by_key(|(u, _)| *u)
        .filter(|(u, _)| *u >= 3)
        .map(|(_, r)| r);
    if let Some(r) = &rec {
        println!("lyrics: {} synced / {} plain lines \
                  ({} — {})",
                 r.synced.len(), r.plain.len(),
                 r.artist_name, r.track_name);
    } else {
        println!("lyrics: none found");
    }

    // phase 2 — chart: import + chroma-align to THIS media, then bake
    // to absolute media-ms so the player needs no warp machinery
    if let Some(chart_path) = &args.chart {
        stage("importing chart");
        let chart = ups_core::chart::load_chart(chart_path);
        let dest = stem_path("upsong");
        if (chart.bpm() - 15000.0).abs() > 1.0 {
            stage("aligning chart to media");
            match align::align_chart(&chart, &media, None) {
                Ok(Some(res)) => {
                    let baked = align::bake(&chart, &res);
                    ups_core::chart::save_native(&baked, &dest)?;
                    println!("chart: aligned off {}ms tempo {} \
                              (matched {}/{} phrases, peak {})",
                             res.offset_ms, res.tempo,
                             res.matched, res.phrases, res.peak);
                }
                Ok(None) => {
                    eprintln!("align: no confident match — importing \
                               unaligned (check timing)");
                    ups_core::chart::save_native(&chart, &dest)?;
                }
                Err(e) => eprintln!("align failed: {e}"),
            }
        } else {
            ups_core::chart::save_native(&chart, &dest)?;
        }
        let n: usize = chart.tracks.values().map(|v| v.len()).sum();
        println!("chart: {n} notes → {}", dest.display());
    } else {
        gen_chart(&args, &media, &artist, &title, &asr_words,
                  &stem_path("upsong"), rec.as_ref());
    }

    stage("fetching artwork");
    let art = stem_path("jpg");
    if !art.exists() {
        match artwork::save(&artist, &title, &art,
                            info.thumb_url.as_deref(),
                            Some(media.as_path())) {
            Ok(true) => println!("artwork: {}", art.display()),
            _ => eprintln!("artwork: none"),
        }
    }

    // analysis sidecar — what the pipeline gathered
    let ana = serde_json::json!({
        "prep": "ups-prep",
        "artist": artist, "title": title,
        "duration_s": dur,
        "lyrics": rec.map(|r| serde_json::json!({
            "synced_lines": r.synced.len(),
            "first_synced_ms": r.synced.first().map(|(t, _)| *t),
            "has_word_sync": r.has_word_sync,
            "lrc_shift_ms": r.synced.first().map(|(t, _)| *t),
        })),
        "has_inst": stem_path("inst.mp4").exists(),
        "has_chart": stem_path("upsong").exists(),
        "asr": {
            "engine": "sherpa-onnx parakeet-tdt-0.6b-v3",
            "words": asr_words.len(),
            "words_ms": asr_words.iter()
                .map(|w| serde_json::json!([w.start_ms, w.word]))
                .collect::<Vec<_>>(),
        },
    });
    std::fs::write(stem_path("analysis.json"),
                   serde_json::to_string_pretty(&ana)?)?;

    let _ = std::fs::remove_dir_all(&dl_dir);
    println!("done → {}", folder.display());
    Ok(())
}

/// `ups-prep batch --library <root>` — regenerate machine-made charts
/// with the current ASR+pitch stack. Songs whose .upsong was imported
/// (not marked ultrapibar-genchart/generated) are left alone.
fn run_batch(args: &Args) -> Result<()> {
    let songs = ups_core::catalog::scan(&args.library);
    let total = songs.len();
    let (mut redone, mut skipped, mut failed) = (0u32, 0u32, 0u32);
    for (i, song) in songs.iter().enumerate() {
        // an imported (non-generated) chart is human-timed — keep it
        if let Some(cp) = &song.chart {
            let c = ups_core::chart::load_chart(cp);
            let gen = c.headers.get("CREATOR")
                    .is_some_and(|v| v.contains("genchart"))
                || c.source["type"].as_str() == Some("generated");
            if !gen {
                skipped += 1;
                continue;
            }
        }
        if !song.media.is_file() {
            skipped += 1;
            continue;
        }
        println!("[{}/{}] {} — {}", i + 1, total,
                 song.artist, song.title);
        match batch_one(args, song) {
            Ok(true) => redone += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                eprintln!("  failed: {e}");
                failed += 1;
            }
        }
    }
    println!("batch done: {redone} regenerated, {skipped} skipped, \
              {failed} failed");
    Ok(())
}

/// Re-chart one library song; Ok(false) = nothing usable produced.
fn batch_one(args: &Args, song: &ups_core::catalog::Song) -> Result<bool> {
    let stem = song.media.file_stem().unwrap_or_default()
        .to_string_lossy().into_owned();
    // overwrite the generated chart in place — its filename may be
    // 'Artist - Title.upsong', not '<media-stem>.upsong'
    let dest = song.chart.clone()
        .unwrap_or_else(|| song.dir.join(format!("{stem}.upsong")));
    let ana_path = song.dir.join(format!("{stem}.analysis.json"));
    // folder names are the catalogued identity — chart headers may be
    // polluted by a previous bad lyric pick (wrong record's title got
    // baked in). Headers only fill in when the folder name is useless.
    let (artist, title) = {
        let bad = |s: &str| s.is_empty() || s == "unknown" || s == stem;
        let (mut a, mut t) = (song.artist.clone(), song.title.clone());
        if bad(&a) || bad(&t) {
            if let Some(cp) = &song.chart {
                let c = ups_core::chart::load_chart(cp);
                if bad(&a) {
                    if let Some(v) = c.headers.get("ARTIST")
                        .filter(|s| !s.is_empty()) {
                        a = v.clone();
                    }
                }
                if bad(&t) {
                    if let Some(v) = c.headers.get("TITLE")
                        .filter(|s| !s.is_empty()) {
                        t = v.clone();
                    }
                }
            }
        }
        (a, t)
    };

    // ASR — reuse the stored transcript when it's already a parakeet
    // pass; needle/empty/missing means re-transcribe
    let asr_words = read_cached_asr(&ana_path)
        .filter(|w| w.len() >= 20)
        .map(|w| {
            println!("  asr: {} cached words", w.len());
            w
        });
    let asr_words = match asr_words {
        Some(w) => w,
        None => {
            if !asr::have_models(&args.models) {
                return Ok(false);
            }
            let voc = song.dir.join(format!("{stem}.vocals.wav"));
            let stem_ref = voc.exists().then_some(voc.as_path());
            let w = asr::transcribe_media(&song.media, stem_ref,
                                          &args.models)?;
            println!("  asr: {} words", w.len());
            if w.len() < 20 {
                return Ok(false);
            }
            // refresh the sidecar so later batch runs reuse these
            if let Ok(bytes) = std::fs::read(&ana_path) {
                if let Ok(mut j) =
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                {
                    j["asr"] = serde_json::json!({
                        "engine": "sherpa-onnx parakeet-tdt-0.6b-v3",
                        "words": w.len(),
                        "words_ms": w.iter()
                            .map(|x| serde_json::json!(
                                [x.start_ms, x.word]))
                            .collect::<Vec<_>>(),
                    });
                    let _ = std::fs::write(
                        &ana_path, serde_json::to_string_pretty(&j)
                            .unwrap_or_default());
                }
            }
            w
        }
    };

    // lyrics — best-matching record, same pick as single-song mode
    let dur = ff::duration(&song.media).ok();
    let mut recs: Vec<lyrics::LyricRecord> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (a, t) in lyrics::yt_candidates(&artist, &title) {
        if let Some(r) = lyrics::fetch(&a, &t, dur) {
            if seen.insert(format!("{}|{}", r.artist_name, r.track_name)) {
                recs.push(r);
            }
        }
    }
    let q = format!("{artist} {title}");
    for r in lyrics::search(q.trim(), dur) {
        if seen.insert(format!("{}|{}", r.artist_name, r.track_name)) {
            recs.push(r);
        }
    }
    let asr_norm: Vec<(String, f64)> = wordmap::norm_asr(
        &asr_words.iter()
            .map(|w| (w.word.clone(), w.start_ms, w.end_ms))
            .collect::<Vec<_>>());
    let rec = recs.into_iter()
        .map(|r| {
            let lines: Vec<String> = if r.synced.is_empty() {
                r.plain.clone()
            } else {
                r.synced.iter().map(|(_, t)| t.clone()).collect()
            };
            let sp = wordmap::map_line_words(&asr_norm, &lines);
            (sp.values().map(|s| s.words.len()).sum::<usize>(), r)
        })
        .max_by_key(|(u, _)| *u)
        .filter(|(u, _)| *u >= 3)
        .map(|(_, r)| r);
    if let Some(r) = &rec {
        println!("  lyrics: {} synced lines ({} — {})",
                 r.synced.len(), r.artist_name, r.track_name);
    }

    Ok(gen_chart(args, &song.media, &artist, &title,
                 &asr_words, &dest, rec.as_ref()))
}

/// Cached transcript from `<stem>.analysis.json` when the ASR engine
/// is already parakeet — avoids re-transcribing on every batch run.
fn read_cached_asr(ana_path: &Path) -> Option<Vec<asr::AsrWord>> {
    let j: serde_json::Value =
        serde_json::from_slice(&std::fs::read(ana_path).ok()?).ok()?;
    let engine = j["asr"]["engine"].as_str().unwrap_or("");
    if !engine.contains("parakeet") {
        return None;
    }
    let words = j["asr"]["words_ms"].as_array()?.iter()
        .filter_map(|w| {
            Some(asr::AsrWord {
                start_ms: w[0].as_f64()?,
                end_ms: w[0].as_f64()?,
                word: w[1].as_str()?.to_string(),
            })
        }).collect();
    Some(words)
}
fn gen_chart(args: &Args, media: &Path, artist: &str, title: &str,
             asr_words: &[asr::AsrWord], dest: &Path,
             rec: Option<&lyrics::LyricRecord>) -> bool {
    if asr_words.len() < 20 {
        eprintln!("chart: too few asr words ({}) — need an imported \
                   chart or better audio", asr_words.len());
        return false;
    }
    let asr: Vec<(String, f64)> = wordmap::norm_asr(
        &asr_words.iter()
            .map(|w| (w.word.clone(), w.start_ms, w.end_ms))
            .collect::<Vec<_>>());
    // RMVPE curve on the 16k vocal stem (or raw 16k mono)
    let curve = if rmvpe::have_model(&args.models) {
        stage("extracting melody (rmvpe)");
        let audio16 = {
            let voc = media.with_extension("vocals.wav");
            if voc.exists() {
                ff::read_wav_f32(&voc).map(|(s, _, _)| s).ok()
            } else {
                ff::decode_f32(media, 16000, 1).ok()
            }
        };
        audio16.and_then(|a| {
            rmvpe::extract_curve(&a, &args.models)
                .map_err(|e| eprintln!("rmvpe failed: {e}")).ok()
                .inspect(|c| {
                    let voiced = c.f0.iter().zip(&c.conf)
                        .filter(|(f, cf)| **f > 0.0 && **cf >= 0.3)
                        .count();
                    println!("rmvpe: {} frames, {} voiced",
                             c.times.len(), voiced);
                })
        })
    } else {
        eprintln!("rmvpe.onnx not under {} — chart words get no pitch \
                   (all freestyle)", args.models.display());
        None
    };
    let (synced, plain): (Vec<(i64, String)>, Vec<String>) =
        match rec {
            Some(r) => (r.synced.clone(), r.plain.clone()),
            None => (Vec::new(), Vec::new()),
        };
    let mut meta = genchart::GenMeta {
        lyrics_kind: None,
        lrc_shift_ms: None,
        lrc_onset_only: false,
        lines: 0, interp_lines: 0, asr_lines: 0,
        words: 0, asr_words: 0, matched: 0, total: 0,
    };
    stage("generating chart");
    match genchart::generate(artist, title, &synced, &plain, &asr,
                             curve.as_ref(), &mut meta) {
        Some(chart) => {
            if let Err(e) = ups_core::chart::save_native(&chart, dest) {
                eprintln!("chart write failed: {e}");
                return false;
            }
            let n: usize =
                chart.tracks.values().map(|v| v.len()).sum();
            println!("chart: generated {n} notes over {} lines \
                      ({:?} lyrics, {} asr words{})",
                     meta.lines, meta.lyrics_kind, meta.asr_words,
                     meta.lrc_shift_ms
                         .map(|s| format!(", lrc shift {s:.0}ms"))
                         .unwrap_or_default());
            true
        }
        None => {
            eprintln!("chart: generation produced nothing usable \
                       — try --chart with an imported file");
            false
        }
    }
}
