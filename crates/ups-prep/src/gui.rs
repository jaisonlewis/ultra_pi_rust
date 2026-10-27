//! Optional native window for the prep pipeline — build with
//! `--features gui`, then `ups-prep --gui` or a bare double-click
//! opens it. Wraps the same `prep`/`run_batch` entry points the CLI
//! uses; progress lines arrive over the `say!` tee.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

use eframe::egui;

const DONE: &str = "job://done";
const ERR: &str = "job://error ";

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
    library: String,
    models: String,
    artist: String,
    title: String,
    chart: String,
    guide: f32,
    no_separate: bool,
    busy: bool,
    status: String,
    log: std::collections::VecDeque<String>,
    rx: Option<Receiver<String>>,
}

impl Default for PrepApp {
    fn default() -> Self {
        PrepApp {
            source: String::new(),
            library: String::new(),
            models: std::env::var("UPS_MODELS_DIR")
                .unwrap_or_else(|_| "models".into()),
            artist: String::new(),
            title: String::new(),
            chart: String::new(),
            guide: 0.15,
            no_separate: false,
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
            batch,
            gui: false,
        }
    }

    fn start(&mut self, batch: bool) {
        let (tx, rx) = channel();
        crate::set_log_sink(Some(tx.clone()));
        self.rx = Some(rx);
        self.busy = true;
        self.status = "working…".into();
        let args = self.args(batch);
        std::thread::spawn(move || {
            let r = if batch {
                crate::run_batch(&args)
            } else {
                crate::prep(&args)
            };
            let _ = tx.send(match r {
                Ok(()) => DONE.to_string(),
                Err(e) => format!("{ERR}{e:#}"),
            });
        });
    }

    fn field(ui: &mut egui::Ui, label: &str, s: &mut String,
             browse: Option<fn() -> Option<PathBuf>>) {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(s).desired_width(430.0));
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
                "Paste a YouTube link or pick a media file; the song \
                 lands in the library folder as a complete \
                 stick-format entry (karaoke video, chart, art).",
            );
            ui.add_space(6.0);

            let can_act = !self.busy;
            ui.add_enabled_ui(can_act, |ui| {
                egui::Grid::new("form")
                    .num_columns(3)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        Self::field(ui, "Song", &mut self.source,
                            Some(|| {
                                rfd::FileDialog::new()
                                    .add_filter("media", &[
                                        "mp4", "mkv", "webm", "mp3",
                                        "m4a", "ogg", "wav", "flac",
                                    ])
                                    .pick_file()
                            }));
                        Self::field(ui, "Library", &mut self.library,
                            Some(|| {
                                rfd::FileDialog::new().pick_folder()
                            }));
                        Self::field(ui, "Models", &mut self.models,
                            Some(|| {
                                rfd::FileDialog::new().pick_folder()
                            }));
                        Self::field(ui, "Artist (opt.)",
                                    &mut self.artist, None);
                        Self::field(ui, "Title (opt.)",
                                    &mut self.title, None);
                        Self::field(ui, "Chart (opt.)",
                                    &mut self.chart,
                            Some(|| {
                                rfd::FileDialog::new()
                                    .add_filter("chart",
                                                &["txt", "upsong"])
                                    .pick_file()
                            }));
                        ui.label("Guide vocal");
                        ui.add(egui::Slider::new(&mut self.guide,
                            0.0..=0.5)
                            .text("level"));
                        ui.checkbox(&mut self.no_separate,
                                    "skip vocal separation");
                        ui.end_row();
                    });
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
                    self.start(false);
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
                    self.start(true);
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
