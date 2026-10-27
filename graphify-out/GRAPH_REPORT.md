# Graph Report - ultra-pi-star-rust  (2026-10-27)

## Corpus Check
- 48 files · ~93,981 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 592 nodes · 1197 edges · 23 communities detected
- Extraction: 85% EXTRACTED · 15% INFERRED · 0% AMBIGUOUS · INFERRED: 183 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `8c718756`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- [[_COMMUNITY_Community 0|Community 0]]
- [[_COMMUNITY_Community 1|Community 1]]
- [[_COMMUNITY_Community 2|Community 2]]
- [[_COMMUNITY_Community 3|Community 3]]
- [[_COMMUNITY_Community 4|Community 4]]
- [[_COMMUNITY_Community 5|Community 5]]
- [[_COMMUNITY_Community 6|Community 6]]
- [[_COMMUNITY_Community 7|Community 7]]
- [[_COMMUNITY_Community 8|Community 8]]
- [[_COMMUNITY_Community 9|Community 9]]
- [[_COMMUNITY_Community 10|Community 10]]
- [[_COMMUNITY_Community 11|Community 11]]
- [[_COMMUNITY_Community 12|Community 12]]
- [[_COMMUNITY_Community 13|Community 13]]
- [[_COMMUNITY_Community 14|Community 14]]
- [[_COMMUNITY_Community 15|Community 15]]
- [[_COMMUNITY_Community 16|Community 16]]
- [[_COMMUNITY_Community 17|Community 17]]
- [[_COMMUNITY_Community 18|Community 18]]
- [[_COMMUNITY_Community 19|Community 19]]
- [[_COMMUNITY_Community 20|Community 20]]
- [[_COMMUNITY_Community 21|Community 21]]
- [[_COMMUNITY_Community 23|Community 23]]

## God Nodes (most connected - your core abstractions)
1. `ensure_sid()` - 31 edges
2. `ok()` - 29 edges
3. `main()` - 25 edges
4. `prep()` - 24 edges
5. `$()` - 22 edges
6. `load_chart()` - 22 edges
7. `play_item()` - 22 edges
8. `is_host()` - 21 edges
9. `State` - 18 edges
10. `generate()` - 17 edges

## Surprising Connections (you probably didn't know these)
- `shade()` --calls--> `c()`  [INFERRED]
  crates/ups-app/web/static/js/tv.js → crates/ups-app/web/static/vendor/socket.io.min.js
- `main()` --calls--> `available()`  [INFERRED]
  crates/ups-tv/src/main.rs → crates/ups-app/src/audio.rs
- `from_native()` --calls--> `ok()`  [INFERRED]
  crates/ups-core/src/chart.rs → crates/ups-app/src/audio.rs
- `align_chart()` --calls--> `ok()`  [INFERRED]
  crates/ups-prep/src/align.rs → crates/ups-app/src/audio.rs
- `save()` --calls--> `ok()`  [INFERRED]
  crates/ups-prep/src/artwork.rs → crates/ups-app/src/audio.rs

## Communities (28 total, 6 thin omitted)

### Community 0 - "Community 0"
Cohesion: 0.06
Nodes (54): align_chart(), AlignResult, bake(), chromagram(), corr(), match_at(), template(), transcribe_media() (+46 more)

### Community 1 - "Community 1"
Cohesion: 0.08
Nodes (47): api_audio_output(), api_catalog_rescan(), api_chart_verify(), api_host_login(), api_host_release(), api_join(), api_manage_entry_delete(), api_manage_entry_repair() (+39 more)

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (40): $(), api(), badges(), singerChip(), toast(), loadAudio(), loadPendingCharts(), render() (+32 more)

### Community 3 - "Community 3"
Cohesion: 0.07
Nodes (26): Chart, from_native(), lane_payload(), load_chart(), native_path(), NativeChart, NativeNote, Note (+18 more)

