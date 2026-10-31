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
mod ctcalign;
mod ff;
mod genchart;
#[cfg(feature = "gui")]
mod gui;
mod lyrics;
mod mel;
mod models;
mod onset;
mod rmvpe;
mod separate;
mod wordmap;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// GUI log tee — when the window is up, every progress line is also
/// forwarded to its log pane. CLI behavior is unchanged otherwise.
static LOG_TX: std::sync::Mutex<
    Option<std::sync::mpsc::Sender<String>>,
> = std::sync::Mutex::new(None);

pub fn set_log_sink(tx: Option<std::sync::mpsc::Sender<String>>) {
    *LOG_TX.lock().unwrap() = tx;
}

macro_rules! say {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        println!("{msg}");
        if let Some(tx) = crate::LOG_TX.lock().unwrap().as_ref() {
            let _ = tx.send(msg);
        }
    }};
}
pub(crate) use say;

struct Args {
    source: String,
    library: PathBuf,
    models: PathBuf,
    chart: Option<PathBuf>,
    artist: Option<String>,
    title: Option<String>,
    guide: f32,
    no_separate: bool,
    /// encode .inst.mp4 as HEVC — the Pi 5 decodes it in hardware
    hevc: bool,
    /// yt-dlp cookies: a browser name (firefox/chrome/edge) or a
    /// cookies.txt path — signed-in sessions dodge the bot-check
    cookies: Option<String>,
    /// `batch` mode — regenerate charts across a whole library.
    batch: bool,
    /// parallel batch: process shard `shard` of `shards` (dir-name
    /// hash partition) so N workers can split one library.
    shard: u32,
    shards: u32,
    /// `models` mode — download the model/binary pack into --models.
    dl_models: bool,
    /// `align-diag <song-dir>` — forced-align fetched lyrics against
    /// the vocal stem and print per-word timings (pipeline dev tool).
    align_diag: bool,
    /// open the native window instead of running the CLI pipeline
    gui: bool,
    /// rechart even charts the ctc aligner already produced
    force: bool,
}

