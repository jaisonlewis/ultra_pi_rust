//! Optional native window for the prep pipeline — build with
//! `--features gui`, then `ups-prep --gui` or a bare double-click
//! opens it. Wraps the same `prep`/`run_batch` entry points the CLI
//! uses; progress lines arrive over the `say!` tee.

use std::path::PathBuf;
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
    busy: bool,
    status: String,
    log: std::collections::VecDeque<String>,
    rx: Option<Receiver<String>>,
}

impl Default for PrepApp {
    fn default() -> Self {
        PrepApp {
            source: String::new(),
            queue: String::new(),
            library: String::new(),
            models: std::env::var("UPS_MODELS_DIR")
                .unwrap_or_else(|_| "models".into()),
            artist: String::new(),
            title: String::new(),
            chart: String::new(),
            guide: 0.15,
            no_separate: false,
            hevc: false,
            search: String::new(),
            hits: Vec::new(),
            busy: false,
            status: String::new(),
            log: std::collections::VecDeque::new(),
            rx: None,
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
            gui: false,
        }
    }

    fn start(&mut self, job: Job) {
        let (tx, rx) = channel();
        crate::set_log_sink(Some(tx.clone()));
        self.rx = Some(rx);
        self.busy = true;
        self.status = "working…".into();
        let args = self.args(matches!(job, Job::Batch));
        let queue_urls: Vec<String> = self.queue.lines()
            .flat_map(|l| l.split_whitespace())
            .map(str::to_string)
            .collect();
        let query = self.search.trim().to_string();
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
                Job::Models => crate::models::download(&args.models),
            };
            let _ = tx.send(match r {
                Ok(()) => DONE.to_string(),
                Err(e) => format!("{ERR}{e:#}"),
            });
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
