//! GUI entry: pdf-wm-remover-gui (egui)
//!
//! Flow matches the original Python tool: load PDF -> analyze -> check
//! candidate watermarks -> remove -> save. Removal is content-stream level.

use std::collections::HashSet;
use std::path::PathBuf;

use eframe::egui;
use pdf_wm_remover::{analyze, load_document, remove_watermarks, Candidate};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 660.0])
            .with_min_inner_size([700.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PDF Watermark Remover",
        options,
        Box::new(|_cc| Ok(Box::<App>::default())),
    )
}

struct App {
    input: Option<PathBuf>,
    candidates: Vec<Candidate>,
    checked: HashSet<usize>,
    manual_keywords: String,
    log: Vec<String>,
    status: String,
}

impl Default for App {
    fn default() -> Self {
        Self {
            input: None,
            candidates: Vec::new(),
            checked: HashSet::new(),
            manual_keywords: String::new(),
            log: vec!["Ready — open a PDF to begin.".into()],
            status: "Ready".into(),
        }
    }
}

impl App {
    fn log(&mut self, msg: impl Into<String>) {
        let line = msg.into();
        eprintln!("{line}");
        self.log.push(line);
        if self.log.len() > 200 {
            self.log.drain(..50);
        }
    }

    fn keywords(&self) -> Vec<String> {
        let mut kws: Vec<String> = self
            .checked
            .iter()
            .filter_map(|&i| self.candidates.get(i))
            .map(|c| c.text.clone())
            .collect();
        for kw in self.manual_keywords.split(',') {
            let kw = kw.trim();
            if !kw.is_empty() {
                kws.push(kw.to_string());
            }
        }
        kws
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("📂 Open PDF").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .pick_file()
                    {
                        match load_document(&path, None) {
                            Ok(doc) => {
                                let total = doc.get_pages().len();
                                match analyze(&doc, 0.3, 2) {
                                    Ok(cands) => {
                                        self.candidates = cands;
                                        self.checked.clear();
                                        self.input = Some(path.clone());
                                        self.status = format!(
                                            "{} pages, {} candidates",
                                            total,
                                            self.candidates.len()
                                        );
                                        self.log(format!(
                                            "Opened {} ({} pages, encrypted={})",
                                            path.display(),
                                            total,
                                            doc.is_encrypted()
                                        ));
                                        self.log(format!(
                                            "Analyze: {} watermark candidates found.",
                                            self.candidates.len()
                                        ));
                                    }
                                    Err(e) => self.log(format!("Analyze failed: {e}")),
                                }
                            }
                            Err(e) => self.log(format!("Open failed: {e}")),
                        }
                    }
                }
                if ui.button("🔍 Re-Analyze").clicked() && self.input.is_some() {
                    if let Ok(doc) = load_document(self.input.as_ref().unwrap(), None) {
                        match analyze(&doc, 0.3, 2) {
                            Ok(cands) => {
                                self.candidates = cands;
                                self.checked.clear();
                                self.status =
                                    format!("{} candidates", self.candidates.len());
                                self.log(format!(
                                    "Re-analyzed: {} candidates.",
                                    self.candidates.len()
                                ));
                            }
                            Err(e) => self.log(format!("Analyze failed: {e}")),
                        }
                    }
                }
                let kw_empty = self.keywords().is_empty();
                if ui
                    .add_enabled(self.input.is_some() && !kw_empty, egui::Button::new("💾 Save Cleaned PDF"))
                    .clicked()
                {
                    let default_name = format!(
                        "cleaned_{}",
                        self.input
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .and_then(|f| f.to_str())
                            .unwrap_or("output.pdf")
                    );
                    if let Some(out) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .set_file_name(default_name)
                        .save_file()
                    {
                        let input = self.input.clone().unwrap();
                        let kws = self.keywords();
                        match remove_watermarks(&input, &out, &kws, None) {
                            Ok(report) => {
                                self.status = format!(
                                    "Saved: {} blocks removed, {} pages touched",
                                    report.removed_blocks, report.pages_touched
                                );
                                self.log(format!(
                                    "Saved {} — {} text blocks removed from {} pages (permissions stripped).",
                                    out.display(),
                                    report.removed_blocks,
                                    report.pages_touched
                                ));
                            }
                            Err(e) => self.log(format!("Remove failed: {e}")),
                        }
                    }
                }
                if let Some(p) = &self.input {
                    ui.separator();
                    ui.label(format!(
                        "📄 {}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
                ui.separator();
                ui.label(&self.status);
            });
            ui.horizontal(|ui| {
                ui.label("Manual keywords (comma separated, optional):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.manual_keywords)
                        .hint_text("e.g. Confidential, Downloaded by, SLS Terms"),
                );
            });
        });

        egui::SidePanel::right("log")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                ui.heading("Log");
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in &self.log {
                            ui.monospace(l);
                        }
                    });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Watermark candidates (repeated text across pages)");
            ui.label("Check the items to remove, then click 'Save Cleaned PDF'.");
            ui.add_space(4.0);
            if self.candidates.is_empty() {
                ui.weak("No candidates — open a PDF (analysis runs automatically).");
            }
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.columns(3, |cols| {
                        for (i, c) in self.candidates.iter().enumerate() {
                            let col = i % 3;
                            let mut checked = self.checked.contains(&i);
                            let label = format!(
                                "[{}x, {:.1}pt]  {}",
                                c.count,
                                c.size,
                                c.text.chars().take(70).collect::<String>()
                            );
                            if cols[col].checkbox(&mut checked, label).changed() {
                                if checked {
                                    self.checked.insert(i);
                                } else {
                                    self.checked.remove(&i);
                                }
                            }
                        }
                    });
                });
            ui.add_space(6.0);
            let n = self.checked.len();
            ui.label(format!("Selected: {n}"));
        });
    }
}