impl Args {
    /// Same options against a different source — playlist members and
    /// GUI queue lines all share the library/models/hevc settings.
    fn for_source(&self, source: String) -> Args {
        Args {
            source,
            library: self.library.clone(),
            models: self.models.clone(),
            chart: self.chart.clone(),
            artist: self.artist.clone(),
            title: self.title.clone(),
            guide: self.guide,
            no_separate: self.no_separate,
            hevc: self.hevc,
            cookies: self.cookies.clone(),
            batch: self.batch,
            shard: self.shard,
            shards: self.shards,
            dl_models: self.dl_models,
            align_diag: self.align_diag,
            gui: self.gui,
            force: self.force,
        }
    }
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
        hevc: false,
        cookies: std::env::var("UPS_YTDLP_COOKIES").ok()
            .filter(|s| !s.is_empty()),
        batch: false,
        shard: 0,
        shards: 1,
        dl_models: false,
        align_diag: false,
        gui: false,
        force: false,
    };
    while let Some(arg) = a.next() {
        match arg.as_str() {
            "batch" => args.batch = true,
            "models" => args.dl_models = true,
            "align-diag" => args.align_diag = true,
            "--gui" => args.gui = true,
            "--library" => args.library = a.next().context("--library")?.into(),
            "--models" => args.models = a.next().context("--models")?.into(),
            "--chart" => args.chart = Some(a.next().context("--chart")?.into()),
            "--artist" => args.artist = Some(a.next().context("--artist")?),
            "--title" => args.title = Some(a.next().context("--title")?),
            "--guide" => args.guide = a.next().context("--guide")?.parse()?,
            "--no-separate" => args.no_separate = true,
            "--hevc" => args.hevc = true,
            "--cookies" => args.cookies =
                Some(a.next().context("--cookies")?),
            "--shard" => args.shard =
                a.next().context("--shard")?.parse()?,
            "--shards" => args.shards =
                a.next().context("--shards")?.parse()?,
            "--force" => args.force = true,
            "-h" | "--help" => {
                println!(
                    "ups-prep <youtube-url | playlist-url | media-file> \
                     [opts]\n\
                     ups-prep batch --library DIR   regenerate charts\n\
                     ups-prep models [--models DIR]  download the model \
                     + binary pack\n\
                     \x20 --library DIR    stick root (default .)\n\
                     \x20 --models DIR     model dir (default ./models or \
                     $UPS_MODELS_DIR)\n\
                     \x20 --chart FILE     import a .txt/.upsong chart\n\
                     \x20 --artist NAME    override artist\n\
                     \x20 --title NAME     override title\n\
                     \x20 --guide F        guide-vocal level 0..1 \
                     (default 0.15)\n\
                     \x20 --no-separate    skip vocal separation\n\
                     \x20 --hevc           encode .inst.mp4 as HEVC —\n\
                     \x20                  hw-decoded on the Pi 5\n\
                     \x20 --cookies SRC    yt-dlp cookies: a browser\n\
                     \x20                  name (firefox) or cookies.txt\n\
                     \x20                  (env UPS_YTDLP_COOKIES)\n\
                     \x20 --gui            open the prep window\n\n\
                     blocked downloads retry with backoff\n\
                     (UPS_YTDLP_RETRIES, default 4); playlist items are\n\
                     paced UPS_YTDLP_DELAY seconds apart (default 4).\n\n\
                     a playlist url expands to every video in it;\n\
                     artist/title/chart overrides only apply when a\n\
                     single song is being prepared.\n\n\
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
    if !args.batch && !args.gui && !args.dl_models
        && args.source.is_empty()
    {
        bail!("need a youtube url or media file (or --gui)");
    }
    Ok(args)
}

fn stage(label: &str) {
    say!("==> {label}");
}

/// Run yt-dlp with the args' cookie source and retry transient
/// YouTube blocks ("Sign in to confirm…", HTTP 429) with backoff —
/// YouTube lifts the check on its own clock, not ours.
fn ytdlp(args: &Args, extra: &[String], url: &str)
        -> Result<std::process::Output> {
    let mut argv: Vec<String> = vec![
        "--extractor-args".into(),
        "youtube:player_client=android".into(),
    ];
    if let Some(c) = &args.cookies {
        if c.ends_with(".txt") || c.contains(['/', '\\']) {
            argv.extend(["--cookies".into(), c.clone()]);
        } else {
            argv.extend(["--cookies-from-browser".into(), c.clone()]);
        }
    }
    argv.extend(extra.iter().cloned());
    let retries: u32 = std::env::var("UPS_YTDLP_RETRIES").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(4);
    let mut attempt = 0u32;
    loop {
        let out = Command::new("yt-dlp")
            .args(&argv).arg(url)
            .output()
            .context("spawn yt-dlp")?;
        let err = String::from_utf8_lossy(&out.stderr);
        let blocked = err.contains("Sign in to confirm")
            || err.contains("429")
            || err.contains("Too Many Requests");
        if out.status.success() || !blocked || attempt == retries {
            return Ok(out);
        }
        attempt += 1;
        let wait = 30u64 << attempt; // 60,120,240,480s
        say!("yt-dlp blocked ({}) — retry {}/{} in {}s",
            err.lines().last().unwrap_or("?").trim(),
            attempt, retries, wait);
        std::thread::sleep(std::time::Duration::from_secs(wait));
    }
}

/// When `src` is a playlist URL, return the member video urls;
/// otherwise a one-element vec holding `src` itself (single video or
/// local file). Flat metadata only — no downloads.
fn expand_source(args: &Args, src: &str) -> Result<Vec<String>> {
    if !src.contains("://") && !src.starts_with("www.") {
        return Ok(vec![src.to_string()]);
    }
    let out = ytdlp(args, &[
        "--flat-playlist".into(), "--no-warnings".into(), "-J".into(),
    ], src)?;
    if !out.status.success() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&out.stderr)
            .lines().last().unwrap_or("failed"));
    }
    let j: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("yt-dlp -J")?;
    let Some(entries) = j["entries"].as_array() else {
        return Ok(vec![src.to_string()]);
    };
    let mut urls = Vec::new();
    for e in entries {
        let u = e["webpage_url"].as_str()
            .or(e["url"].as_str())
            .map(str::to_string)
            .or_else(|| e["id"].as_str()
                .map(|id| format!(
                    "https://www.youtube.com/watch?v={id}")));
        if let Some(u) = u {
            urls.push(if u.contains("://") { u } else {
                format!("https://www.youtube.com/watch?v={u}")
            });
        }
    }
    urls.dedup();
    if urls.is_empty() { Ok(vec![src.to_string()]) } else { Ok(urls) }
}

/// YouTube search → (watch-url, "title — channel") pairs via yt-dlp
/// `ytsearch`. Flat metadata only — the GUI lists them for picking.
#[allow(dead_code)] // GUI-only caller (feature-gated)
fn yt_search(args: &Args, query: &str, n: usize)
        -> Result<Vec<(String, String)>> {
    let spec = format!("ytsearch{n}:{query}");
    let out = ytdlp(args, &[
        "--flat-playlist".into(), "--no-warnings".into(), "-J".into(),
    ], &spec)?;
    if !out.status.success() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&out.stderr)
            .lines().last().unwrap_or("failed"));
    }
    let j: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("yt-dlp -J")?;
    let mut hits = Vec::new();
    for e in j["entries"].as_array().into_iter().flatten() {
        let Some(id) = e["id"].as_str() else { continue };
        let title = e["title"].as_str().unwrap_or(id);
        let chan = e["channel"].as_str()
            .or(e["uploader"].as_str()).unwrap_or("");
        let label = if chan.is_empty() { title.to_string() }
            else { format!("{title} — {chan}") };
        hits.push((format!("https://www.youtube.com/watch?v={id}"),
                   label));
    }
    Ok(hits)
}

/// yt-ids already prepared under `library` — search hits matching one
/// are shown "in library" so they aren't re-prepped.
#[allow(dead_code)] // GUI-only caller (feature-gated)
fn library_video_ids(
        library: &Path) -> std::collections::HashSet<String> {
    ups_core::catalog::scan(library).iter()
        .filter_map(|s| s.media.file_stem()
            .and_then(|st| st.to_str())
            .and_then(|st| st.strip_prefix("yt-"))
            .map(str::to_string))
        .collect()
}

