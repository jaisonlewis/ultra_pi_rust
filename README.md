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
and `ups-play`, a terminal karaoke session: symphonia-decoded audio
playback (mp4/aac/mp3/…), cpal mic capture, live pitch scoring and
lyric lines.

Not yet: video window, web UI, karaoke pitch-lane rendering,
autosync (the Python `sing.py` pitch/warp/whisper machinery — station
charts are GAP-corrected already so plain GAP timing works).

Next milestones, roughly:

1. `ups-app` — axum server reusing the existing `web/` frontend assets
   (the Pi's HTML/JS UI is transport-agnostic over websockets), with
   video + pitch lanes
2. Cross-compile to `aarch64-unknown-linux-gnu` for the Pi itself
   (rodio/cpal are pure Rust + ALSA — no C toolchain gymnastics)

License: GPL-3.0 (matching the source project and the WorldParty code
it ports).
