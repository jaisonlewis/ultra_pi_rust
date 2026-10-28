# Ultra Pi Star — Rust

Rust port of the Ultra Pi Star karaoke engine. Same library format,
same charts, same scoring — a `.upsong` / `artist/title/` library
prepared by the Python prep station works here unchanged, and this
crate is the intended core for native frontends.

## Workspace

```
crates/
  ups-core/   engine library — no I/O beyond file reads, no deps but serde
  ups-cli/    command-line tool (scan / audit / info)
  ups-play/   terminal karaoke — plays the instrumental via symphonia,
              mic pitch via cpal, live scoring + lyric lines on stdout
  ups-app/    axum + socketioxide server — drop-in for the Python app,
              serves web/ verbatim; --tv embeds the native renderer
  ups-tv/     native framebuffer renderer — minifb window, ffmpeg-piped
              video, fontdue text, pitch lane + lyrics + scoring + fx,
              no GPU/DOM/browser (~160 MB on the Pi incl. server)
```

`ups-core` is a faithful port of the Python reference:

| module | ports | contents |
|---|---|---|
| `chart`   | `ultrapistar/charts.py`    | UltraStar `.txt` importer, native `.upsong` v2, line/phrase grouping |
| `scoring` | `ultrapistar/scoring.py`   | WorldParty `UNote.pas` scoring: beat map, tolerance, golden/line bonus, streaks |
| `pitch`   | `ultrapistar/pitch.py`     | `TCaptureBuffer` autocorrelation detector, C2..A5 |
| `layout`  | `ultrapistar/mediastore.py`| `artist_slug/title_slug/` naming — byte-identical to Python |
| `catalog` | station `manifest.json` + Switch folder scan | library discovery |

## CLI

```
ups scan  <library root>     list songs (manifest first, folders fallback)
ups audit <library root>     completeness: media/inst/vocals/analysis/chart/art
ups info  <file.upsong>      chart summary + perfect-run score check
```

## Status

Working today: chart loading (both formats), the full scoring engine
(verified numerically identical to the Python port on real charts),
pitch detection math, manifest + folder library discovery, audit —
`ups-play` terminal karaoke — and:

- `ups-app` — the party server: REST + socket.io contract the existing
  `web/` frontend consumes unchanged (phone remote, host controls,
  mic volume/mix, queue, search, scores), `ups-state.json` recovery,
  PipeWire mic→speaker monitor on PipeWire systems.
- `ups-tv` — the native screen: ffmpeg-decoded video + karaoke lyrics,
  pitch lane, sung-tone trail, hit sparks, rating pops, eased score,
  streak badges, results confetti, animated idle. `--tv` runs it
  inside `ups-app`; `ups-tv <root> <search>` plays one song standalone.

- `ups-prep` — the prep station: YouTube URL or local file →
  normalized media, vocal-cut `.inst.mp4`, vocal stem, artwork,
  analysis sidecar, and a `.upsong` chart — imported + chroma-aligned
  when you supply one, or *generated from scratch* (ASR word timing +
  RMVPE pitch) when you don't. Straight into a stick folder the player
  scans.

Not yet: duet per-mic routing, autosync (station charts are
GAP-corrected so plain GAP works).

## Prepping songs (ups-prep)

```
ups-prep <youtube-url | media-file> --library <stick root> [opts]
  --models DIR     model dir (default ./models, $UPS_MODELS_DIR)
  --chart FILE     import a USDB .txt or .upsong chart (auto-aligned
                   to the media via chroma cross-correlation, baked to
                   absolute media-ms)
  --artist / --title   metadata overrides
  --guide F        guide-vocal level in the karaoke mix (default .15)
  --no-separate    skip the vocal cut

ups-prep batch --library <root>   re-chart a whole library: walks
  every song, regenerates .upsongs that were machine-generated
  (or missing), and leaves imported/human charts untouched.
  A stored parakeet transcript is reused so repeats are fast.

ups-prep --gui                    native window form (song URL/file,
                                  library + models pickers, batch
                                  button, live log) — needs a build
                                  with --features gui
```

GUI build (Windows/Mac/Linux desktop — not needed on the Pi):

```
cargo build --release -p ups-prep --features gui
# then: ups-prep --gui, or double-click the exe (no args = window)
```

Produces `Library/Artist - Title/<media>` + `.inst.mp4` + `.vocals.wav`
+ `.upsong` + `.jpg` + `.analysis.json` — the same layout the Python
pipeline writes, so folders are portable between prep stations.

