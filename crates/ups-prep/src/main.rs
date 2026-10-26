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
mod ff;
mod lyrics;
mod separate;

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
    };
    while let Some(arg) = a.next() {
        match arg.as_str() {
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
                     \x20 --library DIR    stick root (default .)\n\
                     \x20 --models DIR     model dir (default ./models or \
                     $UPS_MODELS_DIR)\n\
                     \x20 --chart FILE     import a .txt/.upsong chart\n\
                     \x20 --artist NAME    override artist\n\
                     \x20 --title NAME     override title\n\
                     \x20 --guide F        guide-vocal level 0..1 \
                     (default 0.15)\n\
                     \x20 --no-separate    skip vocal separation"
                );
                std::process::exit(0);
            }
            s if s.starts_with("--") => bail!("unknown flag {s}"),
            s => args.source = s.to_string(),
        }
    }
    if args.source.is_empty() {
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
    }

    // lyrics + artwork
    stage("fetching lyrics");
    let dur = ff::duration(&media).ok();
    let rec = lyrics::yt_candidates(&artist, &title)
        .into_iter()
        .find_map(|(a, t)| lyrics::fetch(&a, &t, dur))
        .or_else(|| {
            let q = format!("{artist} {title}").trim().to_string();
            lyrics::search(&q, dur).into_iter().next()
        });
    if let Some(r) = &rec {
        println!("lyrics: {} synced / {} plain lines \
                  ({} — {})",
                 r.synced.len(), r.plain.len(),
                 r.artist_name, r.track_name);
    } else {
        println!("lyrics: none found");
    }

    stage("fetching artwork");
    let art = stem_path("jpg");
    if !art.exists() {
        match artwork::save(&artist, &title, &art,
                            info.thumb_url.as_deref()) {
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
    });
    std::fs::write(stem_path("analysis.json"),
                   serde_json::to_string_pretty(&ana)?)?;

    let _ = std::fs::remove_dir_all(&dl_dir);
    println!("done → {}", folder.display());
    Ok(())
}