/// Run the one-song pipeline over `urls`, expanding any playlist URL
/// through yt-dlp. Artist/title/chart overrides only make sense for a
/// single song — they're dropped on multi-entry sources.
fn prep_many(args: &Args, urls: &[String]) -> Result<()> {
    let mut expanded: Vec<String> = Vec::new();
    for u in urls {
        match expand_source(args, u) {
            Ok(m) => expanded.extend(m),
            Err(e) => say!("expand {u}: {e:#}"),
        }
    }
    expanded.dedup();
    if expanded.is_empty() {
        bail!("nothing to prepare");
    }
    let single = expanded.len() == 1;
    let total = expanded.len();
    if total > 1 {
        say!("playlist/queue: {total} videos");
    }
    let (mut ok, mut failed) = (0u32, 0u32);
    for (i, u) in expanded.iter().enumerate() {
        if total > 1 {
            say!("==> [{}/{}] {}", i + 1, total, u);
        }
        let mut one = args.for_source(u.clone());
        if !single {
            one.artist = None;
            one.title = None;
            one.chart = None;
        }
        match prep(&one) {
            Ok(()) => ok += 1,
            Err(e) => {
                say!("  failed: {e:#}");
                failed += 1;
            }
        }
        // pace requests — hammering YouTube is what trips the
        // bot-check in the first place
        if i + 1 < total {
            let delay: u64 = std::env::var("UPS_YTDLP_DELAY").ok()
                .and_then(|v| v.parse().ok()).unwrap_or(4);
            if delay > 0 {
                std::thread::sleep(
                    std::time::Duration::from_secs(delay));
            }
        }
    }
    if total > 1 {
        say!("queue done: {ok} prepared, {failed} failed");
    }
    Ok(())
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
    let out = ytdlp(args, &[
        "-f".into(),
        "best[ext=mp4]/bestvideo[ext=mp4]+bestaudio/best".into(),
        "--print-json".into(), "-o".into(),
        out_tmpl.to_string_lossy().into_owned(),
    ], src)?;
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
    // bare `ups-prep` (double-click) opens the window when built with
    // the gui feature; without it the CLI usage error stands
    if std::env::args().len() == 1 {
        #[cfg(feature = "gui")]
        return gui::run();
        #[cfg(not(feature = "gui"))]
        {
            eprintln!("ups-prep: no arguments — see --help");
            std::process::exit(2);
        }
    }
    let args = parse_args()?;
    #[cfg(feature = "gui")]
    if args.gui {
        return gui::run();
    }
    #[cfg(not(feature = "gui"))]
    if args.gui {
        bail!("--gui needs a build with --features gui");
    }
    if args.batch {
        return run_batch(&args);
    }
    if args.dl_models {
        return models::download(&args.models, None);
    }
    if args.align_diag {
        return run_align_diag(&args);
    }
    prep_many(&args, &[args.source.clone()])
}

/// `ups-prep align-diag <song-dir> [--artist A --title T]` — fetch the
/// best lyric record for the song, forced-align it to the vocal stem
/// and dump per-word timings/confidence. The dev loop for the aligner.
fn run_align_diag(args: &Args) -> Result<()> {
    let dir = PathBuf::from(&args.source);
    let songs = ups_core::catalog::scan(&dir);
    let song = songs.first().context("no song under that dir")?;
    // a bare song dir carries no artist/title — the chart header does
    let hdr = song.chart.as_ref().map(|c| ups_core::chart::load_chart(c));
    let hget = |k: &str| hdr.as_ref()
        .and_then(|c| c.headers.get(k).cloned())
        .filter(|v| !v.is_empty());
    let artist = args.artist.clone()
        .or_else(|| Some(song.artist.clone()).filter(|a| !a.is_empty()))
        .or_else(|| hget("ARTIST")).unwrap_or_default();
    let title = args.title.clone()
        .or_else(|| Some(song.title.clone())
                 .filter(|t| !t.is_empty() && !t.starts_with("yt-")))
        .or_else(|| hget("TITLE")).unwrap_or_default();
    let stem = song.media.file_stem().unwrap_or_default()
        .to_string_lossy().into_owned();
    let voc = song.dir.join(format!("{stem}.vocals.wav"));
    let src = if voc.exists() && std::env::var("UPS_ALIGN_MIX").is_err() {
        voc.clone()
    } else {
        song.media.clone()
    };
    say!("audio: {}", src.display());
    let audio = ff::decode_f32(&src, ctcalign::SR as u32, 1)?;
    let dur = Some(audio.len() as f64 / ctcalign::SR as f64);

    // UPS_ALIGN_BACKEND=omni|parakeet overrides the language pick
    let backend = match std::env::var("UPS_ALIGN_BACKEND").as_deref() {
        Ok("omni") => ctcalign::Backend::Omnilingual,
        Ok("parakeet") => ctcalign::Backend::ParakeetCtc,
        _ => ctcalign::backend_for(&args.models, "")
            .context("no aligner model installed")?,
    };
    let mut al = ctcalign::Aligner::load(&args.models, backend)?;
    let t = std::time::Instant::now();
    let lp = al.log_probs(&audio)?;
    say!("ctc[{:?}]: {} frames @{}ms ({:.1}s audio) in {:.1}s",
         backend, lp.frames, al.frame_ms,
         audio.len() as f64 / ctcalign::SR as f64,
         t.elapsed().as_secs_f64());

    say!("song: {artist} — {title}");
    let recs = collect_lyrics(&artist, &title, dur);
    say!("lyrics: {} candidate records", recs.len());
    let mut best: Option<(f32, usize, Vec<ctcalign::AlignedWord>, &lyrics::LyricRecord)> = None;
    for r in &recs {
        let lines: Vec<String> = if r.synced.is_empty() {
            r.plain.clone()
        } else {
            r.synced.iter().map(|(_, t)| t.clone()).collect()
        };
        if lines.is_empty() { continue; }
        let t = std::time::Instant::now();
        let w = al.align(&lp, &lines);
        let n_words: usize = lines.iter().map(|l| l.split_whitespace()
            .filter(|w| w.chars().any(|c| c.is_alphanumeric())).count()).sum();
        let heard = w.iter().filter(|x| x.conf >= ctcalign::MIN_WORD_CONF).count();
        let mean_conf = if w.is_empty() { 0.0 } else {
            w.iter().map(|x| x.conf).sum::<f32>() / w.len() as f32 };
        let score = heard as f32 / n_words.max(1) as f32;
        say!("  [{}] {} — {}: {} lines/{} words, heard {} ({:.0}%), mean conf {:.2}, dur {:?} ({:.1}s)",
             r.source, r.artist_name, r.track_name, lines.len(), n_words,
             heard, score * 100.0, mean_conf, r.duration.map(|d| d as i64),
             t.elapsed().as_secs_f64());
        if best.as_ref().is_none_or(|b| score > b.0) {
            best = Some((score, n_words, w, r));
        }
    }
    let Some((score, n_words, words, rec)) = best else {
        bail!("no lyric record aligned");
    };
    say!("=== best: {} ({:.0}% of {} words heard) ===", rec.source,
         score * 100.0, n_words);
    let mut cur_line = usize::MAX;
    for w in &words {
        if w.line != cur_line {
            cur_line = w.line;
            println!();
            print!("[{:>6.1}s] ", w.start_ms / 1000.0);
        }
        let mark = if w.conf < ctcalign::MIN_WORD_CONF { "?" } else { "" };
        print!("{}{}({:.0}) ", mark, w.text, w.conf * 100.0);
    }
    println!();
    Ok(())
}

