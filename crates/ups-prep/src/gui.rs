//! Optional native window for the prep pipeline — build with
//! `--features gui`, then `ups-prep --gui` or a bare double-click
//! opens it. Wraps the same `prep`/`run_batch` entry points the CLI
//! uses; progress lines arrive over the `say!` tee.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use eframe::egui;

const DONE: &str = "job://done";
const ERR: &str = "job://error ";
const HIT: &str = "hit://";

enum Job {
    Prep,
    Queue,
    Search,
    Batch,
    Models,
}

// -- persisted prefs ---------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Prefs {
    library: String,
    models: String,
    guide: f32,
    no_separate: bool,
    hevc: bool,
}

fn prefs_path() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    #[cfg(not(windows))]
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("ultra-pi-star").join("prep-gui.json")
}

fn load_prefs() -> Prefs {
    std::fs::read_to_string(prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_prefs(app: &PrepApp) {
    let p = Prefs {
        library: app.library.clone(),
        models: app.models.clone(),
        guide: app.guide,
        no_separate: app.no_separate,
        hevc: app.hevc,
    };
    if let Ok(s) = serde_json::to_string(&p) {
        let path = prefs_path();
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(path, s);
    }
}

// -- first-run setup status ---------------------------------------------------

/// One checklist row's state.
struct Step {
    ok: bool,
    detail: String,
}

/// What the pipeline actually checks for under the models dir —
/// keeps the checklist honest instead of "does the dir exist".
fn model_step(models_dir: &str) -> Step {
    let m = Path::new(models_dir);
    let mut missing: Vec<&str> = Vec::new();
    if !crate::separate::have_models(m) {
        missing.push("vocal separation");
    }
    if !crate::ctcalign::have_model(m) {
        missing.push("lyric aligner");
    }
    if !crate::rmvpe::have_model(m) {
        missing.push("pitch model");
    }
    let sherpa = crate::models::find_bin(
        m,
        if cfg!(windows) {
            "sherpa-onnx-offline.exe"
        } else {
            "sherpa-onnx-offline"
        },
    )
    .is_some();
    if !sherpa {
        missing.push("sherpa binaries");
    }
    if missing.is_empty() {
        Step { ok: true, detail: "ready".into() }
    } else {
        Step { ok: false, detail: format!("missing {}", missing.join(", ")) }
    }
}

fn tool_on_path(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn run() -> anyhow::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ultra Pi Star — song prep")
            .with_inner_size([780.0, 640.0])
            .with_min_inner_size([560.0, 460.0]),
        ..Default::default()
    };
    eframe::run_native("Ultra Pi Star — song prep", opts, Box::new(|_| {
        Ok(Box::new(PrepApp::default()))
    }))
    .map_err(|e| anyhow::anyhow!("window: {e}"))
}

struct PrepApp {
    source: String,
    queue: String,
    library: String,
    models: String,
    artist: String,
    title: String,
    chart: String,
    guide: f32,
    no_separate: bool,
    hevc: bool,
    search: String,
    hits: Vec<(String, String, bool, bool)>, // url,label,in-lib,checked
    packs: Vec<(String, bool)>,   // language pack id → selected
    busy: bool,
    status: String,
    log: std::collections::VecDeque<String>,
    rx: Option<Receiver<String>>,
    // setup checklist caches — the spawn-a-process checks are too
    // slow to run every frame
    tools: Option<(bool, bool)>,        // yt-dlp, ffmpeg on PATH
    model_status: Option<Step>,         // recomputed on dir/job change
    saved_sig: String,                  // last persisted prefs fingerprint
    setup_open: bool,                   // user manually toggled the card
}

impl Default for PrepApp {
    fn default() -> Self {
        let prefs = load_prefs();
        let models = if prefs.models.is_empty() {
            std::env::var("UPS_MODELS_DIR")
                .unwrap_or_else(|_| "models".into())
        } else {
            prefs.models
        };
        let sel = crate::models::load_packs(
            std::path::Path::new(&models));
        let mut packs: Vec<(String, bool)> = crate::models::ASR_PACKS
            .iter().map(|(id, ..)| (*id, sel.contains(*id)))
            .map(|(id, on)| (id.to_string(), on))
            .collect();
        packs.extend(crate::models::FONT_PACKS.iter()
            .map(|(s, ..)| (format!("font:{s}"),
                            sel.contains(&format!("font:{s}")))));
        PrepApp {
            source: String::new(),
            queue: String::new(),
            library: prefs.library,
            models,
            artist: String::new(),
            title: String::new(),
            chart: String::new(),
            guide: prefs.guide.max(0.0),
            no_separate: prefs.no_separate,
            hevc: prefs.hevc,
            search: String::new(),
            hits: Vec::new(),
            packs,
            busy: false,
            status: String::new(),
            log: std::collections::VecDeque::new(),
            rx: None,
            tools: None,
            model_status: None,
            saved_sig: String::new(),
            setup_open: false,
        }
    }
}

impl PrepApp {
    fn args(&self, batch: bool) -> crate::Args {
        crate::Args {
            source: self.source.trim().to_string(),
            library: PathBuf::from(self.library.trim()),
            models: if self.models.trim().is_empty() {
                PathBuf::from("models")
            } else {
                PathBuf::from(self.models.trim())
            },
            chart: (!self.chart.trim().is_empty())
                .then(|| PathBuf::from(self.chart.trim())),
            artist: (!self.artist.trim().is_empty())
                .then(|| self.artist.trim().to_string()),
            title: (!self.title.trim().is_empty())
                .then(|| self.title.trim().to_string()),
            guide: self.guide,
            no_separate: self.no_separate,
            hevc: self.hevc,
            cookies: std::env::var("UPS_YTDLP_COOKIES").ok()
                .filter(|s| !s.is_empty()),
            dl_models: false,
            batch,
            shard: 0,
            shards: 1,
            align_diag: false,
            gui: false,
            force: false,
        }
    }

    fn start(&mut self, job: Job) {
        let (tx, rx) = channel();
        crate::set_log_sink(Some(tx.clone()));
        self.rx = Some(rx);
        self.busy = true;
        self.status = "working…".into();
        // missing models don't hard-fail (prep degrades gracefully),
        // but a first-timer should know *why* output is limited
        if matches!(job, Job::Prep | Job::Queue | Job::Batch) {
            let s = self.model_step();
            if !s.ok {
                self.log.push_back(format!(
                    "note: {} — output will be limited; 'Download \
                     models' fetches the full pack", s.detail));
            }
        }
        let args = self.args(matches!(job, Job::Batch));
        let queue_urls: Vec<String> = self.queue.lines()
            .flat_map(|l| l.split_whitespace())
            .map(str::to_string)
            .collect();
        let query = self.search.trim().to_string();
        let packs: std::collections::HashSet<String> =
            self.packs.iter().filter(|(_, on)| *on)
                .map(|(id, _)| id.clone()).collect();
        std::thread::spawn(move || {
            let r = match job {
                Job::Prep => crate::prep_many(&args,
                                              &[args.source.clone()]),
                Job::Queue => crate::prep_many(&args, &queue_urls),
                Job::Search => crate::yt_search(&args, &query, 15)
                    .map(|hits| {
                    let have = if args.library.as_os_str().is_empty() {
                        Default::default()
                    } else {
                        crate::library_video_ids(&args.library)
                    };
                    for (url, label) in hits {
                        let id = url.rsplit('=').next().unwrap_or("");
                        let _ = tx.send(format!(
                            "{HIT}{url}\t{label}\t{}",
                            have.contains(id)));
                    }
                }),
                Job::Batch => crate::run_batch(&args),
                Job::Models =>
                    crate::models::download(&args.models, Some(&packs)),
            };
            let _ = tx.send(match r {
                Ok(()) => DONE.to_string(),
                Err(e) => format!("{ERR}{e:#}"),
            });
        });
    }

    /// Persist the checkbox set to <models>/langs.json — the file the
    /// CLI `models` download and future GUI launches both read.
    fn save_pack_selection(&self) {
        let set: std::collections::HashSet<String> =
            self.packs.iter().filter(|(_, on)| *on)
                .map(|(id, _)| id.clone()).collect();
        let mdir = if self.models.trim().is_empty() {
            PathBuf::from("models")
        } else {
            PathBuf::from(self.models.trim())
        };
        if let Err(e) = crate::models::save_packs(&mdir, &set) {
            eprintln!("packs save failed: {e}");
        }
    }

    fn models_dir(&self) -> String {
        if self.models.trim().is_empty() {
            "models".into()
        } else {
            self.models.trim().to_string()
        }
    }

    /// Fingerprint of the persisted fields — a change means the prefs
    /// file is stale and gets rewritten.
    fn prefs_sig(&self) -> String {
        format!("{}|{}|{}|{}|{}", self.library.trim(),
                self.models.trim(), self.guide,
                self.no_separate, self.hevc)
    }

    fn model_step(&mut self) -> Step {
        if self.model_status.is_none() {
            self.model_status = Some(model_step(&self.models_dir()));
        }
        Step {
            ok: self.model_status.as_ref().unwrap().ok,
            detail: self.model_status.as_ref().unwrap().detail.clone(),
        }
    }

    /// One checklist row: ✓/✗ marker, step name, detail, action.
    fn step_row(ui: &mut egui::Ui, n: usize, name: &str, s: &Step,
                add_contents: impl FnOnce(&mut egui::Ui)) {
        ui.horizontal(|ui| {
            let (mark, col) = if s.ok {
                ("✔", egui::Color32::from_rgb(60, 180, 90))
            } else {
                ("✗", egui::Color32::from_rgb(220, 80, 70))
            };
            ui.label(egui::RichText::new(mark).color(col).strong());
            ui.label(format!("{n}. {name}"));
            ui.label(egui::RichText::new(&s.detail).weak());
            add_contents(ui);
        });
    }

    /// The first-run checklist — shown expanded until every step is
    /// green, then collapses into a one-line "setup complete" strip
    /// that can be reopened.
    fn setup_card(&mut self, ui: &mut egui::Ui) {
        if self.tools.is_none() {
            self.tools = Some((tool_on_path("yt-dlp"),
                               tool_on_path("ffmpeg")));
        }
        let (ytdlp, ffmpeg) = self.tools.unwrap();
        let tools = Step {
            ok: ytdlp && ffmpeg,
            detail: if ytdlp && ffmpeg {
                "yt-dlp + ffmpeg on PATH".into()
            } else {
                let mut m: Vec<&str> = Vec::new();
                if !ytdlp { m.push("yt-dlp"); }
                if !ffmpeg { m.push("ffmpeg"); }
                format!("install {} and relaunch", m.join(" + "))
            },
        };
        let lib = Step {
            ok: !self.library.trim().is_empty()
                && Path::new(self.library.trim()).is_dir(),
            detail: if self.library.trim().is_empty() {
                "pick the folder prepared songs go into".into()
            } else if Path::new(self.library.trim()).is_dir() {
                self.library.trim().to_string()
            } else {
                "folder does not exist".into()
            },
        };
        let models = self.model_step();
        let all_ok = tools.ok && lib.ok && models.ok;

        if all_ok && !self.setup_open {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("✔ setup complete")
                    .color(egui::Color32::from_rgb(60, 180, 90)));
                if ui.small_button("show").clicked() {
                    self.setup_open = true;
                }
            });
            return;
        }

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Getting started")
                    .strong());
                if all_ok && ui.small_button("hide").clicked() {
                    self.setup_open = false;
                }
            });
            Self::step_row(ui, 1, "Tools", &tools, |_| {});
            Self::step_row(ui, 2, "Library folder", &lib, |ui| {
                if ui.small_button("Browse…").clicked() {
                    if let Some(p) =
                            rfd::FileDialog::new().pick_folder() {
                        self.library = p.display().to_string();
                    }
                }
            });
            Self::step_row(ui, 3, "AI models (~900 MB)", &models, |ui| {
                if !models.ok {
                    let b = ui.add_enabled(!self.busy,
                        egui::Button::new("Download models").small());
                    if b.clicked() {
                        self.start(Job::Models);
                    }
                }
            });
            if !all_ok {
                ui.label(egui::RichText::new(
                    "finish these, then paste a song or playlist \
                     link below").weak().italics());
            }
        });
    }

    fn field(ui: &mut egui::Ui, label: &str, s: &mut String,
             hint: &str,
             browse: Option<fn() -> Option<PathBuf>>) {
        ui.label(label).on_hover_text(hint);
        ui.add(egui::TextEdit::singleline(s).desired_width(430.0)
            .hint_text(hint))
            .on_hover_text(hint);
        if let Some(pick) = browse {
            if ui.button("Browse…").clicked() {
                if let Some(p) = pick() {
                    *s = p.display().to_string();
                }
            }
        } else {
            ui.label("");
        }
        ui.end_row();
    }
}

