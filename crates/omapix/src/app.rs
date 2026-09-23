use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::{Align, Button, Layout, Pos2, RichText, Ui};
use omapix_engine::layer::{Layer, Mask};
use omapix_engine::{Document, export, filters, ops, ora};

use crate::canvas::ToolInput;
use crate::commands::Command;
use crate::editor::{Editor, Target};
use crate::layers_panel::LayersPanel;
use crate::theme::{self, Theme};
use crate::tools::Tools;

const OPEN_EXTENSIONS: [&str; 6] = ["ora", "tif", "tiff", "png", "jpg", "jpeg"];
const JPEG_QUALITY: u8 = 92;

/// What a file dialog was opened for.
#[derive(Clone, Copy)]
enum Purpose {
    Open,
    SaveAs,
    ExportTiff,
    ExportJpeg,
}

/// Something to do once unsaved changes are dealt with.
#[derive(Clone, Copy)]
enum Then {
    Quit,
    Open,
}

enum Dialog {
    /// Ask for a radius, then run a filter.
    Radius {
        command: Command,
        radius: f32,
    },
    UnsavedChanges {
        then: Then,
    },
}

/// Result of a background save or export: the document revision written,
/// where, and whether it was a native save (vs a flattened export).
type Written = Result<(u64, PathBuf, bool), String>;

/// A document read from disk, with the original profile's name if a linear
/// file was converted for editing.
type Opened = Result<(Document, Option<String>), String>;

/// A save or export running in the background.
struct FileJob {
    label: String,
    rx: Receiver<Written>,
}

/// One step of an `OMAPIX_SCRIPT` (comma-separated steps):
/// - a command name, e.g. `FrequencySeparation` (radius commands use their
///   default radius);
/// - `Stroke x0 y0 x1 y1`: a brush stroke with the current tool, in image
///   pixels, through the same path as mouse strokes;
/// - `Tool Brush|Eraser|Clone|Heal`, `Size n`, `Opacity percent`,
///   `Color r g b` (sRGB), `Source x y` (clone/heal source, like Alt+click),
///   `Look x y` (centre the view on an image point at 100 %).
#[derive(Debug)]
enum ScriptStep {
    Command(Command),
    Stroke([f32; 4]),
    Tool(crate::tools::Tool),
    Size(f32),
    Opacity(f32),
    Color([u8; 3]),
    Source(Pos2),
    Look(Pos2),
}

impl ScriptStep {
    fn parse(step: &str) -> Option<Self> {
        let mut words = step.split_whitespace();
        let head = words.next()?;
        let nums: Vec<f32> = words.clone().filter_map(|w| w.parse().ok()).collect();
        Some(match (head, nums.as_slice()) {
            ("Stroke", &[x0, y0, x1, y1]) => ScriptStep::Stroke([x0, y0, x1, y1]),
            ("Size", &[n]) => ScriptStep::Size(n),
            ("Opacity", &[n]) => ScriptStep::Opacity(n / 100.0),
            ("Color", &[r, g, b]) => ScriptStep::Color([r as u8, g as u8, b as u8]),
            ("Source", &[x, y]) => ScriptStep::Source(egui::pos2(x, y)),
            ("Look", &[x, y]) => ScriptStep::Look(egui::pos2(x, y)),
            ("Tool", _) => match words.next()? {
                "Brush" => ScriptStep::Tool(crate::tools::Tool::Brush),
                "Eraser" => ScriptStep::Tool(crate::tools::Tool::Eraser),
                "Clone" => ScriptStep::Tool(crate::tools::Tool::CloneStamp),
                "Heal" => ScriptStep::Tool(crate::tools::Tool::Healing),
                _ => return None,
            },
            _ => ScriptStep::Command(Command::from_name(head)?),
        })
    }
}

/// Work to do after a dialog closes, which needs the whole app.
type DialogAction = Box<dyn FnOnce(&mut App, &egui::Context)>;