/// The one-song pipeline: fetch → normalize → separate → karaoke →
/// ASR → lyrics → chart → artwork → sidecar.
fn prep(args: &Args) -> Result<()> {
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
    say!("song: {artist} — {title}");

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
        say!("loudnorm skipped: {e}");
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
                    ff::karaoke_mp4(&media, &mix, &inst, args.hevc)?;
                    stage("writing vocal stem");
                    let mono = separate::mono16(&vocals)?;
                    ff::write_wav(&voc, &mono, 16000, 1)?;
                    stage("normalizing karaoke track");
                    let _ = ff::normalize(&inst);
                }
                Err(e) => say!("separation failed: {e}"),
            }
        }
    } else if !args.no_separate {
        say!("separation models/binary not found under {} — \
                   skipping vocal cut", args.models.display());
    }

    // ASR — word onsets for chart generation + lyric anchoring.
    // Vocal stem first; full media when the stem won't transcribe.
    let mut asr_words: Vec<asr::AsrWord> = Vec::new();
    let mut asr_lang: Option<String> = None;
    let mut asr_engine = "none";
    if asr::have_models(&args.models) {
        stage("transcribing (asr)");
        let voc = stem_path("vocals.wav");
        let stem = voc.exists().then_some(voc.as_path());
        match asr::transcribe_media(&media, stem, &args.models) {
            Ok(o) => {
                say!("asr: {} words ({}, lang {})",
                         o.words.len(), o.engine,
                         o.lang.as_deref().unwrap_or("?"));
                asr_lang = o.lang;
                asr_engine = o.engine;
                asr_words = o.words;
            }
            Err(e) => say!("asr failed: {e}"),
        }
    }

    // lyrics — provider fan-out, then the record whose text best
    // matches what the ASR actually heard wins
    stage("fetching lyrics");
    let dur = ff::duration(&media).ok();
    let recs = collect_lyrics(&artist, &title, dur);
    let asr_norm: Vec<(String, f64)> = wordmap::norm_asr(
        &asr_words.iter()
            .map(|w| (w.word.clone(), w.start_ms, w.end_ms))
            .collect::<Vec<_>>());
    let rec = pick_lyrics(&recs, &asr_norm, dur);
    if let Some(r) = &rec {
        say!("lyrics: {} synced / {} plain lines \
                  ({} — {}, {})",
                 r.synced.len(), r.plain.len(),
                 r.artist_name, r.track_name, r.source);
    } else {
        say!("lyrics: none found");
    }

    // language coverage — the alerts the user asked for: a script we
    // can't romanize, or a non-English song with no whisper model
    let lyric_text = rec.as_ref().map(|r| {
        if r.synced.is_empty() {
            r.plain.join("\n")
        } else {
            r.synced.iter().map(|(_, t)| t.as_str())
                .collect::<Vec<_>>().join("\n")
        }
    });
    if let Some(txt) = &lyric_text {
        for sc in ups_core::romanize::scripts_in(txt) {
            if !ups_core::romanize::romanizable(sc) {
                say!("⚠ {} script can't be romanized yet — \
                          rom toggle will keep native text",
                         ups_core::romanize::script_name(sc));
            }
            if !models::font_installed(&args.models, sc) {
                say!("⚠ no {} font installed — native lyrics may \
                          draw as boxes on the TV (Download models \
                          → Language packs → Fonts)",
                         ups_core::romanize::script_name(sc));
            }
        }
    }
    let foreign = asr_lang.as_deref().map_or(false, |l| l != "en")
        || rec.as_ref().and_then(|r| r.lang.as_deref())
            .map_or(false, |l| l != "english" && l != "en")
        || lyric_text.as_deref()
            .map_or(false, |t| ups_core::romanize::script_of(t)
                != "latin");
    if foreign && !asr::have_whisper(&args.models) {
        say!("⚠ non-English song but no multilingual ASR model — \
              grab a whisper pack via Download models");
    }

    // phase 2 — chart: import + chroma-align to THIS media, then bake
    // to absolute media-ms so the player needs no warp machinery
    if let Some(chart_path) = &args.chart {
        stage("importing chart");
        let mut chart = ups_core::chart::load_chart(chart_path);
        ups_core::romanize::romanize_chart(&mut chart);
        let dest = stem_path("upsong");
        if (chart.bpm() - 15000.0).abs() > 1.0 {
            stage("aligning chart to media");
            match align::align_chart(&chart, &media, None) {
                Ok(Some(res)) => {
                    let baked = align::bake(&chart, &res);
                    ups_core::chart::save_native(&baked, &dest)?;
                    say!("chart: aligned off {}ms tempo {} \
                              (matched {}/{} phrases, peak {})",
                             res.offset_ms, res.tempo,
                             res.matched, res.phrases, res.peak);
                }
                Ok(None) => {
                    say!("align: no confident match — importing \
                               unaligned (check timing)");
                    ups_core::chart::save_native(&chart, &dest)?;
                }
                Err(e) => say!("align failed: {e}"),
            }
        } else {
            ups_core::chart::save_native(&chart, &dest)?;
        }
        let n: usize = chart.tracks.values().map(|v| v.len()).sum();
        say!("chart: {n} notes → {}", dest.display());
    } else {
        gen_chart(&args, &media, &artist, &title, &asr_words,
                  &stem_path("upsong"), &recs, rec.as_ref(),
                  asr_lang.as_deref(), None);
    }

    stage("fetching artwork");
    let art = stem_path("jpg");
    if !art.exists() {
        match artwork::save(&artist, &title, &art,
                            info.thumb_url.as_deref(),
                            Some(media.as_path())) {
            Ok(true) => say!("artwork: {}", art.display()),
            _ => say!("artwork: none"),
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
            "engine": asr_engine,
            "lang": asr_lang,
            "words": asr_words.len(),
            "words_ms": asr_words.iter()
                .map(|w| serde_json::json!([w.start_ms, w.word]))
                .collect::<Vec<_>>(),
        },
    });
    std::fs::write(stem_path("analysis.json"),
                   serde_json::to_string_pretty(&ana)?)?;

    let _ = std::fs::remove_dir_all(&dl_dir);
    say!("done → {}", folder.display());
    Ok(())
}