External tools (all cross-platform): `ffmpeg`, `ffprobe`, `yt-dlp`.
The ML stages use external sherpa-onnx binaries plus ONNX models —
no Python or ML-framework installs:

The easiest way to get them is built in — downloads + unpacks
everything, platform-aware, idempotent, resumable by re-running:

```
ups-prep models [--models ./models]   # ~900 MB total
# or the "Download models" button in the GUI
```

```
models/
  sherpa-onnx-v2.0.1-<platform>-shared*/   sherpa-onnx binaries
    bin/sherpa-onnx-offline-source-separation   vocal separation
        sherpa-onnx-offline                      ASR
  sherpa-onnx-spleeter-2stems-int8/        separation models
  sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/   ASR model
  rmvpe.onnx                               melody extraction
```

Binaries resolve from `models/bin/` (if you lay them out yourself),
any `models/sherpa-onnx-*/bin/` (the downloader's layout — keeps the
shared libs next to the exes), or PATH.

Manual setup is still fine — fetch the pieces into `models/` with
this layout and `ups-prep` will find them:

```
# sherpa-onnx binaries — grab the release for your OS
#   (win-x64 / linux-x64+aarch64 / macos-x64+arm64 all exist):
#   https://github.com/k2-fsa/sherpa-onnx/releases

# spleeter int8 separation models:
curl -LO https://github.com/k2-fsa/sherpa-onnx/releases/download/\
source-separation-models/sherpa-onnx-spleeter-2stems-int8.tar.bz2
tar xjf sherpa-onnx-spleeter-2stems-int8.tar.bz2   # into models/

# parakeet ASR model (~490 MB, 25 languages):
curl -LO https://github.com/k2-fsa/sherpa-onnx/releases/download/\
asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2
tar xjf sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2

# rmvpe pitch model — a release asset on this repo:
#   ultra_pi_rust → releases → v0.1.0 → rmvpe.onnx → models/rmvpe.onnx
```

**ASR is Parakeet TDT, not Whisper** — a deliberate choice: TDT heads
emit *native* per-token frame times (real word onsets), while sherpa's
stock Whisper ONNX exports only give segment-level times. Parakeet
runs ~4× faster than faster-whisper turbo on CPU and anchors lyrics
frame-exactly. If the vocal stem won't transcribe (int8 spleeter
artifacts), the raw media audio is tried automatically. The Python
prep station uses faster-whisper `large-v3-turbo`; both are fine —
charts are interchangeable either way.

Chart generation (`genchart.rs`, a port of `genchart.py`): synced
LRCLIB lyrics are placed by a global LRC→media shift that lands line
onsets on heard-word onsets, then snapped to real phrase starts;
unmatched lyrics interpolate between anchored neighbours; words the
pitch curve can't confirm become `freestyle` notes instead of
pitch-scored ones. Everything bakes to absolute media milliseconds
(`BPM=15000`), so generated charts need no runtime warp.

Without the models the pipeline still runs — media, artwork, analysis,
imported-chart alignment — just no `.inst.mp4`, no vocal stem, and no
generated chart.

## Platforms

The whole workspace is portable — no unix-only APIs; every external
tool degrades gracefully when absent.

| platform | build | notes |
|---|---|---|
| Raspberry Pi (labwc/Wayland) | `cargo build --release -p ups-app` | fullscreen + 720p out via `scripts/pi-display.sh` (installs the `ultra-pi-star` labwc windowRule + autostart); PipeWire mic monitor active; renders at 720p by default (`UPS_RENDER_RES=0` for native) |
| Windows | `cargo build --release -p ups-tv` | works today; `ffmpeg.exe` on PATH; windowed 1280×720 (no /sys probe) — Esc quits |
| macOS | `cargo build --release -p ups-tv` | `brew install ffmpeg`; build natively on the Mac (no practical cross-compile); fonts auto-resolve to Helvetica |

Runtime deps: `ffmpeg` on PATH (video). Optional on Linux only:
`pw-loopback`/`wpctl`/`pw-dump` for the speaker mic monitor — without
them scoring still works, you just don't hear yourself.

Standalone player (the light way to try it):

```
ups-tv <library-root> <search>   # e.g. ups-tv D:\songs "billie jean"
```

Full party server (phone join + queue + host controls):

```
ups-app --media <library-root> [--tv]   # --tv adds the native screen
```

License: GPL-3.0 (matching the source project and the WorldParty code
it ports).
