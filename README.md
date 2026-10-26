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
pitch detection math, manifest + folder library discovery, audit.

Not yet: audio/video playback, mic capture, web UI, karaoke rendering.
Next milestones, roughly:

1. `ups-play` — playback via ffmpeg/gstreamer + cpal mic + live `Scorer`
2. `ups-app` — axum server reusing the existing `web/` frontend assets
   (the Pi's HTML/JS UI is transport-agnostic over websockets)
3. Cross-compile to `aarch64-unknown-linux-gnu` for the Pi itself

License: GPL-3.0 (matching the source project and the WorldParty code
it ports).