/// Dedupe lyric records by song identity — a later synced record
/// upgrades an earlier plain-only hit for the same song.
fn push_rec(r: Option<lyrics::LyricRecord>,
            recs: &mut Vec<lyrics::LyricRecord>) {
    let Some(r) = r else { return };
    let key = format!("{}|{}",
        r.artist_name.to_lowercase().trim(),
        r.track_name.to_lowercase().trim());
    let key_of = |e: &lyrics::LyricRecord| format!("{}|{}",
        e.artist_name.to_lowercase().trim(),
        e.track_name.to_lowercase().trim());
    if let Some(ex) = recs.iter_mut().find(|e| key_of(e) == key) {
        if ex.synced.is_empty() && !r.synced.is_empty() {
            *ex = r;
        }
        return;
    }
    recs.push(r);
}

/// Provider fan-out — LRCLIB first, then the alternates on the
/// likeliest candidates; plain-text/search fallbacks only when nothing
/// synced landed. Shared by single-song and batch paths.
fn collect_lyrics(artist: &str, title: &str, dur: Option<f64>)
                 -> Vec<lyrics::LyricRecord> {
    let mut recs: Vec<lyrics::LyricRecord> = Vec::new();
    let cands = lyrics::yt_candidates(artist, title);
    for (a, t) in &cands {
        push_rec(lyrics::fetch(a, t, dur), &mut recs);
    }
    // alternates — synclrc covers Indian languages well; lrcmux fans
    // out to deezer/musixmatch/etc itself
    for (a, t) in cands.iter().take(3) {
        push_rec(lyrics::synclrc(a, t, dur), &mut recs);
        push_rec(lyrics::lrcmux(a, t), &mut recs);
    }
    let q = format!("{artist} {title}").trim().to_string();
    for r in lyrics::search(&q, dur) {
        push_rec(Some(r), &mut recs);
    }
    if recs.iter().all(|r| r.synced.is_empty()) {
        // jiosaavn carries Indic lyrics + language tags, synclrc_search
        // covers name variants the exact lookups missed
        for r in lyrics::jiosaavn(&q, dur) {
            push_rec(Some(r), &mut recs);
        }
        if let Some((a, t)) = cands.first() {
            push_rec(lyrics::ovh(a, t), &mut recs);
        }
        for r in lyrics::synclrc_search(&q, dur) {
            push_rec(Some(r), &mut recs);
        }
    }
    recs
}