### Community 4 - "Community 4"
Cohesion: 0.1
Nodes (35): file_name(), find_art(), find_chart(), find_charts_rec(), is_inst(), is_junk(), is_source_media(), list_dir() (+27 more)

### Community 5 - "Community 5"
Cohesion: 0.13
Nodes (25): draw_host_badge(), draw_idle(), Driver, Fx, native_size(), now_secs(), play_item(), PlayItem (+17 more)

### Community 6 - "Community 6"
Cohesion: 0.11
Nodes (25): asr_lines(), estimate_key(), generate(), GenMeta, interp_spans(), line_end(), line_word_spans(), lrc_shift() (+17 more)

### Community 7 - "Community 7"
Cohesion: 0.13
Nodes (26): a(), ae(), c(), Ce(), d(), e(), Ee(), f() (+18 more)

### Community 8 - "Community 8"
Cohesion: 0.1
Nodes (13): chart_mtime(), Entry, entry_of(), HistoryRow, now_secs(), Performer, Persisted, QueueItem (+5 more)

### Community 9 - "Community 9"
Cohesion: 0.13
Nodes (22): cyr(), is_cyr(), is_hangul(), is_hira(), is_kanji(), is_kata(), kana_to_hepburn(), kana_val() (+14 more)

### Community 10 - "Community 10"
Cohesion: 0.18
Nodes (23): asr_bin(), AsrWord, have_models(), model_dir(), transcribe(), alsa_card_for(), available(), enable_card() (+15 more)

### Community 11 - "Community 11"
Cohesion: 0.22
Nodes (7): bold_font(), find_font(), find_font_bold(), font_blobs(), font_dirs(), primary_font(), Text

### Community 12 - "Community 12"
Cohesion: 0.22
Nodes (7): FinishSummary, LineEvent, PlayerNote, PlayerScore, score_factor(), Scorer, TickEvent

### Community 13 - "Community 13"
Cohesion: 0.22
Nodes (3): PlayerTick, Session, TickResult

### Community 14 - "Community 14"
Cohesion: 0.24
Nodes (9): add(), Bands, disc(), draw(), fft(), hsl(), line(), trails() (+1 more)

### Community 15 - "Community 15"
Cohesion: 0.24
Nodes (4): freq_to_tone_abs(), PitchDetector, PitchResult, tone_freq()

### Community 17 - "Community 17"
Cohesion: 0.5
Nodes (8): clean(), deezer_art(), get_json(), grab_frame(), itunes_art(), lookup(), save(), urlenc()

## Knowledge Gaps
- **38 isolated node(s):** `SingCtx`, `Rescan`, `Performer`, `QueueItem`, `HistoryRow` (+33 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **6 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `f()` connect `Community 7` to `Community 11`?**
  _High betweenness centrality (0.224) - this node is a cross-community bridge._
- **Why does `load_chart()` connect `Community 3` to `Community 0`, `Community 1`, `Community 4`, `Community 5`, `Community 8`?**
  _High betweenness centrality (0.211) - this node is a cross-community bridge._
- **Are the 23 inferred relationships involving `ok()` (e.g. with `from_native()` and `.open()`) actually correct?**
  _`ok()` has 23 INFERRED edges - model-reasoned connections that need verification._
- **Are the 9 inferred relationships involving `main()` (e.g. with `available()` and `select_output()`) actually correct?**
  _`main()` has 9 INFERRED edges - model-reasoned connections that need verification._
- **Are the 19 inferred relationships involving `prep()` (e.g. with `.start()` and `song_folder()`) actually correct?**
  _`prep()` has 19 INFERRED edges - model-reasoned connections that need verification._
- **Are the 20 inferred relationships involving `$()` (e.g. with `render()` and `loadAudio()`) actually correct?**
  _`$()` has 20 INFERRED edges - model-reasoned connections that need verification._
- **What connects `SingCtx`, `Rescan`, `Performer` to the rest of the system?**
  _38 weakly-connected nodes found - possible documentation gaps or missing edges._