pub struct App {
    theme: Theme,
    theme_rx: Receiver<Theme>,
    editor: Option<Editor>,
    layers: LayersPanel,
    tools: Tools,
    /// A file being opened in the background.
    opening: Option<(PathBuf, Receiver<Opened>)>,
    /// A file dialog open in the background.
    picking: Option<(Purpose, Receiver<Option<PathBuf>>)>,
    file_job: Option<FileJob>,
    dialog: Option<Dialog>,
    /// A transient message for the status bar, and whether it's an error.
    status: Option<(String, bool, Instant)>,
    blur_radius: f32,
    separation_radius: Option<f32>,
    /// The user chose to discard changes, so the next close goes through.
    allow_close: bool,
    title: String,
    /// Steps to run once an image is open, from `OMAPIX_SCRIPT`. For testing
    /// the real UI without a mouse; see [`ScriptStep`].
    script: VecDeque<ScriptStep>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let ctx = &cc.egui_ctx;
        // Ctrl+= / Ctrl+- zoom the image, not the interface.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        theme::install_font(ctx);
        let theme = Theme::load();
        ctx.set_visuals(theme.visuals());

        let mut app = Self {
            theme,
            theme_rx: theme::watch(ctx.clone()),
            editor: None,
            layers: LayersPanel::default(),
            tools: Tools::default(),
            opening: None,
            picking: None,
            file_job: None,
            dialog: None,
            status: None,
            blur_radius: 2.0,
            separation_radius: None,
            allow_close: false,
            title: String::new(),
            script: std::env::var("OMAPIX_SCRIPT")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .filter_map(|step| {
                    let parsed = ScriptStep::parse(step.trim());
                    if parsed.is_none() {
                        log::warn!("OMAPIX_SCRIPT: can't understand {step:?}");
                    }
                    parsed
                })
                .collect(),
        };
        if let Some(path) = path {
            app.open(path, ctx);
        }
        app
    }

    fn message(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some((text.into(), error, Instant::now()));
    }

    fn modified(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.modified)
    }

    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let target = path.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = omapix_engine::io::load(&target)
                .and_then(|mut doc| {
                    let converted = ops::prepare_for_editing(&mut doc)?;
                    Ok((doc, converted))
                })
                .map_err(|e| e.to_string());
            log::info!("opened {} in {:?}", target.display(), started.elapsed());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.opening = Some((path, rx));
    }

    fn pick(&mut self, purpose: Purpose, ctx: &egui::Context) {
        if self.picking.is_some() {
            return;
        }
        let doc = self.editor.as_ref().map(|e| &e.doc);
        let dir = doc
            .map(|d| d.saved_path.as_ref().unwrap_or(&d.path))
            .and_then(|p| p.parent())
            .map(Path::to_path_buf);
        let stem = doc
            .and_then(|d| d.path.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = dir {
            dialog = dialog.set_directory(dir);
        }
        dialog = match purpose {
            Purpose::Open => dialog
                .set_title("Open")
                .add_filter("Images", &OPEN_EXTENSIONS),
            Purpose::SaveAs => dialog
                .set_title("Save As")
                .add_filter("OpenRaster", &["ora"])
                .set_file_name(format!("{stem}.ora")),
            Purpose::ExportTiff => dialog
                .set_title("Export as TIFF")
                .add_filter("TIFF", &["tif", "tiff"])
                .set_file_name(format!("{stem}-edit.tif")),
            Purpose::ExportJpeg => dialog
                .set_title("Export as JPEG")
                .add_filter("JPEG", &["jpg", "jpeg"])
                .set_file_name(format!("{stem}.jpg")),
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let file = match purpose {
                Purpose::Open => dialog.pick_file(),
                _ => dialog.save_file(),
            };
            let _ = tx.send(file);
            ctx.request_repaint();
        });
        self.picking = Some((purpose, rx));
    }

    /// Write the document in the background: a native .ora save that becomes
    /// the document's file, or a flattened export.
    fn write(&mut self, purpose: Purpose, path: PathBuf, ctx: &egui::Context) {
        let Some(editor) = &self.editor else { return };
        if self.file_job.is_some() {
            self.message("Still saving — try again in a moment", true);
            return;
        }
        let doc = editor.doc.clone();
        let revision = editor.revision();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let label = match purpose {
            Purpose::ExportTiff | Purpose::ExportJpeg => "Exporting",
            _ => "Saving",
        };
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = match purpose {
                Purpose::ExportTiff => export::tiff(&doc, &path).map(|_| false),
                Purpose::ExportJpeg => export::jpeg(&doc, &path, JPEG_QUALITY).map(|_| false),
                _ => ora::save(&doc, &path).map(|_| true),
            };
            log::info!("wrote {} in {:?}", path.display(), started.elapsed());
            let _ = tx.send(
                result
                    .map(|native| (revision, path, native))
                    .map_err(|e| e.to_string()),
            );
            ctx.request_repaint();
        });
        self.file_job = Some(FileJob {
            label: label.into(),
            rx,
        });
    }

    fn save(&mut self, ctx: &egui::Context) {
        let saved = self.editor.as_ref().and_then(|e| e.doc.saved_path.clone());
        match saved {
            Some(path) => self.write(Purpose::SaveAs, path, ctx),
            None => self.pick(Purpose::SaveAs, ctx),
        }
    }

    /// Run `then` now, or ask about unsaved changes first.
    fn guard(&mut self, then: Then, ctx: &egui::Context) {
        if self.modified() {
            self.dialog = Some(Dialog::UnsavedChanges { then });
        } else {
            self.proceed(then, ctx);
        }
    }

    fn proceed(&mut self, then: Then, ctx: &egui::Context) {
        match then {
            Then::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Then::Open => self.pick(Purpose::Open, ctx),
        }
    }

    /// Pick up results from background work.
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(theme) = self.theme_rx.try_iter().last() {
            ctx.set_visuals(theme.visuals());
            self.theme = theme;
        }
        if let Some((purpose, rx)) = &self.picking
            && let Ok(result) = rx.try_recv()
        {
            let purpose = *purpose;
            self.picking = None;
            if let Some(path) = result {
                match purpose {
                    Purpose::Open => self.open(path, ctx),
                    Purpose::SaveAs => self.write(purpose, path.with_extension("ora"), ctx),
                    _ => self.write(purpose, path, ctx),
                }
            }
        }
        if let Some((path, rx)) = &self.opening
            && let Ok(result) = rx.try_recv()
        {
            let path = path.clone();
            self.opening = None;
            match result.and_then(|(doc, converted)| Ok((Editor::new(doc)?, converted))) {
                Ok((editor, converted)) => {
                    if let Some(original) = converted {
                        let now = editor.doc.profile.description().to_owned();
                        self.message(format!("Converted {original} to {now} for editing"), false);
                    }
                    self.editor = Some(editor);
                    self.separation_radius = None;
                }
                Err(err) => self.message(format!("Couldn't open {}: {err}", path.display()), true),
            }
        }
        if let Some(job) = &self.file_job
            && let Ok(result) = job.rx.try_recv()
        {
            let label = job.label.clone();
            self.file_job = None;
            match result {
                Ok((revision, path, native)) => {
                    if native && let Some(editor) = &mut self.editor {
                        editor.doc.saved_path = Some(path.clone());
                        editor.mark_saved(revision);
                    }
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let verb = if native { "Saved" } else { "Exported" };
                    self.message(format!("{verb} {name}"), false);
                }
                Err(err) => self.message(format!("{label} failed: {err}"), true),
            }
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            if self.modified() {
                self.message(
                    "Save or close the current image before opening another",
                    true,
                );
            } else {
                self.open(path, ctx);
            }
        }
        // Closing the window with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && self.modified() && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::UnsavedChanges { then: Then::Quit });
        }
    }

    fn enabled(&self, cmd: Command) -> bool {
        let Some(editor) = &self.editor else {
            return matches!(cmd, Command::Open | Command::Quit);
        };
        let view = matches!(
            cmd,
            Command::ZoomIn | Command::ZoomOut | Command::FitOnScreen | Command::ActualPixels
        );
        if editor.busy().is_some() && !view && !matches!(cmd, Command::Quit) {
            return false;
        }
        let index = editor.active_index();
        let has_mask = editor
            .doc
            .layer(editor.active)
            .is_some_and(|l| l.mask.is_some());
        match cmd {
            Command::Undo => editor.undo_label().is_some(),
            Command::Redo => editor.redo_label().is_some(),
            Command::DeleteLayer => editor.doc.layers.len() > 1,
            Command::MergeDown | Command::LowerLayer => index.is_some_and(|i| i > 0),
            Command::RaiseLayer => index.is_some_and(|i| i + 1 < editor.doc.layers.len()),
            Command::AddMask => !has_mask,
            Command::DeleteMask | Command::ToggleMask => has_mask,
            Command::GaussianBlur => editor.target == Target::Pixels,
            _ => true,
        }
    }

    fn run(&mut self, cmd: Command, ctx: &egui::Context) {
        if !self.enabled(cmd) {
            return;
        }
        match cmd {
            Command::Open => self.guard(Then::Open, ctx),
            Command::Quit => self.guard(Then::Quit, ctx),
            Command::Save => self.save(ctx),
            Command::SaveAs => self.pick(Purpose::SaveAs, ctx),
            Command::ExportTiff => self.pick(Purpose::ExportTiff, ctx),
            Command::ExportJpeg => self.pick(Purpose::ExportJpeg, ctx),
            Command::GaussianBlur => {
                self.dialog = Some(Dialog::Radius {
                    command: cmd,
                    radius: self.blur_radius,
                });
            }
            Command::FrequencySeparation => {
                let Some(editor) = &self.editor else { return };
                // ~8.6 px on a 24 MP frame, scaling with resolution.
                let longest = editor.doc.width.max(editor.doc.height) as f32;
                let default = (longest / 700.0 * 10.0).round() / 10.0;
                let radius = self.separation_radius.unwrap_or(default);
                self.dialog = Some(Dialog::Radius {
                    command: cmd,
                    radius,
                });
            }
            _ => {
                if let Some(editor) = &mut self.editor {
                    run_on_editor(editor, cmd, ctx);
                }
            }
        }
    }

    fn apply_radius(&mut self, command: Command, radius: f32, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let index = editor.active_index().unwrap_or(0);
        match command {
            Command::GaussianBlur => {
                self.blur_radius = radius;
                let id = editor.active;
                editor.edit_in_background(
                    "Gaussian Blur",
                    move |doc, _| {
                        if let Some(layer) = doc.layer_mut(id) {
                            layer.pixels = filters::gaussian_blur(&layer.pixels, radius);
                        }
                    },
                    ctx,
                );
            }
            Command::FrequencySeparation => {
                self.separation_radius = Some(radius);
                editor.target = Target::Pixels;
                editor.edit_in_background(
                    "Frequency Separation",
                    move |doc, active| {
                        let (_, high) = ops::frequency_separation(doc, index, radius);
                        *active = high;
                    },
                    ctx,
                );
            }
            _ => {}
        }
    }

    fn menu_item(&mut self, ui: &mut Ui, cmd: Command, label: Option<String>) {
        let text = label.unwrap_or_else(|| cmd.label().to_owned());
        let mut button = Button::new(text);
        if let Some(shortcut) = cmd.shortcut() {
            button = button.shortcut_text(ui.ctx().format_shortcut(&shortcut));
        }
        if ui.add_enabled(self.enabled(cmd), button).clicked() {
            let ctx = ui.ctx().clone();
            self.run(cmd, &ctx);
        }
    }

    fn menu_bar(&mut self, ui: &mut Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                self.menu_item(ui, Command::Open, None);
                ui.separator();
                self.menu_item(ui, Command::Save, None);
                self.menu_item(ui, Command::SaveAs, None);
                ui.separator();
                self.menu_item(ui, Command::ExportTiff, None);
                self.menu_item(ui, Command::ExportJpeg, None);
                ui.separator();
                self.menu_item(ui, Command::Quit, None);
            });
            ui.menu_button("Edit", |ui| {
                let editor = self.editor.as_ref();
                let undo = editor
                    .and_then(|e| e.undo_label())
                    .map(|l| format!("Undo {l}"));
                let redo = editor
                    .and_then(|e| e.redo_label())
                    .map(|l| format!("Redo {l}"));
                self.menu_item(ui, Command::Undo, undo);
                self.menu_item(ui, Command::Redo, redo);
            });
            ui.menu_button("Layer", |ui| {
                self.menu_item(ui, Command::NewLayer, None);
                self.menu_item(ui, Command::DuplicateLayer, None);
                self.menu_item(ui, Command::DeleteLayer, None);
                ui.separator();
                self.menu_item(ui, Command::AddMask, None);
                self.menu_item(ui, Command::ToggleMask, None);
                self.menu_item(ui, Command::DeleteMask, None);
                ui.separator();
                self.menu_item(ui, Command::RaiseLayer, None);
                self.menu_item(ui, Command::LowerLayer, None);
                ui.separator();
                self.menu_item(ui, Command::MergeDown, None);
                self.menu_item(ui, Command::StampVisible, None);
            });
            ui.menu_button("Image", |ui| {
                self.menu_item(ui, Command::Invert, None);
            });
            ui.menu_button("Filter", |ui| {
                self.menu_item(ui, Command::GaussianBlur, None);
            });
            ui.menu_button("Retouch", |ui| {
                self.menu_item(ui, Command::FrequencySeparation, None);
                self.menu_item(ui, Command::DodgeAndBurn, None);
            });
            ui.menu_button("View", |ui| {
                self.menu_item(ui, Command::ZoomIn, None);
                self.menu_item(ui, Command::ZoomOut, None);
                self.menu_item(ui, Command::FitOnScreen, None);
                self.menu_item(ui, Command::ActualPixels, None);
            });
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let dim = self.theme.dark_foreground;
        ui.horizontal(|ui| {
            if let Some(editor) = &self.editor {
                let doc = &editor.doc;
                ui.label(editor.canvas.zoom_label());
                ui.separator();
                ui.label(RichText::new(format!("{} × {} px", doc.width, doc.height)).color(dim));
                ui.label(RichText::new(format!("{}-bit", doc.source_bits)).color(dim));
                ui.label(RichText::new(doc.profile.description()).color(dim));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let busy = self
                    .opening
                    .as_ref()
                    .map(|(p, _)| {
                        let name = p.file_name().unwrap_or_default().to_string_lossy();
                        format!("Opening {name}…")
                    })
                    .or_else(|| self.file_job.as_ref().map(|j| format!("{}…", j.label)))
                    .or_else(|| {
                        let editor = self.editor.as_ref()?;
                        editor.busy().map(|l| format!("{l}…"))
                    });
                if let Some(text) = busy {
                    ui.spinner();
                    ui.label(text);
                } else if let Some((text, error, _)) = &self.status {
                    let colour = if *error {
                        self.theme.red
                    } else {
                        self.theme.accent
                    };
                    ui.label(RichText::new(text).color(colour));
                } else if let Some(((x, y), [r, g, b, _])) =
                    self.editor.as_ref().and_then(|e| e.canvas.hovered_value())
                {
                    let v = |c: u16| c >> 8;
                    let rgb = format!("R {:>3}  G {:>3}  B {:>3}", v(r), v(g), v(b));
                    ui.label(RichText::new(rgb).color(dim));
                    ui.label(format!("X {x}  Y {y}"));
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut close = false;
        let mut action: Option<DialogAction> = None;
        let hint = self.theme.dark_foreground;
        let name = self
            .editor
            .as_ref()
            .map(|e| e.doc.file_name())
            .unwrap_or_default();
        let response = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
            ui.set_min_width(340.0);
            match dialog {
                Dialog::Radius { command, radius } => {
                    ui.heading(command.label().trim_end_matches('…'));
                    ui.add_space(8.0);
                    if *command == Command::FrequencySeparation {
                        ui.label(
                            RichText::new(
                                "Raise the radius until skin blotches vanish from the\n\
                                 color/tone layer and only pores remain in texture.",
                            )
                            .color(hint),
                        );
                        ui.add_space(8.0);
                    }
                    ui.horizontal(|ui| {
                        ui.label("Radius");
                        let value = egui::DragValue::new(radius)
                            .range(0.1..=250.0)
                            .speed(0.1)
                            .suffix(" px")
                            .fixed_decimals(1);
                        ui.add(value);
                    });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ok = ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ok {
                            let (command, radius) = (*command, *radius);
                            action = Some(Box::new(move |app, ctx| {
                                app.apply_radius(command, radius, ctx)
                            }));
                            close = true;
                        }
                    });
                }
                Dialog::UnsavedChanges { then } => {
                    let then = *then;
                    ui.heading("Unsaved changes");
                    ui.add_space(8.0);
                    ui.label(format!("Save changes to {name} before closing it?"));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            action = Some(Box::new(|app, ctx| app.save(ctx)));
                            close = true;
                        }
                        if ui.button("Don't Save").clicked() {
                            action = Some(Box::new(move |app, ctx| {
                                if let Some(e) = &mut app.editor {
                                    e.modified = false;
                                }
                                app.proceed(then, ctx);
                            }));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if close || response.should_close() {
            self.dialog = None;
        }
        if let Some(action) = action {
            action(self, ctx);
        }
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let hint = self.theme.dark_foreground;
        let accent = self.theme.accent;
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.4);
                ui.label(RichText::new("Omapix").size(28.0).color(accent));
                ui.add_space(8.0);
                ui.label(RichText::new("Open an image with Ctrl+O, or drop one here").color(hint));
                ui.add_space(12.0);
                if ui.button("Open…").clicked() {
                    self.run(Command::Open, &ctx);
                }
            });
        });
    }

    fn tool_input(&mut self, input: ToolInput) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        match input {
            ToolInput::StrokeBegin(p) => {
                let Some(paint) = self.tools.paint(editor.target, &editor.doc.profile, p) else {
                    return;
                };
                let (settings, sample_all) = (self.tools.settings(), self.tools.sample_all);
                if editor.begin_stroke(settings, paint, sample_all) {
                    editor.stroke_to(p.x, p.y);
                }
            }
            ToolInput::StrokeMove(p) => editor.stroke_to(p.x, p.y),
            ToolInput::StrokeEnd => editor.end_stroke(),
            ToolInput::Sample(p) if self.tools.tool.copies() => self.tools.set_source(p),
            ToolInput::Sample(p) => {
                if p.x >= 0.0
                    && p.y >= 0.0
                    && let Some(pixel) = editor.canvas.sample(p.x as u32, p.y as u32)
                {
                    self.tools.sample(pixel, &editor.doc.profile);
                }
            }
        }
    }

    /// Run the next scripted command once the app is idle.
    fn run_script(&mut self, ctx: &egui::Context) {
        let idle = self.opening.is_none()
            && self.file_job.is_none()
            && self.dialog.is_none()
            && self
                .editor
                .as_ref()
                .is_some_and(|e| e.busy().is_none() && e.canvas.ready());
        if !idle {
            return;
        }
        let Some(step) = self.script.pop_front() else {
            return;
        };
        log::info!("script: {step:?}");
        match step {
            ScriptStep::Command(cmd) => {
                self.run(cmd, ctx);
                // Radius commands open a dialog; accept its default.
                if let Some(Dialog::Radius { command, radius }) = self.dialog.take() {
                    self.apply_radius(command, radius, ctx);
                }
            }
            ScriptStep::Stroke([x0, y0, x1, y1]) => {
                self.tool_input(ToolInput::StrokeBegin(egui::pos2(x0, y0)));
                // Several moves, like a real drag across frames.
                for i in 1..=20 {
                    let t = i as f32 / 20.0;
                    let p = egui::pos2(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                    self.tool_input(ToolInput::StrokeMove(p));
                }
                self.tool_input(ToolInput::StrokeEnd);
            }
            ScriptStep::Tool(tool) => self.tools.tool = tool,
            ScriptStep::Size(n) => self.tools.set_size(n),
            ScriptStep::Opacity(o) => self.tools.set_opacity(o),
            ScriptStep::Color(c) => self.tools.foreground = c,
            ScriptStep::Source(p) => self.tools.set_source(p),
            ScriptStep::Look(p) => {
                if let Some(editor) = &mut self.editor {
                    editor.canvas.look_at(p);
                }
            }
        }
        ctx.request_repaint();
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.editor {
            Some(e) => format!(
                "{}{} — Omapix",
                e.doc.file_name(),
                if e.modified { " •" } else { "" }
            ),
            None => "Omapix".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

/// Commands that only change the open document.
fn run_on_editor(editor: &mut Editor, cmd: Command, ctx: &egui::Context) {
    let index = editor.active_index().unwrap_or(0);
    let id = editor.active;
    let (w, h) = (editor.doc.width, editor.doc.height);
    match cmd {
        Command::Undo => editor.undo(),
        Command::Redo => editor.redo(),
        Command::NewLayer => {
            editor.target = Target::Pixels;
            editor.edit("New Layer", |doc, active| {
                let new = doc.next_layer_id();
                let name = doc.unused_name("Layer");
                doc.layers.insert(index + 1, Layer::empty(new, name, w, h));
                *active = new;
            });
        }
        Command::DuplicateLayer => {
            editor.edit("Duplicate Layer", |doc, active| {
                let new = doc.next_layer_id();
                let mut copy = doc.layers[index].clone();
                copy.id = new;
                copy.name = format!("{} copy", copy.name);
                doc.layers.insert(index + 1, copy);
                *active = new;
            });
        }
        Command::DeleteLayer => {
            editor.edit("Delete Layer", |doc, active| {
                doc.layers.remove(index);
                *active = doc.layers[index.saturating_sub(1).min(doc.layers.len() - 1)].id;
            });
            editor.fix_selection();
        }
        Command::RaiseLayer => {
            editor.edit("Bring Forward", |doc, _| doc.layers.swap(index, index + 1));
        }
        Command::LowerLayer => {
            editor.edit("Send Backward", |doc, _| doc.layers.swap(index, index - 1));
        }
        Command::AddMask => {
            editor.edit("Add Layer Mask", |doc, _| {
                if let Some(l) = doc.layer_mut(id) {
                    l.mask = Some(Mask::white(w, h));
                }
            });
            editor.target = Target::Mask;
        }
        Command::DeleteMask => {
            editor.edit("Delete Layer Mask", |doc, _| {
                if let Some(l) = doc.layer_mut(id) {
                    l.mask = None;
                }
            });
            editor.fix_selection();
        }
        Command::ToggleMask => {
            editor.edit("Disable/Enable Layer Mask", |doc, _| {
                if let Some(m) = doc.layer_mut(id).and_then(|l| l.mask.as_mut()) {
                    m.enabled = !m.enabled;
                }
            });
        }
        Command::Invert => {
            if editor.target == Target::Mask {
                editor.edit("Invert Mask", |doc, _| {
                    if let Some(m) = doc.layer_mut(id).and_then(|l| l.mask.as_mut()) {
                        m.invert();
                    }
                });
            } else {
                editor.edit_in_background(
                    "Invert",
                    move |doc, _| {
                        if let Some(l) = doc.layer_mut(id) {
                            l.pixels.par_update(|_, _, tile| {
                                let max = u16::MAX;
                                tile.map(|t| {
                                    t.iter()
                                        .map(|p| [max - p[0], max - p[1], max - p[2], p[3]])
                                        .collect()
                                })
                            });
                        }
                    },
                    ctx,
                );
            }
        }
        Command::MergeDown => {
            editor.edit_in_background(
                "Merge Down",
                move |doc, active| {
                    if let Some(merged) = ops::merge_down(doc, index) {
                        *active = merged;
                    }
                },
                ctx,
            );
        }
        Command::StampVisible => {
            editor.target = Target::Pixels;
            editor.edit_in_background(
                "Stamp Visible",
                move |doc, active| {
                    let top = doc.layers.len() - 1;
                    *active = ops::stamp_visible(doc, top);
                },
                ctx,
            );
        }
        Command::DodgeAndBurn => {
            editor.target = Target::Pixels;
            editor.edit("Dodge & Burn Layer", |doc, active| {
                *active = ops::dodge_and_burn_layer(doc, index);
            });
        }
        Command::ZoomIn => editor.canvas.step_zoom(true),
        Command::ZoomOut => editor.canvas.step_zoom(false),
        Command::FitOnScreen => editor.canvas.fit(),
        Command::ActualPixels => editor.canvas.actual_pixels(),
        _ => {}
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if self.dialog.is_none() {
            for cmd in Command::pressed(ctx) {
                self.run(cmd, ctx);
            }
            if !ctx.egui_wants_keyboard_input() {
                self.tools.keys(ctx);
            }
        }
        if let Some(editor) = &mut self.editor {
            if !ctx.input(|i| i.pointer.any_down()) {
                editor.end_live();
            }
            editor.update(ctx);
        }
        if let Some((_, _, when)) = &self.status {
            let left = Duration::from_secs(4).saturating_sub(when.elapsed());
            if left.is_zero() {
                self.status = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
        self.run_script(ctx);
        self.update_title(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let bar = egui::Frame::new()
            .fill(self.theme.dark_background)
            .inner_margin(egui::Margin::symmetric(8, 4));
        egui::Panel::top("menu")
            .frame(bar)
            .show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status")
            .frame(bar)
            .show(ui, |ui| self.status_bar(ui));
        if let Some(editor) = &self.editor {
            let target = editor.target;
            egui::Panel::top("options")
                .frame(bar)
                .show(ui, |ui| self.tools.options_bar(ui, target, &self.theme));
            egui::Panel::left("tools")
                .frame(bar)
                .exact_size(44.0)
                .resizable(false)
                .show(ui, |ui| self.tools.toolbar(ui, &self.theme));
        }
        if let Some(editor) = &mut self.editor {
            let mut command = None;
            egui::Panel::right("layers")
                .frame(bar)
                .default_size(280.0)
                .resizable(true)
                .show(ui, |ui| command = self.layers.show(ui, editor, &self.theme));
            if let Some(cmd) = command {
                let ctx = ui.ctx().clone();
                self.run(cmd, &ctx);
            }
        }
        let pasteboard = self.theme.pasteboard();
        let brush = self.tools.settings();
        let source = self.tools.source_marker();
        let mut input = None;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Some(editor) = &mut self.editor {
                let cursor = (editor.busy().is_none()).then_some(brush.size);
                input = editor.canvas.show(ui, pasteboard, cursor, source);
            } else {
                ui.painter().rect_filled(ui.max_rect(), 0.0, pasteboard);
                self.empty_state(ui);
            }
        });
        if let Some(input) = input {
            self.tool_input(input);
        }
        let ctx = ui.ctx().clone();
        self.dialogs(&ctx);
    }
}