/// Keep the record whose text best matches what the ASR heard —
/// LRCLIB stubs and same-title-different-song picks lose on word
/// coverage. Foreign-language records score ~0 against a script-
/// mismatched ASR: fall back to the synced record whose provider
/// duration agrees with the media.
fn pick_lyrics(recs: &[lyrics::LyricRecord],
               asr_norm: &[(String, f64)], dur: Option<f64>)
               -> Option<lyrics::LyricRecord> {
    recs.iter()
        .map(|r| {
            let lines: Vec<String> = if r.synced.is_empty() {
                r.plain.clone()
            } else {
                r.synced.iter().map(|(_, t)| t.clone()).collect()
            };
            let sp = wordmap::map_line_words(asr_norm, &lines);
            (sp.values().map(|s| s.words.len()).sum::<usize>(), r)
        })
        .max_by_key(|(u, _)| *u)
        .filter(|(u, _)| *u >= 3)
        .map(|(_, r)| r.clone())
        .or_else(|| recs.iter()
            .filter(|r| !r.synced.is_empty())
            .filter_map(|r| {
                dur.and_then(|d| r.duration.map(|g| (g - d).abs()))
                    .map(|delta| (delta, r))
            })
            .filter(|(delta, _)| *delta <= 4.0)
            .min_by(|a, b| a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, r)| r.clone()))
}