impl eframe::App for PrepApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // drain log lines + completion sentinel from the worker
        if let Some(rx) = &self.rx {
            let mut drained: Vec<String> = Vec::new();
            while let Ok(m) = rx.try_recv() {
                drained.push(m);
            }
            for m in drained {
                if m == DONE {
                    self.busy = false;
                    self.status = "done".into();
                    crate::set_log_sink(None);
                    self.log.push_back("— finished —".into());
                    // a Models job just wrote new files — recheck
                    self.model_status = None;
                } else if let Some(e) = m.strip_prefix(ERR) {
                    self.busy = false;
                    self.status = format!("failed: {e}");
                    crate::set_log_sink(None);
                    self.log.push_back(format!("FAILED: {e}"));
                } else if let Some(h) = m.strip_prefix(HIT) {
                    let mut p = h.split('\t');
                    if let (Some(u), Some(l), Some(hv)) =
                            (p.next(), p.next(), p.next()) {
                        self.hits.push((u.to_string(), l.to_string(),
                                        hv == "true", false));
                    }
                } else {
                    // cap the pane — batch over a big library would
                    // otherwise grow without bound
                    if self.log.len() >= 4000 {
                        self.log.pop_front();
                    }
                    self.log.push_back(m);
                }
            }
        }
        if self.busy {
            ctx.request_repaint_after(
                std::time::Duration::from_millis(100));
        }

        // persist library/models/options the moment they change —
        // a first-timer shouldn't retype paths on relaunch
        let sig = self.prefs_sig();
        if sig != self.saved_sig {
            save_prefs(self);
            self.saved_sig = sig;
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.busy {
                    ui.spinner();
                }
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Ultra Pi Star — song prep");
            ui.label(
                "Paste a YouTube link — a single video or a whole \
                 playlist — or pick a media file; the song lands in \
                 the library folder as a complete stick-format entry \
                 (karaoke video, chart, art).",
            );
            ui.add_space(6.0);

            self.setup_card(ui);
            ui.add_space(6.0);

            let can_act = !self.busy;
            ui.add_enabled_ui(can_act, |ui| {
                egui::Grid::new("form")
                    .num_columns(3)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        Self::field(ui, "Song", &mut self.source,
                            "YouTube link, playlist URL, or a local \
                             media file — 'Prepare song' runs the \
                             full pipeline on it",
                            Some(|| {
                                rfd::FileDialog::new()
                                    .add_filter("media", &[
                                        "mp4", "mkv", "webm", "mp3",
                                        "m4a", "ogg", "wav", "flac",
                                    ])
                                    .pick_file()
                            }));
                        Self::field(ui, "Library", &mut self.library,
                            "destination root for prepared songs — \
                             the folder tree that syncs to the \
                             pendrive (e.g. station-out)",
                            Some(|| {
                                rfd::FileDialog::new().pick_folder()
                            }));
                        Self::field(ui, "Models", &mut self.models,
                            "AI model pack dir (spleeter, parakeet, \
                             rmvpe) — 'Download models' fills it",
                            Some(|| {
                                rfd::FileDialog::new().pick_folder()
                            }));
                        Self::field(ui, "Artist (opt.)",
                                    &mut self.artist,
                            "override the artist name — YouTube \
                             metadata fills this in automatically",
                            None);
                        Self::field(ui, "Title (opt.)",
                                    &mut self.title,
                            "override the song title — YouTube \
                             metadata fills this in automatically",
                            None);
                        Self::field(ui, "Chart (opt.)",
                                    &mut self.chart,
                            "import a hand-made .txt/.upsong chart \
                             instead of generating one from ASR+pitch",
                            Some(|| {
                                rfd::FileDialog::new()
                                    .add_filter("chart",
                                                &["txt", "upsong"])
                                    .pick_file()
                            }));
                        ui.label("Guide vocal").on_hover_text(
                            "how loud the vocals stay in the karaoke \
                             track — 0 = pure instrumental");
                        ui.add(egui::Slider::new(&mut self.guide,
                            0.0..=0.5)
                            .text("level"));
                        ui.checkbox(&mut self.no_separate,
                                    "skip vocal separation")
                            .on_hover_text(
                                "no spleeter pass — no karaoke track \
                                 or vocal stem (chart still gets made)");
                        ui.checkbox(&mut self.hevc,
                                    "HEVC karaoke video")
                            .on_hover_text(
                                "encode .inst.mp4 as HEVC — the Pi 5 \
                                 decodes it in hardware, h264 it can't");
                        ui.end_row();
                    });

                ui.add_space(4.0);
                egui::CollapsingHeader::new("Language packs")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label(
                            "what 'Download models' fetches on top \
                             of the base pack — whisper covers every \
                             language at once; fonts let the TV \
                             render native scripts");
                        ui.horizontal(|ui| {
                            ui.label("ASR:").on_hover_text(
                                "whisper auto-detects the song's \
                                 language and transcribes it; pick \
                                 the size that fits — tiny is fine \
                                 for most, small is more accurate");
                            let mut changed = false;
                            for (id, on) in self.packs.iter_mut()
                                    .filter(|(id, _)| {
                                        !id.starts_with("font:")}) {
                                let label = crate::models::ASR_PACKS
                                    .iter().find(|(pid, ..)| *pid == id)
                                    .map(|(.., l)| *l)
                                    .unwrap_or(id.as_str());
                                changed |= ui.checkbox(on, label)
                                    .changed();
                            }
                            if changed {
                                self.save_pack_selection();
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Fonts:").on_hover_text(
                                "native-script fonts the TV renderer \
                                 falls back to — tick the scripts \
                                 your songs use");
                            let mut changed = false;
                            for (id, on) in self.packs.iter_mut()
                                    .filter(|(id, _)| {
                                        id.starts_with("font:")}) {
                                let name = ups_core::romanize
                                    ::script_name(id.trim_start_matches(
                                        "font:"));
                                changed |= ui.checkbox(on, name)
                                    .changed();
                            }
                            if changed {
                                self.save_pack_selection();
                            }
                        });
                    });

                ui.add_space(4.0);
                ui.label("Queue — links or playlist URLs, one per \
                          line:")
                    .on_hover_text(
                        "each line is one source: a YouTube video, a \
                         playlist URL (expands to every video), or a \
                         local file path — 'Prepare queue' runs them \
                         in order and keeps going past failures");
                ui.add(egui::TextEdit::multiline(&mut self.queue)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text(
                        "https://youtube.com/playlist?list=…\n\
                         https://youtu.be/…"));

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Find songs:").on_hover_text(
                        "search YouTube without leaving the window — \
                         check the results you want and 'Queue \
                         checked' adds their links to the queue box");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .desired_width(330.0)
                            .hint_text("artist title…"))
                        .on_hover_text(
                            "e.g. 'fleetwood mac dreams' — Enter \
                             searches too");
                    let go = resp.lost_focus()
                        && ui.input(|i| i.key_pressed(
                            egui::Key::Enter));
                    if (ui.button("Search")
                            .on_hover_text("search YouTube")
                            .clicked() || go)
                        && !self.search.trim().is_empty()
                    {
                        self.hits.clear();
                        self.start(Job::Search);
                    }
                    let any = self.hits.iter().any(|h| h.3);
                    if ui.add_enabled(any,
                            egui::Button::new("Queue checked"))
                        .on_hover_text(
                            "append the ticked results to the queue \
                             box above")
                        .clicked()
                    {
                        for h in self.hits.iter().filter(|h| h.3) {
                            self.queue.push_str(&h.0);
                            self.queue.push('\n');
                        }
                        self.hits.retain(|h| !h.3);
                    }
                });
                if !self.hits.is_empty() {
                    egui::ScrollArea::vertical()
                        .max_height(120.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for h in &mut self.hits {
                                ui.horizontal(|ui| {
                                    let cb = egui::Checkbox::new(
                                        &mut h.3, &h.1);
                                    if h.2 {
                                        ui.add_enabled(false, cb)
                                            .on_hover_text(
                                                "already in the \
                                                 library");
                                    } else {
                                        ui.add(cb);
                                    }
                                });
                            }
                        });
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let prep_ok = !self.source.trim().is_empty()
                    && !self.library.trim().is_empty();
                if ui
                    .add_enabled(can_act && prep_ok,
                                 egui::Button::new("Prepare song"))
                    .on_hover_text(
                        "download/normalize/separate/chart this song")
                    .clicked()
                {
                    self.start(Job::Prep);
                }
                let queue_ok = !self.library.trim().is_empty()
                    && self.queue.lines()
                        .any(|l| !l.trim().is_empty());
                if ui
                    .add_enabled(can_act && queue_ok,
                                 egui::Button::new("Prepare queue"))
                    .on_hover_text(
                        "process every link in the queue box — \
                         playlist URLs expand to all their videos")
                    .clicked()
                {
                    self.start(Job::Queue);
                }
                let batch_ok = !self.library.trim().is_empty();
                if ui
                    .add_enabled(can_act && batch_ok,
                                 egui::Button::new(
                                     "Regenerate library charts"))
                    .on_hover_text(
                        "re-run ASR + pitch + chart generation on \
                         every generated chart under the library")
                    .clicked()
                {
                    self.start(Job::Batch);
                }
                if ui
                    .add_enabled(can_act,
                                 egui::Button::new("Download models"))
                    .on_hover_text(
                        "fetch sherpa binaries + spleeter + parakeet \
                         + rmvpe (~900 MB) into the models dir")
                    .clicked()
                {
                    self.start(Job::Models);
                }
            });

            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.label(
                            egui::RichText::new(line.as_str())
                                .monospace(),
                        );
                    }
                });
        });
    }
}
