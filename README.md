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

Not yet: pipeline ingestion (YouTube download / normalize / vocal cut /
chart generation — still Python-side), duet per-mic routing, autosync
(station charts are GAP-corrected so plain GAP works).

## Platforms

The whole workspace is portable — no unix-only APIs; every external
tool degrades gracefully when absent.

| platform | build | notes |
|---|---|---|
| Raspberry Pi (labwc/Wayland) | `cargo build --release -p ups-app` | fullscreen via labwc windowRule (`app_id=ultra-pi-star`); PipeWire mic monitor active; `/sys/class/drm` picks native panel res |
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