/// `ups-prep batch --library <root>` — regenerate machine-made charts
/// with the current ASR+pitch stack. Songs whose .upsong was imported
/// (not marked ultrapibar-genchart/generated) are left alone.
fn run_batch(args: &Args) -> Result<()> {
    let mut songs = ups_core::catalog::scan(&args.library);
    if args.shards > 1 {
        // deterministic partition — same song always lands in the
        // same shard regardless of scan order
        let h = |s: &ups_core::catalog::Song| -> u32 {
            use std::hash::{Hash, Hasher};
            let mut x = std::collections::hash_map::DefaultHasher::new();
            s.dir.hash(&mut x);
            x.finish() as u32
        };
        songs.retain(|s| h(s) % args.shards == args.shard);
    }
    let total = songs.len();
    let (mut redone, mut skipped, mut failed) = (0u32, 0u32, 0u32);
    for (i, song) in songs.iter().enumerate() {
        // an imported (non-generated) chart is human-timed — keep it;
        // a chart already made by the ctc aligner is current — keep
        // it too so interrupted sweeps resume instead of redoing
        if let Some(cp) = &song.chart {
            let c = ups_core::chart::load_chart(cp);
            let gen = c.headers.get("CREATOR")
                    .is_some_and(|v| v.contains("genchart"))
                || c.source["type"].as_str() == Some("generated");
            let aligned = c.source["lyrics"].as_str()
                .is_some_and(|v| v.starts_with("ctc-"));
            if !gen || (aligned && !args.force) {
                skipped += 1;
                continue;
            }
        }
        if !song.media.is_file() {
            skipped += 1;
            continue;
        }
        say!("[{}/{}] {} — {}", i + 1, total,
                 song.artist, song.title);
        match batch_one(args, song) {
            Ok(true) => redone += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                say!("  failed: {e}");
                failed += 1;
            }
        }
    }
    say!("batch done: {redone} regenerated, {skipped} skipped, \
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
    let mut asr_lang: Option<String> = None;
    let asr_words = read_cached_asr(&ana_path)
        .filter(|(w, _)| w.len() >= 20)
        .map(|(w, l)| {
            say!("  asr: {} cached words", w.len());
            asr_lang = l;
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
            let o = asr::transcribe_media(&song.media, stem_ref,
                                          &args.models)?;
            asr_lang = o.lang.clone();
            let w = o.words;
            say!("  asr: {} words ({})", w.len(), o.engine);
            if w.len() < 20 {
                return Ok(false);
            }
            // refresh the sidecar so later batch runs reuse these
            if let Ok(bytes) = std::fs::read(&ana_path) {
                if let Ok(mut j) =
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                {
                    j["asr"] = serde_json::json!({
                        "engine": o.engine,
                        "lang": o.lang,
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

    // lyrics — cached beside the media after the first fetch so
    // batch re-runs skip the provider fan-out entirely; an empty
    // result is never cached (providers may recover later)
    let dur = ff::duration(&song.media).ok();
    let lyr_path = song.media.with_extension("lyrics.json");
    let recs = read_cached_lyrics(&lyr_path).unwrap_or_else(|| {
        let r = collect_lyrics(&artist, &title, dur);
        if !r.is_empty() {
            if let Ok(j) = serde_json::to_string_pretty(&r) {
                let _ = std::fs::write(&lyr_path, j);
            }
        }
        r
    });
    let asr_norm: Vec<(String, f64)> = wordmap::norm_asr(
        &asr_words.iter()
            .map(|w| (w.word.clone(), w.start_ms, w.end_ms))
            .collect::<Vec<_>>());
    let rec = pick_lyrics(&recs, &asr_norm, dur);
    if let Some(r) = &rec {
        say!("  lyrics: {} synced lines ({} — {}, {})",
                 r.synced.len(), r.artist_name, r.track_name, r.source);
    }

    let meta = gen_chart(args, &song.media, &artist, &title,
                         &asr_words, &dest, &recs, rec.as_ref(),
                         asr_lang.as_deref(),
                         read_cached_pitch(&ana_path));
    if let Some(m) = &meta {
        // generation provenance for audits/ground-truth test runs
        if let Ok(bytes) = std::fs::read(&ana_path) {
            if let Ok(mut j) =
                serde_json::from_slice::<serde_json::Value>(&bytes)
            {
                j["chart"]["gen"] = serde_json::json!({
                    "lyrics_kind": m.lyrics_kind,
                    "lines": m.lines,
                    "words": m.words,
                    "aligned": m.matched,
                    "of": m.total,
                });
                let _ = std::fs::write(
                    &ana_path, serde_json::to_string_pretty(&j)
                        .unwrap_or_default());
            }
        }
    }
    Ok(meta.is_some())
}

/// Cached transcript from `<stem>.analysis.json` when it was written
/// by a known ASR engine — avoids re-transcribing on every batch run.
fn read_cached_asr(ana_path: &Path)
        -> Option<(Vec<asr::AsrWord>, Option<String>)> {
    let j: serde_json::Value =
        serde_json::from_slice(&std::fs::read(ana_path).ok()?).ok()?;
    let engine = j["asr"]["engine"].as_str().unwrap_or("");
    if !(engine.contains("parakeet") || engine.contains("whisper")) {
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
    let lang = j["asr"]["lang"].as_str().map(str::to_string);
    Some((words, lang))
}
/// Cached lyric records from `<stem>.lyrics.json`.
fn read_cached_lyrics(path: &Path)
        -> Option<Vec<lyrics::LyricRecord>> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
        .filter(|v: &Vec<lyrics::LyricRecord>| !v.is_empty())
}

/// Cached rmvpe curve from `<stem>.analysis.json` — the sidecar
/// already stores f0/conf/hop, so batch recharts skip the expensive
/// pitch pass entirely.
fn read_cached_pitch(ana_path: &Path) -> Option<rmvpe::Curve> {
    let j: serde_json::Value =
        serde_json::from_slice(&std::fs::read(ana_path).ok()?).ok()?;
    let p = &j["pitch"];
    let f0: Vec<f32> = p["f0"].as_array()?.iter()
        .filter_map(|v| v.as_f64().map(|x| x as f32)).collect();
    let conf: Vec<f32> = p["conf"].as_array()?.iter()
        .filter_map(|v| v.as_f64().map(|x| x as f32)).collect();
    if f0.is_empty() || f0.len() != conf.len() {
        return None;
    }
    let hop = p["hop_ms"].as_f64().unwrap_or(0.01);
    let times = (0..f0.len()).map(|i| i as f64 * hop).collect();
    Some(rmvpe::Curve { times, f0, conf })
}

/// Lyric lines for the aligner: synced text wins, plain as backup.
/// Section markers ([Chorus], {Verse}) and (x2) repeat tags are
/// dropped so they never enter the token lattice as fake words.
fn align_lines(r: &lyrics::LyricRecord) -> Vec<String> {
    let raw: Vec<String> = if r.synced.is_empty() {
        r.plain.clone()
    } else {
        r.synced.iter().map(|(_, t)| t.clone()).collect()
    };
    raw.into_iter().filter_map(|l| {
        let keep: Vec<&str> = l.split_whitespace().filter(|w| {
            if w.starts_with('[') || w.starts_with('{') {
                return false;
            }
            let inner = w.trim_matches(|c: char| !c.is_alphanumeric());
            let rep = inner.trim_start_matches(['x', 'X']);
            if !inner.is_empty() && !rep.is_empty()
                && rep.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            w.chars().any(|c| c.is_alphanumeric())
        }).collect();
        (!keep.is_empty()).then(|| keep.join(" "))
    }).collect()
}

/// Forced-alignment chart: run CTC log-probs once, then every lyric
/// candidate competes on the fraction of its words the model
/// actually heard. Returns true when a chart was written.
fn aligned_chart(args: &Args, audio16: &[f32], artist: &str,
                 title: &str, recs: &[lyrics::LyricRecord],
                 backend: ctcalign::Backend, curve: Option<&rmvpe::Curve>,
                 dest: &Path, meta: &mut genchart::GenMeta) -> bool {
    let mut al = match ctcalign::Aligner::load(&args.models, backend) {
        Ok(a) => a,
        Err(e) => {
            say!("align: model load failed: {e}");
            return false;
        }
    };
    let lp = match al.log_probs(audio16) {
        Ok(l) => l,
        Err(e) => {
            say!("align: inference failed: {e}");
            return false;
        }
    };
    let mut best: Option<(f32, Vec<ctcalign::AlignedWord>,
                          Vec<String>, &lyrics::LyricRecord)> = None;
    // candidates arrive roughly best-first; a >=80% heard record is
    // a decisive win — stop burning Viterbi passes on the rest
    for r in recs.iter().take(4) {
        let lines = align_lines(r);
        let n_words: usize = lines.iter()
            .map(|l| l.split_whitespace().count()).sum();
        if n_words < 15 {
            continue;
        }
        let w = al.align(&lp, &lines);
        let heard = w.iter()
            .filter(|x| x.conf >= ctcalign::MIN_WORD_CONF).count();
        let frac = heard as f32 / n_words as f32;
        say!("  align[{}]: {} lines/{} words, heard {:.0}%",
                 r.source, lines.len(), n_words, frac * 100.0);
        if best.as_ref().is_none_or(|b| frac > b.0) {
            best = Some((frac, w, lines, r));
        }
        if frac >= 0.80 {
            break;
        }
    }
    let Some((frac, words, lines, r)) = best else { return false };
    if frac < 0.35 {
        say!("align: best record only {:.0}% heard — wrong version? \
                   falling back to asr matching", frac * 100.0);
        return false;
    }
    let onsets = onset::strength(audio16);
    let source = serde_json::json!({
        "type": "generated",
        "extractor": "rmvpe",
        "lyrics": format!("ctc-{backend:?}"),
        "lrc": !r.synced.is_empty(),
        "align_heard": (frac * 100.0).round() / 100.0,
        "align_src": r.source,
    });
    meta.lyrics_kind = Some(format!("ctc-{backend:?}"));
    match genchart::generate_aligned(artist, title, &lines, &words,
                                     curve, Some(&onsets), source, meta) {
        Some(chart) => {
            if let Err(e) = ups_core::chart::save_native(&chart, dest) {
                say!("chart write failed: {e}");
                return false;
            }
            let n: usize =
                chart.tracks.values().map(|v| v.len()).sum();
            say!("chart: aligned {n} notes over {} lines \
                      ({}, {:.0}% heard)",
                     meta.lines, r.source, frac * 100.0);
            true
        }
        None => false,
    }
}

fn gen_chart(args: &Args, media: &Path, artist: &str, title: &str,
             asr_words: &[asr::AsrWord], dest: &Path,
             recs: &[lyrics::LyricRecord],
             rec: Option<&lyrics::LyricRecord>,
             lang: Option<&str>,
             cached_curve: Option<rmvpe::Curve>)
             -> Option<genchart::GenMeta> {
    let asr: Vec<(String, f64)> = wordmap::norm_asr(
        &asr_words.iter()
            .map(|w| (w.word.clone(), w.start_ms, w.end_ms))
            .collect::<Vec<_>>());
    // shared 16k mono: vocal stem first, raw decode as backup — feeds
    // rmvpe, the CTC aligner, and onset strength
    let backend = ctcalign::backend_for(&args.models,
                                        lang.unwrap_or(""));
    let want_audio = (backend.is_some() && !recs.is_empty())
        || (cached_curve.is_none() && rmvpe::have_model(&args.models));
    let audio16 = want_audio.then(|| {
        let voc = media.with_extension("vocals.wav");
        if voc.exists() {
            ff::read_wav_f32(&voc).map(|(s, _, _)| s).ok()
        } else {
            ff::decode_f32(media, 16000, 1).ok()
        }
    }).flatten();
    // RMVPE curve: batch hands us the sidecar-cached curve instead
    let curve = cached_curve.or_else(|| {
        if !rmvpe::have_model(&args.models) {
            return None;
        }
        stage("extracting melody (rmvpe)");
        audio16.as_ref().and_then(|a| {
            rmvpe::extract_curve(a, &args.models)
                .map_err(|e| say!("rmvpe failed: {e}")).ok()
                .inspect(|c| {
                    let voiced = c.f0.iter().zip(&c.conf)
                        .filter(|(f, cf)| **f > 0.0 && **cf >= 0.3)
                        .count();
                    say!("rmvpe: {} frames, {} voiced",
                             c.times.len(), voiced);
                })
        })
    }).or_else(|| {
        if rmvpe::have_model(&args.models) {
            return None; // already tried above
        }
        say!("rmvpe.onnx not under {} — chart words get no pitch \
                   (all freestyle)", args.models.display());
        None
    });
    let mut meta = genchart::GenMeta {
        lyrics_kind: None,
        lrc_shift_ms: None,
        lrc_onset_only: false,
        lines: 0, interp_lines: 0, asr_lines: 0,
        words: 0, asr_words: 0, matched: 0, total: 0,
    };
    // forced-alignment path first — word onsets come straight off
    // the acoustic model, no ASR transcript needed
    if let (Some(b), Some(a)) = (backend, audio16.as_ref()) {
        if !recs.is_empty() {
            stage("aligning lyrics (ctc)");
            if aligned_chart(args, a, artist, title, recs, b,
                             curve.as_ref(), dest, &mut meta) {
                return Some(meta);
            }
        }
    }
    // fuzzy fallback: match lyric words against the ASR transcript
    if asr.len() < 20 {
        say!("chart: too few asr words ({}) — need an imported \
                   chart or better audio", asr.len());
        return None;
    }
    let (synced, plain): (Vec<(i64, String)>, Vec<String>) =
        match rec {
            Some(r) => (r.synced.clone(), r.plain.clone()),
            None => (Vec::new(), Vec::new()),
        };
    stage("generating chart");
    match genchart::generate(artist, title, &synced, &plain, &asr,
                             curve.as_ref(), &mut meta) {
        Some(chart) => {
            if let Err(e) = ups_core::chart::save_native(&chart, dest) {
                say!("chart write failed: {e}");
                return None;
            }
            let n: usize =
                chart.tracks.values().map(|v| v.len()).sum();
            say!("chart: generated {n} notes over {} lines \
                      ({:?} lyrics, {} asr words{})",
                     meta.lines, meta.lyrics_kind, meta.asr_words,
                     meta.lrc_shift_ms
                         .map(|s| format!(", lrc shift {s:.0}ms"))
                         .unwrap_or_default());
            Some(meta)
        }
        None => {
            say!("chart: generation produced nothing usable \
                       — try --chart with an imported file");
            None
        }
    }
}
