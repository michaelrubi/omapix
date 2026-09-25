use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::{Align, Align2, Button, Layout, Pos2, RichText, Sense, Ui, Vec2, pos2, vec2};
use omapix_engine::adjust::Eyedropper;
use omapix_engine::brush::Paint;
use omapix_engine::clip::{self, Clip};
use omapix_engine::filters::LayerFilter;
use omapix_engine::layer::{Layer, Mask};
use omapix_engine::selection::{Channel, Combine, Selection};
use omapix_engine::tiled::Tiled;
use omapix_engine::{
    Document, NoiseDistribution, NoiseOptions, export, ops, ora,
};

use crate::canvas::ToolInput;
use crate::clipboard::Clipboard;
use crate::commands::Command;
use crate::editor::{Editor, Target, View};
use crate::history_panel::HistoryPanel;
use crate::layers_panel::LayersPanel;
use crate::properties_panel::PropertiesPanel;
use crate::recent::RecentStore;
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
#[derive(Clone)]
enum Then {
    Quit,
    Open,
    OpenFile(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum RightTab {
    #[default]
    Layers,
    History,
}

enum Dialog {
    /// Ask for a radius, then run a filter. For frequency separation,
    /// `preview` shows the texture layer (`Some(true)`), the colour/tone
    /// layer (`Some(false)`) or the image (`None`) while adjusting.
    /// For Gaussian blur, `preview` shows the blurred layer (`Some(true)`)
    /// or the unblurred image (`Some(false)`).
    Radius {
        command: Command,
        radius: f32,
        preview: Option<bool>,
    },
    AddNoise {
        options: NoiseOptions,
        preview: bool,
    },
    /// A Filter menu filter's settings, previewed on the canvas.
    Filter {
        filter: LayerFilter,
        preview: bool,
    },
    UnsavedChanges {
        then: Then,
    },
    BlendingOptions(crate::blending_options::BlendingOptions),
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
/// - `Tool Move|Brush|Eraser|Clone|Heal|SpotHeal|Marquee|Lasso`, `Size n`, `Opacity percent`,
///   `Color r g b` (sRGB), `Source x y` (clone/heal source, like Alt+click),
///   `Look x y` (centre the view on an image point at 100 %),
///   `View image|mask|overlay|texture r|tone r|blur r` (what the canvas shows).
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
    View(View),
    /// Jump to a step in history: `History n`.
    History(usize),
    /// `BlendIf this|under black black_split white_split white` (0–255).
    BlendIf(bool, [f32; 4]),
    /// Magic Wand tolerance (0–255).
    Tolerance(u8),
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
            ("Tolerance", &[n]) => ScriptStep::Tolerance(n as u8),
            ("Color", &[r, g, b]) => ScriptStep::Color([r as u8, g as u8, b as u8]),
            ("Source", &[x, y]) => ScriptStep::Source(egui::pos2(x, y)),
            ("Look", &[x, y]) => ScriptStep::Look(egui::pos2(x, y)),
            ("History", &[n]) => ScriptStep::History(n as usize),
            ("BlendIf", &[a, b, c, d]) => {
                let under = step.split_whitespace().any(|w| w == "under");
                ScriptStep::BlendIf(under, [a / 255.0, b / 255.0, c / 255.0, d / 255.0])
            }
            ("View", nums) => match (words.next()?, nums) {
                ("image", _) => ScriptStep::View(View::Image),
                ("mask", _) => ScriptStep::View(View::Mask(0)),
                ("overlay", _) => ScriptStep::View(View::MaskOverlay(0)),
                ("texture", &[radius]) => ScriptStep::View(View::Separation {
                    radius,
                    texture: true,
                }),
                ("tone", &[radius]) => ScriptStep::View(View::Separation {
                    radius,
                    texture: false,
                }),
                ("blur", &[radius]) => ScriptStep::View(View::Filter {
                    layer: 0,
                    filter: LayerFilter::GaussianBlur { radius },
                    mask: false,
                }),
                ("noise", &[amount]) => ScriptStep::View(View::AddNoise {
                    layer: 0,
                    options: NoiseOptions {
                        amount,
                        ..Default::default()
                    },
                }),
                ("noise", _) => ScriptStep::View(View::AddNoise {
                    layer: 0,
                    options: NoiseOptions::default(),
                }),
                _ => return None,
            },
            ("Tool", _) => match words.next()? {
                "Move" => ScriptStep::Tool(crate::tools::Tool::Move),
                "Brush" => ScriptStep::Tool(crate::tools::Tool::Brush),
                "Eraser" => ScriptStep::Tool(crate::tools::Tool::Eraser),
                "Clone" => ScriptStep::Tool(crate::tools::Tool::CloneStamp),
                "Heal" => ScriptStep::Tool(crate::tools::Tool::Healing),
                "SpotHeal" => ScriptStep::Tool(crate::tools::Tool::SpotHealing),
                "Marquee" => ScriptStep::Tool(crate::tools::Tool::Marquee),
                "EllipticalMarquee" | "Ellipse" => {
                    ScriptStep::Tool(crate::tools::Tool::EllipticalMarquee)
                }
                "Lasso" => ScriptStep::Tool(crate::tools::Tool::Lasso),
                "Wand" | "MagicWand" => ScriptStep::Tool(crate::tools::Tool::MagicWand),
                "Eyedropper" => ScriptStep::Tool(crate::tools::Tool::Eyedropper),
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
    properties: PropertiesPanel,
    history: HistoryPanel,
    recent: RecentStore,
    right_tab: RightTab,
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
    unsharp_mask: LayerFilter,
    feather_radius: f32,
    separation_radius: Option<f32>,
    high_pass_radius: f32,
    noise_options: NoiseOptions,
    /// The user chose to discard changes, so the next close goes through.
    allow_close: bool,
    title: String,
    /// A selection being drawn: its points so far and how it will combine
    /// with the current selection.
    drawing: Option<(Vec<Pos2>, Combine)>,
    /// Where a Move tool drag started, in image pixels.
    move_from: Option<Pos2>,
    /// Steps to run once an image is open, from `OMAPIX_SCRIPT`. For testing
    /// the real UI without a mouse; see [`ScriptStep`].
    script: VecDeque<ScriptStep>,
    clipboard: Clipboard,
    /// An image being read from the system clipboard to paste.
    pasting: Option<Receiver<Result<Clip, String>>>,
    /// Whether V is held, for spotting Ctrl+V (see [`Command::pressed`]).
    v_down: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let ctx = &cc.egui_ctx;
        // Ctrl+= / Ctrl+- zoom the image, not the interface.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        theme::install_font(ctx);
        if let Some(render_state) = &cc.wgpu_render_state {
            crate::gpu::install(render_state);
        }
        let theme = Theme::load();
        ctx.set_visuals(theme.visuals());

        let mut app = Self {
            theme,
            theme_rx: theme::watch(ctx.clone()),
            editor: None,
            layers: LayersPanel::default(),
            properties: PropertiesPanel::default(),
            history: HistoryPanel::default(),
            recent: RecentStore::load(),
            right_tab: RightTab::default(),
            drawing: None,
            move_from: None,
            tools: Tools::default(),
            opening: None,
            picking: None,
            file_job: None,
            dialog: None,
            status: None,
            blur_radius: 2.0,
            unsharp_mask: DEFAULT_UNSHARP_MASK,
            high_pass_radius: 2.0,
            feather_radius: 10.0,
            separation_radius: None,
            noise_options: NoiseOptions::default(),
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
            clipboard: Clipboard::new(std::env::var_os("WAYLAND_DISPLAY").is_some()),
            pasting: None,
            v_down: false,
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
            Then::OpenFile(path) => self.open(path, ctx),
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
                    self.recent.add(&path);
                    if let Some(original) = converted {
                        let now = editor.doc.profile.description().to_owned();
                        self.message(format!("Converted {original} to {now} for editing"), false);
                    }
                    self.editor = Some(editor);
                    self.separation_radius = None;
                }
                Err(err) => {
                    self.recent.remove(&path);
                    self.message(format!("Couldn't open {}: {err}", path.display()), true);
                }
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
                        self.recent.add(&path);
                    }
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let verb = if native { "Saved" } else { "Exported" };
                    self.message(format!("{verb} {name}"), false);
                }
                Err(err) => self.message(format!("{label} failed: {err}"), true),
            }
        }
        if let Some(rx) = &self.pasting
            && self.editor.as_ref().is_some_and(|e| e.busy().is_none())
            && let Ok(result) = rx.try_recv()
        {
            self.pasting = None;
            match result {
                Ok(clip) => {
                    if let Some(editor) = &mut self.editor {
                        paste(editor, Arc::new(clip), ctx);
                    }
                }
                Err(err) => self.message(err, true),
            }
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cube")) {
                if let Some(editor) = &mut self.editor {
                    let mut lut = omapix_engine::adjust::ColorLookup::default();
                    match lut.load_cube_file(&path) {
                        Ok(()) => {
                            let adj = omapix_engine::adjust::Adjustment::ColorLookup(lut);
                            let label = format!("New {} Layer", adj.name());
                            let (w, h) = (editor.doc.width, editor.doc.height);
                            let index = editor.active_index().unwrap_or(0);
                            editor.edit(&label, |doc, active| {
                                let new = doc.next_layer_id();
                                doc.insert_above(index, omapix_engine::Layer::adjustment(new, adj, w, h));
                                *active = new;
                            });
                            editor.target = Target::Mask;
                        }
                        Err(err) => self.message(format!("Failed to load LUT: {err}"), true),
                    }
                }
            } else if self.modified() {
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
            return matches!(cmd, Command::Open | Command::Quit)
                || (cmd == Command::ReopenLast && self.recent.last().is_some());
        };
        let view = matches!(
            cmd,
            Command::ZoomIn | Command::ZoomOut | Command::FitOnScreen | Command::ActualPixels
        );
        if editor.busy().is_some() && !view && !matches!(cmd, Command::Quit) {
            return false;
        }
        let doc = &editor.doc;
        let index = editor.active_index();
        let layer = doc.layer(editor.active);
        let has_mask = layer.is_some_and(|l| l.mask.is_some());
        let is_group = layer.is_some_and(|l| l.is_group);
        // Adjustment layers and groups have no pixels to change.
        let no_pixels = layer.is_some_and(|l| !l.has_pixels());
        match cmd {
            Command::ReopenLast => self.recent.last().is_some(),
            Command::ShowLayers | Command::ShowHistory => true,
            Command::Undo => editor.undo_label().is_some(),
            Command::Redo => editor.redo_label().is_some(),
            // Something must be left.
            Command::DeleteLayer => doc.removed_count(&editor.selected()) < doc.layers.len(),
            Command::MergeDown => {
                is_group
                    || editor.several_selected()
                    || index.is_some_and(|i| ops::can_merge_down(doc, i))
            }
            Command::RaiseLayer => doc.raise_place(editor.active).is_some(),
            Command::LowerLayer => doc.lower_place(editor.active).is_some(),
            Command::BringToFront => doc.front_place(&editor.selected(), editor.active).is_some(),
            Command::SendToBack => doc.back_place(&editor.selected(), editor.active).is_some(),
            Command::SelectLayerAbove => self.layers.row_beside(doc, editor.active, true).is_some(),
            Command::SelectLayerBelow => self.layers.row_beside(doc, editor.active, false).is_some(),
            Command::UngroupLayers => is_group,
            Command::ClippingMask => {
                layer.is_some_and(|l| l.clipped) || index.is_some_and(|i| doc.can_clip(i))
            }
            Command::AddMask => !has_mask,
            Command::LockTransparent => !no_pixels,
            Command::LoadSelectionTransparency => !no_pixels,
            Command::LoadSelectionLayerMask => has_mask,
            Command::Deselect | Command::InvertSelection | Command::Feather => {
                editor.doc.selection.is_some()
            }
            Command::FillForeground
            | Command::FillBackground
            | Command::Clear
            | Command::Cut
            | Command::Copy => editor.target == Target::Mask || !no_pixels,
            Command::Paste => self.pasting.is_none(),
            Command::DeleteMask | Command::ToggleMask | Command::MaskOverlay => has_mask,
            Command::GaussianBlur | Command::HighPass | Command::UnsharpMask => {
                if editor.target == Target::Mask {
                    has_mask
                } else {
                    !no_pixels
                }
            }
            Command::AddNoise => editor.target == Target::Pixels || has_mask,
            Command::Invert => editor.target == Target::Mask || !no_pixels,
            _ => true,
        }
    }

    fn run(&mut self, cmd: Command, ctx: &egui::Context) {
        // Delete with several layers selected deletes them, as in Photoshop.
        let cmd = match (cmd, &self.editor) {
            (Command::Clear, Some(e)) if e.several_selected() => Command::DeleteLayer,
            _ => cmd,
        };
        if !self.enabled(cmd) {
            return;
        }
        match cmd {
            Command::Open => self.guard(Then::Open, ctx),
            Command::ReopenLast => {
                if let Some(path) = self.recent.last().cloned() {
                    self.guard(Then::OpenFile(path), ctx);
                }
            }
            Command::ShowLayers => self.right_tab = RightTab::Layers,
            Command::ShowHistory => self.right_tab = RightTab::History,
            Command::Quit => self.guard(Then::Quit, ctx),
            Command::Save => self.save(ctx),
            Command::SaveAs => self.pick(Purpose::SaveAs, ctx),
            Command::ExportTiff => self.pick(Purpose::ExportTiff, ctx),
            Command::ExportJpeg => self.pick(Purpose::ExportJpeg, ctx),
            // On a mask, noise goes straight into it; on pixels, onto a
            // Grain layer.
            Command::AddNoise if self.editor.as_ref().is_some_and(|e| e.target == Target::Mask) => {
                self.dialog = Some(Dialog::Filter {
                    filter: LayerFilter::AddNoise(self.noise_options),
                    preview: true,
                });
            }
            Command::AddNoise => {
                self.dialog = Some(Dialog::AddNoise {
                    options: self.noise_options,
                    preview: true,
                });
            }
            Command::GaussianBlur | Command::HighPass | Command::UnsharpMask => {
                let filter = match cmd {
                    Command::GaussianBlur => LayerFilter::GaussianBlur {
                        radius: self.blur_radius,
                    },
                    Command::HighPass => LayerFilter::HighPass {
                        radius: self.high_pass_radius,
                    },
                    _ => self.unsharp_mask,
                };
                self.dialog = Some(Dialog::Filter {
                    filter,
                    preview: true,
                });
            }
            Command::Feather => {
                self.dialog = Some(Dialog::Radius {
                    command: cmd,
                    radius: self.feather_radius,
                    preview: None,
                });
            }
            Command::BlendingOptions => {
                if let Some(editor) = &mut self.editor {
                    editor.begin_group("Blending Options");
                    self.dialog = Some(Dialog::BlendingOptions(
                        crate::blending_options::BlendingOptions::new(editor.active),
                    ));
                }
            }
            Command::FillForeground | Command::FillBackground | Command::Clear => {
                let colour = match cmd {
                    Command::FillForeground => Some(self.tools.foreground),
                    Command::FillBackground => Some(self.tools.background),
                    _ => None,
                };
                let background = self.tools.background;
                if let Some(editor) = &mut self.editor {
                    let label = if colour.is_some() { "Fill" } else { "Clear" };
                    fill(editor, label, colour, background);
                }
            }
            Command::Cut | Command::Copy | Command::CopyMerged => {
                let Some(editor) = &mut self.editor else {
                    return;
                };
                let Some(clip) = copy(editor, cmd == Command::CopyMerged) else {
                    self.message("Nothing to copy: the selected area is empty", true);
                    return;
                };
                self.clipboard.set(clip);
                if cmd == Command::Cut {
                    fill(editor, "Cut", None, self.tools.background);
                }
            }
            Command::Paste => {
                let Some(editor) = &mut self.editor else {
                    return;
                };
                match self.clipboard.current() {
                    Some(clip) => paste(editor, clip, ctx),
                    None => {
                        let (tx, rx) = channel();
                        let ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(crate::clipboard::read_system());
                            ctx.request_repaint();
                        });
                        self.pasting = Some(rx);
                    }
                }
            }
            Command::HighPassSharpening => {
                self.dialog = Some(Dialog::Radius {
                    command: cmd,
                    radius: self.high_pass_radius,
                    preview: None,
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
                    preview: Some(true),
                });
            }
            Command::SelectLayerAbove => {
                if let Some(editor) = &mut self.editor {
                    self.layers.step_selection(editor, true);
                }
            }
            Command::SelectLayerBelow => {
                if let Some(editor) = &mut self.editor {
                    self.layers.step_selection(editor, false);
                }
            }
            Command::SelectAll | Command::InvertSelection => {
                if let Some(editor) = &mut self.editor {
                    editor.hide_selection_edges = false;
                    run_on_editor(editor, cmd, ctx);
                }
            }
            Command::SelectionEdges => {
                if let Some(editor) = &mut self.editor {
                    editor.hide_selection_edges = !editor.hide_selection_edges;
                }
            }
            _ => {
                if let Some(editor) = &mut self.editor {
                    run_on_editor(editor, cmd, ctx);
                }
            }
        }
    }

    fn apply_add_noise(&mut self, options: NoiseOptions, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        self.noise_options = options;
        let index = editor.active_index().unwrap_or(0);
        editor.target = Target::Pixels;
        editor.edit_in_background(
            "Add Noise",
            move |doc, active| {
                *active = ops::add_noise_layer(doc, index, &options);
            },
            ctx,
        );
    }

    /// Apply a Filter menu filter to the active layer (within the
    /// selection), remembering its settings for next time.
    fn apply_filter(&mut self, filter: LayerFilter, ctx: &egui::Context) {
        match filter {
            LayerFilter::GaussianBlur { radius } => self.blur_radius = radius,
            LayerFilter::HighPass { radius } => self.high_pass_radius = radius,
            LayerFilter::UnsharpMask { .. } => self.unsharp_mask = filter,
            LayerFilter::AddNoise(options) => self.noise_options = options,
        }
        let Some(editor) = &mut self.editor else {
            return;
        };
        let (id, mask) = (editor.active, editor.target == Target::Mask);
        editor.edit_in_background(
            filter.name(),
            move |doc, _| {
                if mask {
                    if let Some(pixels) = ops::filtered_mask(doc, id, &filter)
                        && let Some(m) = &mut doc.layer_mut(id).expect("just filtered").mask
                    {
                        m.pixels = pixels;
                    }
                } else if let Some(pixels) = ops::filtered(doc, id, &filter) {
                    doc.layer_mut(id).expect("just filtered").pixels = pixels;
                }
            },
            ctx,
        );
    }

    fn apply_radius(&mut self, command: Command, radius: f32, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let index = editor.active_index().unwrap_or(0);
        match command {
            Command::Feather => {
                self.feather_radius = radius;
                editor.hide_selection_edges = false;
                editor.edit_in_background(
                    "Feather",
                    move |doc, _| {
                        doc.selection = doc.selection.as_ref().map(|s| s.feather(radius));
                    },
                    ctx,
                );
            }
            Command::HighPassSharpening => {
                self.high_pass_radius = radius;
                editor.target = Target::Pixels;
                editor.edit_in_background(
                    "High Pass Sharpening",
                    move |doc, active| *active = ops::high_pass_sharpening(doc, index, radius),
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

    fn active_is_group(&self) -> bool {
        self.editor
            .as_ref()
            .and_then(|e| e.doc.layer(e.active))
            .is_some_and(|l| l.is_group)
    }

    fn active_is_locked(&self) -> bool {
        self.editor
            .as_ref()
            .and_then(|e| e.doc.layer(e.active))
            .is_some_and(|l| l.lock_alpha)
    }

    fn active_is_clipped(&self) -> bool {
        self.editor
            .as_ref()
            .and_then(|e| e.doc.layer(e.active))
            .is_some_and(|l| l.clipped)
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
                ui.menu_button("Open Recent", |ui| {
                    if self.recent.files().is_empty() {
                        ui.add_enabled(false, Button::new("No Recent Files"));
                    } else {
                        let mut to_open = None;
                        for path in self.recent.files() {
                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                            let button = Button::new(name.as_ref());
                            let parent = path.parent().map(|p| p.to_string_lossy());
                            let item = if let Some(p) = parent {
                                button.shortcut_text(p)
                            } else {
                                button
                            };
                            if ui.add(item).clicked() {
                                to_open = Some(path.clone());
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear Recent Files").clicked() {
                            self.recent.clear();
                            ui.close();
                        }
                        if let Some(path) = to_open {
                            let ctx = ui.ctx().clone();
                            self.guard(Then::OpenFile(path), &ctx);
                        }
                    }
                });
                self.menu_item(ui, Command::ReopenLast, None);
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
                ui.separator();
                self.menu_item(ui, Command::Cut, None);
                self.menu_item(ui, Command::Copy, None);
                self.menu_item(ui, Command::CopyMerged, None);
                self.menu_item(ui, Command::Paste, None);
                ui.separator();
                self.menu_item(ui, Command::FillForeground, None);
                self.menu_item(ui, Command::FillBackground, None);
                self.menu_item(ui, Command::Clear, None);
            });
            ui.menu_button("Layer", |ui| {
                let several = self.editor.as_ref().is_some_and(|e| e.several_selected());
                let plural = |label: &str| several.then(|| format!("{label}s"));
                self.menu_item(ui, Command::NewLayer, None);
                self.menu_item(ui, Command::DuplicateLayer, plural("Duplicate Layer"));
                self.menu_item(ui, Command::DeleteLayer, plural("Delete Layer"));
                ui.separator();
                self.menu_item(ui, Command::NewGroup, None);
                self.menu_item(ui, Command::GroupLayers, None);
                self.menu_item(ui, Command::UngroupLayers, None);
                ui.separator();
                let release = self.active_is_clipped().then(|| "Release Clipping Mask".to_owned());
                self.menu_item(ui, Command::ClippingMask, release);
                let unlock = self.active_is_locked().then(|| "Unlock Transparent Pixels".to_owned());
                self.menu_item(ui, Command::LockTransparent, unlock);
                ui.separator();
                self.menu_item(ui, Command::BlendingOptions, None);
                ui.separator();
                self.menu_item(ui, Command::AddMask, None);
                self.menu_item(ui, Command::ToggleMask, None);
                self.menu_item(ui, Command::DeleteMask, None);
                ui.separator();
                self.menu_item(ui, Command::BringToFront, None);
                self.menu_item(ui, Command::RaiseLayer, None);
                self.menu_item(ui, Command::LowerLayer, None);
                self.menu_item(ui, Command::SendToBack, None);
                ui.separator();
                let merge = if several {
                    Some("Merge Layers".to_owned())
                } else {
                    self.active_is_group().then(|| "Merge Group".to_owned())
                };
                self.menu_item(ui, Command::MergeDown, merge);
                self.menu_item(ui, Command::StampVisible, None);
            });
            ui.menu_button("Select", |ui| {
                self.menu_item(ui, Command::SelectAll, None);
                self.menu_item(ui, Command::Deselect, None);
                self.menu_item(ui, Command::InvertSelection, None);
                ui.separator();
                self.menu_item(ui, Command::Feather, None);
                ui.separator();
                ui.menu_button("Load Selection", |ui| {
                    self.menu_item(ui, Command::LoadSelectionRed, None);
                    self.menu_item(ui, Command::LoadSelectionGreen, None);
                    self.menu_item(ui, Command::LoadSelectionBlue, None);
                    self.menu_item(ui, Command::LoadSelectionLuminosity, None);
                    ui.separator();
                    self.menu_item(ui, Command::LoadSelectionTransparency, None);
                    self.menu_item(ui, Command::LoadSelectionLayerMask, None);
                });
            });
            ui.menu_button("Image", |ui| {
                ui.menu_button("Adjustments", |ui| {
                    self.menu_item(ui, Command::NewCurves, None);
                    self.menu_item(ui, Command::NewLevels, None);
                    self.menu_item(ui, Command::NewHueSaturation, None);
                    self.menu_item(ui, Command::NewColorBalance, None);
                    self.menu_item(ui, Command::NewSelectiveColor, None);
                    self.menu_item(ui, Command::NewChannelMixer, None);
                    self.menu_item(ui, Command::NewColorLookup, None);
                    ui.separator();
                    self.menu_item(ui, Command::Invert, None);
                });
            });
            ui.menu_button("Filter", |ui| {
                ui.menu_button("Noise", |ui| {
                    self.menu_item(ui, Command::AddNoise, None);
                });
                self.menu_item(ui, Command::GaussianBlur, None);
                ui.menu_button("Sharpen", |ui| {
                    self.menu_item(ui, Command::UnsharpMask, None);
                });
                self.menu_item(ui, Command::HighPass, None);
            });
            ui.menu_button("Retouch", |ui| {
                self.menu_item(ui, Command::FrequencySeparation, None);
                self.menu_item(ui, Command::DodgeAndBurn, None);
                self.menu_item(ui, Command::DodgeAndBurnCurves, None);
                ui.separator();
                self.menu_item(ui, Command::HighPassSharpening, None);
            });
            ui.menu_button("View", |ui| {
                self.menu_item(ui, Command::ZoomIn, None);
                self.menu_item(ui, Command::ZoomOut, None);
                self.menu_item(ui, Command::FitOnScreen, None);
                self.menu_item(ui, Command::ActualPixels, None);
                ui.separator();
                let hidden = self.editor.as_ref().is_some_and(|e| e.hide_selection_edges);
                let edges_label = if hidden {
                    "Show Selection Edges"
                } else {
                    "Hide Selection Edges"
                };
                self.menu_item(ui, Command::SelectionEdges, Some(edges_label.to_string()));
                self.menu_item(ui, Command::MaskOverlay, None);
            });
            ui.menu_button("Window", |ui| {
                let layers_label = if self.right_tab == RightTab::Layers {
                    "✓ Layers"
                } else {
                    "   Layers"
                };
                let history_label = if self.right_tab == RightTab::History {
                    "✓ History"
                } else {
                    "   History"
                };
                self.menu_item(ui, Command::ShowLayers, Some(layers_label.to_string()));
                self.menu_item(ui, Command::ShowHistory, Some(history_label.to_string()));
            });
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let dim = self.theme.dark_foreground;
        ui.horizontal(|ui| {
            if let Some(editor) = &self.editor {
                let doc = &editor.doc;
                let showing = match editor.view() {
                    View::Mask(_) => {
                        Some("Viewing layer mask — Alt+click the mask or press Esc to return")
                    }
                    View::MaskOverlay(_) => Some("Mask overlay on — press \\ or Esc to hide"),
                    _ => None,
                };
                if let Some(text) = showing {
                    ui.label(RichText::new(text).color(self.theme.accent));
                    ui.separator();
                }
                if editor.hide_selection_edges && doc.selection.is_some() {
                    ui.label(
                        RichText::new("Selection edges hidden — press Ctrl+H to show")
                            .color(self.theme.accent),
                    );
                    ui.separator();
                }
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
        if let (Some(Dialog::BlendingOptions(panel)), Some(editor)) =
            (&mut self.dialog, &mut self.editor)
        {
            let closed = match panel.show(ctx, editor, &self.theme) {
                Some(keep) => {
                    editor.end_group(keep);
                    true
                }
                // The layer went away.
                None if editor.doc.layer(panel.layer).is_none() => {
                    editor.end_group(false);
                    true
                }
                None => false,
            };
            if closed {
                self.dialog = None;
            }
            return;
        }
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
        let mut modal = egui::Modal::new(egui::Id::new("dialog"));
        if matches!(dialog, Dialog::Radius { .. }) {
            // Keep the image visible while choosing a radius.
            let area = egui::Modal::default_area(egui::Id::new("dialog"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-320.0, 90.0));
            modal = modal.area(area).backdrop_color(egui::Color32::TRANSPARENT);
        }
        let response = modal.show(ctx, |ui| {
            ui.set_min_width(340.0);
            match dialog {
                Dialog::Radius {
                    command,
                    radius,
                    preview,
                } => {
                    ui.heading(command.label().trim_end_matches('…'));
                    ui.add_space(8.0);
                    if *command == Command::HighPassSharpening {
                        ui.label(
                            RichText::new(
                                "A small radius (1–3 px) sharpens fine detail.\n\
                                 The layer's opacity sets the strength.",
                            )
                            .color(hint),
                        );
                        ui.add_space(8.0);
                    }
                    if *command == Command::FrequencySeparation {
                        ui.label(
                            RichText::new(
                                "Raise the radius until skin blotches vanish from the\n\
                                 color/tone layer and only pores remain in texture.",
                            )
                            .color(hint),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label("Preview");
                            ui.selectable_value(preview, Some(true), "Texture");
                            ui.selectable_value(preview, Some(false), "Color/Tone");
                            ui.selectable_value(preview, None, "Image");
                        });
                        ui.add_space(4.0);
                    }
                    radius_field(ui, radius);
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
                Dialog::Filter { filter, preview } => {
                    ui.heading(filter.name());
                    ui.add_space(8.0);
                    match filter {
                        LayerFilter::GaussianBlur { radius } | LayerFilter::HighPass { radius } => {
                            radius_field(ui, radius);
                        }
                        LayerFilter::UnsharpMask {
                            amount,
                            radius,
                            threshold,
                        } => {
                            let mut percent = *amount * 100.0;
                            ui.horizontal(|ui| {
                                ui.label("Amount");
                                let slider = egui::Slider::new(&mut percent, 1.0..=500.0)
                                    .suffix(" %")
                                    .fixed_decimals(0);
                                ui.add(slider);
                            });
                            *amount = percent / 100.0;
                            radius_field(ui, radius);
                            ui.horizontal(|ui| {
                                ui.label("Threshold");
                                let slider = egui::Slider::new(threshold, 0.0..=255.0)
                                    .suffix(" levels")
                                    .fixed_decimals(0);
                                ui.add(slider);
                            });
                            ui.label(
                                RichText::new("Sharpens luminance only. Judge it at 100 %.")
                                    .color(hint),
                            );
                        }
                        LayerFilter::AddNoise(options) => noise_controls(ui, options),
                    }
                    ui.add_space(4.0);
                    ui.checkbox(preview, "Preview");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ok = ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ok {
                            let filter = *filter;
                            action = Some(Box::new(move |app, ctx| app.apply_filter(filter, ctx)));
                            close = true;
                        }
                    });
                }
                Dialog::AddNoise { options, preview } => {
                    ui.heading("Add Noise");
                    ui.add_space(8.0);
                    noise_controls(ui, options);
                    ui.add_space(8.0);
                    ui.checkbox(preview, "Preview");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ok = ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ok {
                            let options = *options;
                            action = Some(Box::new(move |app, ctx| {
                                app.apply_add_noise(options, ctx);
                            }));
                            close = true;
                        }
                    });
                }
                Dialog::BlendingOptions(_) => {}
                Dialog::UnsavedChanges { then } => {
                    let then = then.clone();
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
        // Show the separation or blur preview while its dialog is open.
        if let Some(editor) = &mut self.editor {
            match &self.dialog {
                Some(Dialog::Radius {
                    command: Command::FrequencySeparation,
                    radius,
                    preview,
                }) => {
                    let view = match preview {
                        Some(texture) => View::Separation {
                            radius: *radius,
                            texture: *texture,
                        },
                        None => View::Image,
                    };
                    editor.set_view(view);
                }
                Some(Dialog::Filter { filter, preview }) => {
                    let view = if *preview {
                        View::Filter {
                            layer: editor.active,
                            filter: *filter,
                            mask: editor.target == Target::Mask,
                        }
                    } else {
                        View::Image
                    };
                    editor.set_view(view);
                }
                Some(Dialog::AddNoise { options, preview }) => {
                    let view = if *preview {
                        View::AddNoise {
                            layer: editor.active,
                            options: *options,
                        }
                    } else {
                        View::Image
                    };
                    editor.set_view(view);
                }
                _ if matches!(editor.view(), View::Separation { .. } | View::Filter { .. } | View::AddNoise { .. }) => {
                    editor.set_view(View::Image)
                }
                _ => {}
            }
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
                let space = if self.recent.files().is_empty() {
                    ui.available_height() * 0.4
                } else {
                    (ui.available_height() * 0.18).max(30.0)
                };
                ui.add_space(space);
                ui.label(RichText::new("Omapix").size(28.0).color(accent));
                ui.add_space(8.0);
                ui.label(RichText::new("Open an image with Ctrl+O, or drop one here").color(hint));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let total_width = if self.recent.last().is_some() { 200.0 } else { 70.0 };
                    let pad = (ui.available_width() - total_width).max(0.0) * 0.5;
                    ui.add_space(pad);
                    if ui.button("Open…").clicked() {
                        self.run(Command::Open, &ctx);
                    }
                    if let Some(last) = self.recent.last() {
                        let name = last.file_name().unwrap_or_default().to_string_lossy();
                        if ui
                            .button(format!("Reopen {name}"))
                            .on_hover_text(last.to_string_lossy())
                            .clicked()
                        {
                            self.run(Command::ReopenLast, &ctx);
                        }
                    }
                });

                if !self.recent.files().is_empty() {
                    ui.add_space(20.0);
                    ui.label(RichText::new("Recent files").strong().color(self.theme.foreground));
                    ui.add_space(6.0);

                    let mut to_open = None;
                    egui::Frame::new()
                        .fill(self.theme.dark_background)
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(450.0);
                            for path in self.recent.files().iter().take(6) {
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                let parent = path
                                    .parent()
                                    .map(|p| p.to_string_lossy().into_owned())
                                    .unwrap_or_default();

                                let (rect, response) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 26.0),
                                    Sense::click(),
                                );
                                if response.hovered() {
                                    ui.painter().rect_filled(rect, 2.0, self.theme.lighter_background);
                                }
                                ui.painter().text(
                                    pos2(rect.left() + 6.0, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    &name,
                                    egui::FontId::proportional(13.0),
                                    self.theme.foreground,
                                );
                                ui.painter().text(
                                    pos2(rect.right() - 6.0, rect.center().y),
                                    Align2::RIGHT_CENTER,
                                    &parent,
                                    egui::FontId::proportional(11.0),
                                    hint,
                                );
                                if response.clicked() {
                                    to_open = Some(path.clone());
                                }
                            }
                        });
                    if let Some(path) = to_open {
                        self.guard(Then::OpenFile(path), &ctx);
                    }
                }
            });
        });
    }

    fn apply_eyedropper(&mut self, eyedropper: Eyedropper, x: u32, y: u32) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let active_id = editor.active;
        let Some(layer) = editor.doc.layer(active_id) else {
            return;
        };
        let Some(mut new_adjustment) = layer.adjustment.clone() else {
            return;
        };
        let Some(sample) = editor.doc.sample_below(active_id, x, y) else {
            return;
        };
        let sample = [0, 1, 2].map(|c| f32::from(sample[c]) / 65535.0);
        match &mut new_adjustment {
            omapix_engine::adjust::Adjustment::Curves(c) => c.set_point(eyedropper, sample),
            omapix_engine::adjust::Adjustment::Levels(l) => l.set_point(eyedropper, sample),
            _ => return,
        }
        editor.edit(eyedropper.label(), |doc, _| {
            if let Some(layer) = doc.layer_mut(active_id) {
                layer.adjustment = Some(new_adjustment);
            }
        });
    }

    /// Esc disarms an eyedropper, or leaves mask view or the mask overlay
    /// (the editor leaves them itself when their mask goes away).
    fn check_escape(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let mask_view = matches!(editor.view(), View::Mask(_) | View::MaskOverlay(_));
        let escape = (mask_view || self.properties.eyedropper.is_some())
            && self.dialog.is_none()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if escape && self.properties.eyedropper.take().is_none() {
            editor.set_view(View::Image);
        }
    }

    fn tool_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        if let ToolInput::BrushDrag { size, hardness } = input {
            self.tools.drag_brush(size, hardness);
            return;
        }
        let Some(editor) = &mut self.editor else {
            return;
        };
        if let Some(eyedropper) = self.properties.eyedropper {
            if let ToolInput::StrokeBegin(p) = input
                && p.x >= 0.0
                && p.y >= 0.0
            {
                self.apply_eyedropper(eyedropper, p.x as u32, p.y as u32);
            }
            return;
        }
        if self.tools.tool.selects() {
            self.selection_input(input, modifiers);
            return;
        }
        if self.tools.tool == crate::tools::Tool::Move {
            self.move_input(input, modifiers);
            return;
        }
        match input {
            ToolInput::StrokeBegin(p) => {
                let Some(mut paint) = self.tools.paint(editor.target, &editor.doc.profile, p)
                else {
                    return;
                };
                // With transparency locked, the eraser paints the background
                // colour, as in Photoshop.
                let locked = editor.doc.layer(editor.active).is_some_and(|l| l.lock_alpha);
                if paint == Paint::Erase && locked && editor.target == Target::Pixels {
                    let background = editor.doc.profile.from_srgb8(self.tools.background);
                    paint = Paint::Color(background.unwrap_or([65535; 4]));
                }
                let (settings, sample_all) = (self.tools.settings(), self.tools.sample_all);
                if editor.begin_stroke(settings, paint, sample_all) {
                    editor.stroke_to(p.x, p.y);
                }
            }
            ToolInput::StrokeMove(p) => editor.stroke_to(p.x, p.y),
            ToolInput::StrokeEnd => editor.end_stroke(),
            // Handled before the tools.
            ToolInput::BrushDrag { .. } => {}
            ToolInput::Sample(p) if self.tools.tool.copies() => self.tools.set_source(p),
            ToolInput::Sample(p) => {
                if p.x >= 0.0
                    && p.y >= 0.0
                    && let Some(pixel) = editor.sample(
                        p.x as u32,
                        p.y as u32,
                        self.tools.sample_size,
                        self.tools.sample_all,
                    )
                {
                    let is_bg = self.tools.tool == crate::tools::Tool::Eyedropper && modifiers.alt;
                    self.tools.sample(pixel, &editor.doc.profile, is_bg);
                }
            }
        }
    }

    /// Dragging with the Move tool. Alt moves a copy; Shift keeps the move
    /// horizontal, vertical or diagonal.
    fn move_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        let background = crate::tools::grey(self.tools.background);
        let Some(editor) = &mut self.editor else {
            return;
        };
        match input {
            ToolInput::StrokeBegin(p) => {
                if editor.begin_move("Move", modifiers.alt, background) {
                    self.move_from = Some(p);
                }
            }
            ToolInput::StrokeMove(p) => {
                let Some(from) = self.move_from else {
                    return;
                };
                let d = p - from;
                let d = if modifiers.shift { constrain_45(d) } else { d };
                editor.move_to(d.x.round() as i32, d.y.round() as i32);
            }
            ToolInput::StrokeEnd => {
                self.move_from = None;
                editor.end_move();
            }
            ToolInput::Sample(_) | ToolInput::BrushDrag { .. } => {}
        }
    }

    /// Drawing a selection with the marquee or lasso.
    fn selection_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        match input {
            ToolInput::StrokeBegin(p) => {
                let how = Combine::from_modifiers(modifiers.shift, modifiers.alt);
                self.drawing = Some((vec![p], how));
            }
            ToolInput::StrokeMove(p) => {
                if let Some((points, _)) = &mut self.drawing {
                    match self.tools.tool {
                        crate::tools::Tool::Marquee | crate::tools::Tool::EllipticalMarquee => {
                            points.truncate(1);
                            points.push(p);
                        }
                        crate::tools::Tool::MagicWand => {}
                        _ => {
                            // Skip points closer than a pixel to the last.
                            if points.last().is_none_or(|l| l.distance(p) >= 1.0) {
                                points.push(p);
                            }
                        }
                    }
                }
            }
            ToolInput::StrokeEnd => {
                let Some((points, how)) = self.drawing.take() else {
                    return;
                };
                let (w, h) = (editor.doc.width, editor.doc.height);
                if self.tools.tool == crate::tools::Tool::MagicWand {
                    let p = points[0];
                    if p.x < 0.0 || p.y < 0.0 || p.x >= w as f32 || p.y >= h as f32 {
                        if how == Combine::Replace && editor.doc.selection.is_some() {
                            editor.edit("Deselect", |doc, _| doc.selection = None);
                        }
                        return;
                    }
                    if editor.magic_wand(
                        (p.x as u32, p.y as u32),
                        omapix_engine::raster::widen(self.tools.wand_tolerance),
                        self.tools.wand_contiguous,
                        self.tools.wand_anti_alias,
                        self.tools.sample_all,
                        how,
                    ) {
                        editor.hide_selection_edges = false;
                    }
                    return;
                }
                // A click without a real drag.
                let tiny = points.iter().all(|q| q.distance(points[0]) < 2.0);
                let p1 = if (self.tools.tool == crate::tools::Tool::Marquee
                    || self.tools.tool == crate::tools::Tool::EllipticalMarquee)
                    && modifiers.shift
                    && points.len() >= 2
                {
                    constrain_square(points[0], points[1])
                } else if points.len() >= 2 {
                    points[1]
                } else {
                    points[0]
                };
                let shape = if tiny {
                    None
                } else if self.tools.tool == crate::tools::Tool::Marquee {
                    Some(Selection::rectangle(
                        w,
                        h,
                        (points[0].x, points[0].y),
                        (p1.x, p1.y),
                    ))
                } else if self.tools.tool == crate::tools::Tool::EllipticalMarquee {
                    Some(Selection::ellipse(
                        w,
                        h,
                        (points[0].x, points[0].y),
                        (p1.x, p1.y),
                    ))
                } else {
                    let pts: Vec<(f32, f32)> = points.iter().map(|p| (p.x, p.y)).collect();
                    Some(Selection::polygon(w, h, &pts))
                };
                let label = match self.tools.tool {
                    crate::tools::Tool::Marquee => "Rectangular Marquee",
                    crate::tools::Tool::EllipticalMarquee => "Elliptical Marquee",
                    _ => "Lasso",
                };
                match (shape, how) {
                    // A click without dragging deselects, as in Photoshop.
                    (None, Combine::Replace) => {
                        if editor.doc.selection.is_some() {
                            editor.edit("Deselect", |doc, _| doc.selection = None);
                        }
                    }
                    (None, _) => {}
                    (Some(shape), how) => editor.set_selection(label, shape, how),
                }
            }
            ToolInput::Sample(_) | ToolInput::BrushDrag { .. } => {}
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
                if let Some(Dialog::Radius {
                    command, radius, ..
                }) = self.dialog.take()
                {
                    self.apply_radius(command, radius, ctx);
                }
                match self.dialog.take() {
                    Some(Dialog::AddNoise { options, .. }) => self.apply_add_noise(options, ctx),
                    Some(Dialog::Filter { filter, .. }) => self.apply_filter(filter, ctx),
                    dialog => self.dialog = dialog,
                }
            }
            ScriptStep::Stroke([x0, y0, x1, y1]) => {
                self.tool_input(
                    ToolInput::StrokeBegin(egui::pos2(x0, y0)),
                    egui::Modifiers::NONE,
                );
                // Several moves, like a real drag across frames.
                for i in 1..=20 {
                    let t = i as f32 / 20.0;
                    let p = egui::pos2(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                    self.tool_input(ToolInput::StrokeMove(p), egui::Modifiers::NONE);
                }
                self.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
            }
            ScriptStep::Tool(tool) => self.tools.select(tool),
            ScriptStep::Size(n) => self.tools.set_size(n),
            ScriptStep::Opacity(o) => self.tools.set_opacity(o),
            ScriptStep::Tolerance(t) => self.tools.wand_tolerance = t,
            ScriptStep::Color(c) => self.tools.foreground = c,
            ScriptStep::Source(p) => self.tools.set_source(p),
            ScriptStep::BlendIf(under, range) => {
                if let Some(editor) = &mut self.editor {
                    let id = editor.active;
                    editor.edit("Blending Options", |doc, _| {
                        if let Some(layer) = doc.layer_mut(id) {
                            let mut b = layer.blend_if.unwrap_or_default();
                            if under {
                                b.underlying = range;
                            } else {
                                b.this = range;
                            }
                            layer.blend_if = Some(b);
                        }
                    });
                }
            }
            ScriptStep::View(view) => {
                if let Some(editor) = &mut self.editor {
                    // "mask", "overlay", "blur" and "noise" apply to the active layer.
                    let view = match view {
                        View::Mask(_) => View::Mask(editor.active),
                        View::MaskOverlay(_) => View::MaskOverlay(editor.active),
                        View::Filter { filter, .. } => View::Filter {
                            layer: editor.active,
                            filter,
                            mask: editor.target == Target::Mask,
                        },
                        View::AddNoise { options, .. } => View::AddNoise {
                            layer: editor.active,
                            options,
                        },
                        view => view,
                    };
                    editor.set_view(view);
                }
            }
            ScriptStep::History(step) => {
                if let Some(editor) = &mut self.editor {
                    editor.jump_to_history(step);
                }
            }
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

/// Unsharp Mask's settings until it's first used: a moderate sharpening
/// for a 24 MP portrait.
const DEFAULT_UNSHARP_MASK: LayerFilter = LayerFilter::UnsharpMask {
    amount: 0.8,
    radius: 1.5,
    threshold: 2.0,
};

/// Add Noise's settings, for its dialog and for noise on a mask.
fn noise_controls(ui: &mut Ui, options: &mut NoiseOptions) {
    ui.horizontal(|ui| {
        ui.label("Amount");
        ui.add(
            egui::Slider::new(&mut options.amount, 0.0..=100.0)
                .suffix(" %")
                .fixed_decimals(1),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Distribution:");
        ui.radio_value(&mut options.distribution, NoiseDistribution::Uniform, "Uniform");
        ui.radio_value(&mut options.distribution, NoiseDistribution::Gaussian, "Gaussian");
    });
    ui.add_space(4.0);
    ui.checkbox(&mut options.monochromatic, "Monochromatic");
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Grain Size");
        ui.add(
            egui::Slider::new(&mut options.grain_size, 1.0..=20.0)
                .suffix(" px")
                .fixed_decimals(1),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Roughness");
        ui.add(
            egui::Slider::new(&mut options.roughness, 0.0..=1.0)
                .fixed_decimals(2),
        );
    });
    ui.add_space(4.0);
    ui.checkbox(&mut options.tonal_falloff, "Shadow/highlight falloff");
}

/// A radius in pixels, for filter and radius dialogs.
fn radius_field(ui: &mut Ui, radius: &mut f32) {
    ui.horizontal(|ui| {
        ui.label("Radius");
        let value = egui::DragValue::new(radius)
            .range(0.1..=250.0)
            .speed(0.1)
            .suffix(" px")
            .fixed_decimals(1);
        ui.add(value);
    });
}

/// Fill the active layer (or its mask) where selected, like Photoshop's
/// Alt+Backspace, as an undo step called `label`. `None` clears instead
/// (Delete): pixels to transparency, masks to the background colour's grey,
/// as Photoshop does.
fn fill(editor: &mut Editor, label: &str, colour: Option<[u8; 3]>, background: [u8; 3]) {
    let id = editor.active;
    let target = editor.target;
    let profile = editor.doc.profile.clone();
    editor.edit(label, |doc, _| {
        let selection = doc.selection.clone();
        let Some(layer) = doc.layer_mut(id) else {
            return;
        };
        match target {
            Target::Mask => {
                if let Some(mask) = layer.mask.as_mut() {
                    let grey = crate::tools::grey(colour.unwrap_or(background));
                    mask.pixels = ops::fill_mask(&mask.pixels, grey, selection.as_ref());
                }
            }
            Target::Pixels => {
                // With transparency locked, Delete fills with the background
                // colour, and only the colour of what's there changes.
                let colour = colour.or(layer.lock_alpha.then_some(background));
                let pixel =
                    colour.map(|rgb| profile.from_srgb8(rgb).unwrap_or([0, 0, 0, u16::MAX]));
                let filled = ops::fill_pixels(&layer.pixels, pixel, selection.as_ref());
                layer.pixels = if layer.lock_alpha {
                    ops::keep_alpha(&layer.pixels, filled)
                } else {
                    filled
                };
            }
        }
    });
}

/// Copy the selected part of the active layer (or its mask), or with
/// `merged` of the whole visible image. `None` if that's empty.
fn copy(editor: &Editor, merged: bool) -> Option<Clip> {
    let doc = &editor.doc;
    let selection = doc.selection.as_ref();
    let clip = if merged {
        Clip::copy(
            &Tiled::from_raster(&doc.composite()),
            selection,
            &doc.profile,
        )
    } else {
        let layer = doc.layer(editor.active)?;
        match (editor.target, &layer.mask) {
            (Target::Mask, Some(mask)) => Clip::copy_mask(&mask.pixels, selection, &doc.profile),
            _ => Clip::copy(&layer.pixels, selection, &doc.profile),
        }
    }?;
    (!clip.is_empty()).then_some(clip)
}

/// Paste `clip` as a new layer above the active one.
fn paste(editor: &mut Editor, clip: Arc<Clip>, ctx: &egui::Context) {
    let index = editor.active_index().unwrap_or(0);
    editor.target = Target::Pixels;
    editor.edit_in_background(
        "Paste",
        move |doc, active| match clip::paste(doc, &clip, index) {
            Ok(id) => *active = id,
            Err(e) => log::error!("paste failed: {e}"),
        },
        ctx,
    );
}

/// Commands that only change the open document.
fn run_on_editor(editor: &mut Editor, cmd: Command, ctx: &egui::Context) {
    let index = editor.active_index().unwrap_or(0);
    let id = editor.active;
    let is_group = editor.doc.layer(id).is_some_and(|l| l.is_group);
    let (w, h) = (editor.doc.width, editor.doc.height);
    let selected = editor.selected();
    let several = editor.several_selected();
    match cmd {
        Command::Undo => editor.undo(),
        Command::Redo => editor.redo(),
        Command::NewLayer => {
            editor.target = Target::Pixels;
            editor.edit("New Layer", |doc, active| {
                let new = doc.next_layer_id();
                let name = doc.unused_name("Layer");
                doc.insert_above(index, Layer::empty(new, name, w, h));
                *active = new;
            });
        }
        Command::DuplicateLayer if several => {
            // The copies are selected, the active layer's copy (or the top
            // one) active.
            let mut copies = Vec::new();
            let roots = editor.doc.outermost(&selected);
            editor.edit("Duplicate Layers", |doc, _| copies = doc.duplicate_layers(&roots));
            if let Some(&top) = copies.last() {
                let at = roots.iter().position(|&r| r == id);
                editor.select_layers(at.map_or(top, |i| copies[i]), copies);
            }
        }
        Command::DuplicateLayer => {
            let label = if is_group { "Duplicate Group" } else { "Duplicate Layer" };
            editor.edit(label, |doc, active| *active = doc.duplicate_layer(index));
        }
        Command::DeleteLayer => {
            let label = if several { "Delete Layers" } else { "Delete Layer" };
            editor.edit(label, |doc, active| {
                let below = doc.remove_layers(&selected).unwrap_or(0);
                *active = doc.layers[below.min(doc.layers.len() - 1)].id;
            });
            editor.fix_selection();
        }
        Command::RaiseLayer | Command::LowerLayer => {
            let (label, place) = if cmd == Command::RaiseLayer {
                ("Bring Forward", editor.doc.raise_place(id))
            } else {
                ("Send Backward", editor.doc.lower_place(id))
            };
            if let Some(place) = place {
                editor.edit(label, |doc, _| {
                    doc.move_layer(id, place);
                });
            }
        }
        Command::LockTransparent => {
            let lock = !editor.doc.layer(id).is_some_and(|l| l.lock_alpha);
            let label = if lock {
                "Lock Transparent Pixels"
            } else {
                "Unlock Transparent Pixels"
            };
            editor.edit(label, |doc, _| {
                for &s in &selected {
                    if let Some(l) = doc.layer_mut(s).filter(|l| l.has_pixels()) {
                        l.lock_alpha = lock;
                    }
                }
            });
        }
        Command::BringToFront | Command::SendToBack => {
            let (label, place) = if cmd == Command::BringToFront {
                ("Bring to Front", editor.doc.front_place(&selected, id))
            } else {
                ("Send to Back", editor.doc.back_place(&selected, id))
            };
            if let Some(place) = place {
                editor.edit(label, |doc, _| {
                    doc.move_layers(&selected, place);
                });
            }
        }
        Command::NewGroup => {
            editor.edit("New Group", |doc, active| *active = doc.new_group(index));
            editor.fix_selection();
        }
        Command::GroupLayers => {
            editor.edit("Group Layers", |doc, active| {
                if let Some(group) = doc.group_layers(&selected) {
                    *active = group;
                }
            });
            editor.fix_selection();
        }
        Command::UngroupLayers => {
            editor.edit("Ungroup Layers", |doc, active| {
                if let Some(top) = doc.ungroup(index) {
                    *active = top;
                }
            });
            editor.fix_selection();
        }
        Command::ClippingMask => {
            let clipped = editor.doc.layer(id).is_some_and(|l| l.clipped);
            let label = if clipped {
                "Release Clipping Mask"
            } else {
                "Create Clipping Mask"
            };
            editor.edit(label, |doc, _| {
                doc.toggle_clipping(id);
            });
        }
        Command::AddMask => {
            // With a selection, the mask reveals just the selection.
            editor.edit("Add Layer Mask", |doc, _| {
                let mask = match &doc.selection {
                    Some(sel) => Mask {
                        pixels: sel.coverage.clone(),
                        enabled: true,
                    },
                    None => Mask::white(w, h),
                };
                if let Some(l) = doc.layer_mut(id) {
                    l.mask = Some(mask);
                }
            });
            editor.target = Target::Mask;
        }
        Command::DeleteMask => {
            if editor.view() == View::Mask(id) {
                editor.set_view(View::Image);
            }
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
                        let selection = doc.selection.clone();
                        if let Some(l) = doc.layer_mut(id) {
                            let original = l.pixels.clone();
                            l.pixels.par_update(|_, _, tile| {
                                let max = u16::MAX;
                                tile.map(|t| {
                                    t.iter()
                                        .map(|p| [max - p[0], max - p[1], max - p[2], p[3]])
                                        .collect()
                                })
                            });
                            l.pixels = ops::within_selection(
                                &original,
                                l.pixels.clone(),
                                selection.as_ref(),
                            );
                        }
                    },
                    ctx,
                );
            }
        }
        Command::MergeDown if several => {
            editor.target = Target::Pixels;
            editor.edit_in_background(
                "Merge Layers",
                move |doc, active| {
                    if let Some(merged) = ops::merge_layers(doc, &selected) {
                        *active = merged;
                    }
                },
                ctx,
            );
        }
        Command::MergeDown => {
            editor.target = Target::Pixels;
            editor.edit_in_background(
                if is_group { "Merge Group" } else { "Merge Down" },
                move |doc, active| {
                    let merged = if is_group {
                        ops::merge_group(doc, index)
                    } else {
                        ops::merge_down(doc, index)
                    };
                    if let Some(merged) = merged {
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
                move |doc, active| *active = ops::stamp_visible(doc),
                ctx,
            );
        }
        Command::DodgeAndBurn => {
            editor.target = Target::Pixels;
            editor.edit("Dodge & Burn Layer", |doc, active| {
                *active = ops::dodge_and_burn_layer(doc, index);
            });
        }
        Command::DodgeAndBurnCurves => {
            // Ready to paint the Dodge layer's mask.
            editor.target = Target::Mask;
            editor.edit("Dodge & Burn Curves", |doc, active| {
                *active = ops::dodge_and_burn_curves(doc, index).0;
            });
        }
        Command::NewCurves
        | Command::NewLevels
        | Command::NewHueSaturation
        | Command::NewColorBalance
        | Command::NewSelectiveColor
        | Command::NewChannelMixer
        | Command::NewColorLookup => {
            use omapix_engine::adjust::{
                Adjustment, ChannelMixer, ColorBalance, ColorLookup, Curves, HueSaturation, Levels,
                SelectiveColor,
            };
            let adjustment = match cmd {
                Command::NewCurves => Adjustment::Curves(Curves::default()),
                Command::NewLevels => Adjustment::Levels(Levels::default()),
                Command::NewHueSaturation => Adjustment::HueSaturation(HueSaturation::default()),
                Command::NewColorBalance => Adjustment::ColorBalance(ColorBalance::default()),
                Command::NewSelectiveColor => Adjustment::SelectiveColor(SelectiveColor::default()),
                Command::NewChannelMixer => Adjustment::ChannelMixer(ChannelMixer::default()),
                _ => Adjustment::ColorLookup(ColorLookup::default()),
            };
            let label = format!("New {} Layer", adjustment.name());
            editor.edit(&label, |doc, active| {
                let new = doc.next_layer_id();
                doc.insert_above(index, Layer::adjustment(new, adjustment, w, h));
                *active = new;
            });
            // Painting on an adjustment layer paints its mask.
            editor.target = Target::Mask;
        }
        Command::SelectAll => {
            editor.edit("Select All", |doc, _| {
                doc.selection = Some(Selection::all(w, h))
            });
        }
        Command::Deselect => {
            editor.edit("Deselect", |doc, _| doc.selection = None);
        }
        Command::InvertSelection => {
            editor.edit("Inverse", |doc, _| {
                let inverted = doc.selection.as_ref().map(Selection::invert);
                doc.selection = inverted.filter(|s| !s.is_empty());
            });
        }
        Command::LoadSelectionRed
        | Command::LoadSelectionGreen
        | Command::LoadSelectionBlue
        | Command::LoadSelectionLuminosity => {
            let channel = match cmd {
                Command::LoadSelectionRed => Channel::Red,
                Command::LoadSelectionGreen => Channel::Green,
                Command::LoadSelectionBlue => Channel::Blue,
                _ => Channel::Luminosity,
            };
            let composite = editor.doc.composite();
            editor.set_selection("Load Selection", Selection::from_channel(&composite, channel), Combine::Replace);
        }
        Command::LoadSelectionTransparency => {
            if let Some(layer) = editor.doc.layer(editor.active)
                && layer.has_pixels()
            {
                editor.set_selection("Load Selection", Selection::from_alpha(&layer.pixels), Combine::Replace);
            }
        }
        Command::LoadSelectionLayerMask => {
            if let Some(mask) = editor.doc.layer(editor.active).and_then(|l| l.mask.as_ref()) {
                editor.set_selection("Load Selection", Selection::from_mask(&mask.pixels), Combine::Replace);
            }
        }
        Command::ZoomIn => editor.canvas.step_zoom(true),
        Command::ZoomOut => editor.canvas.step_zoom(false),
        Command::FitOnScreen => editor.canvas.fit(),
        Command::ActualPixels => editor.canvas.actual_pixels(),
        Command::MaskOverlay => {
            let showing = editor.view() == View::MaskOverlay(id);
            editor.set_view(if showing {
                View::Image
            } else {
                View::MaskOverlay(id)
            });
        }
        _ => {}
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        // While a text field (layer rename) has focus, keys edit the text.
        if self.dialog.is_none() && !ctx.egui_wants_keyboard_input() {
            for cmd in Command::pressed(ctx, &mut self.v_down) {
                self.run(cmd, ctx);
            }
            if let Some(opacity) = self.tools.keys(ctx)
                && let Some(editor) = &mut self.editor
            {
                let id = editor.active;
                editor.edit("Opacity", |doc, _| {
                    if let Some(l) = doc.layer_mut(id) {
                        l.opacity = opacity;
                    }
                });
            }
            if let Some((dx, dy)) = self.tools.nudge(ctx) {
                let background = crate::tools::grey(self.tools.background);
                if let Some(editor) = &mut self.editor
                    && editor.begin_move("Nudge", false, background)
                {
                    editor.move_to(dx, dy);
                    editor.end_move();
                }
            }
        }
        self.check_escape(ctx);
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
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let layers_active = self.right_tab == RightTab::Layers;
                        let history_active = self.right_tab == RightTab::History;

                        let tab_btn = |ui: &mut Ui, label: &str, active: bool| {
                            let text = if active {
                                RichText::new(label).color(self.theme.foreground).strong()
                            } else {
                                RichText::new(label).color(self.theme.dark_foreground)
                            };
                            ui.add(Button::new(text).frame(false))
                        };

                        if tab_btn(ui, "Layers", layers_active).clicked() {
                            self.right_tab = RightTab::Layers;
                        }
                        ui.add_space(8.0);
                        if tab_btn(ui, "History", history_active).clicked() {
                            self.right_tab = RightTab::History;
                        }
                    });
                    ui.separator();

                    match self.right_tab {
                        RightTab::Layers => {
                            self.properties.show(ui, editor, &self.theme);
                            command = self.layers.show(ui, editor, &self.theme);
                        }
                        RightTab::History => {
                            command = self.history.show(ui, editor, &self.theme);
                        }
                    }
                });
            if let Some(cmd) = command {
                let ctx = ui.ctx().clone();
                self.run(cmd, &ctx);
            }
        }
        let pasteboard = self.theme.pasteboard();
        let brush = self.tools.settings();
        let source = self.tools.source_marker();
        let tool = self.tools.tool;
        let shift = ui.input(|i| i.modifiers.shift);
        let drawing: Option<Vec<Pos2>> = self.drawing.as_ref().map(|(points, _)| match tool {
            // Show the marquee as its rectangle (constrained to square with Shift).
            crate::tools::Tool::Marquee if points.len() == 2 => {
                let a = points[0];
                let b = if shift {
                    constrain_square(a, points[1])
                } else {
                    points[1]
                };
                vec![a, egui::pos2(b.x, a.y), b, egui::pos2(a.x, b.y), a]
            }
            // Show the elliptical marquee as an ellipse (circle with Shift).
            crate::tools::Tool::EllipticalMarquee if points.len() == 2 => {
                let a = points[0];
                let b = if shift {
                    constrain_square(a, points[1])
                } else {
                    points[1]
                };
                ellipse_points(a, b)
            }
            _ => points.clone(),
        });
        let mut input = None;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Some(editor) = &mut self.editor {
                let idle = editor.busy().is_none();
                let outlines = if editor.hide_selection_edges {
                    &[][..]
                } else {
                    editor
                        .doc
                        .selection
                        .as_ref()
                        .map_or(&[][..], |s| &s.outlines[..])
                };
                let modifiers = ui.input(|i| i.modifiers);
                let eyedropper_armed = self.properties.eyedropper.is_some();
                let overlay = crate::canvas::Overlay {
                    tool: idle,
                    alt_samples: !eyedropper_armed && tool.paints(),
                    samples: !eyedropper_armed && tool == crate::tools::Tool::Eyedropper,
                    brush: (!eyedropper_armed && tool.paints()).then_some(brush.size),
                    moves: !eyedropper_armed && tool == crate::tools::Tool::Move,
                    source,
                    selection: outlines,
                    drawing: (!eyedropper_armed).then_some(drawing.as_deref()).flatten(),
                    badge: (idle && !eyedropper_armed)
                        .then(|| crate::tools::cursor_badge(tool, modifiers))
                        .flatten(),
                };
                input = editor.canvas.show(ui, pasteboard, overlay);
            } else {
                ui.painter().rect_filled(ui.max_rect(), 0.0, pasteboard);
                self.empty_state(ui);
            }
        });
        if let Some(input) = input {
            let modifiers = ui.input(|i| i.modifiers);
            self.tool_input(input, modifiers);
        }
        let ctx = ui.ctx().clone();
        self.dialogs(&ctx);
    }
}

/// Constrain a rectangular or elliptical drag from `p0` to `p1` to 1:1 aspect ratio.
fn constrain_square(p0: Pos2, p1: Pos2) -> Pos2 {
    let dx = p1.x - p0.x;
    let dy = p1.y - p0.y;
    let side = dx.abs().max(dy.abs());
    let sx = if dx >= 0.0 { 1.0 } else { -1.0 };
    let sy = if dy >= 0.0 { 1.0 } else { -1.0 };
    egui::pos2(p0.x + side * sx, p0.y + side * sy)
}

/// Snap a move to the nearest multiple of 45°, as Shift does in Photoshop.
fn constrain_45(d: Vec2) -> Vec2 {
    let (ax, ay) = (d.x.abs(), d.y.abs());
    // tan 22.5°: closer to an axis than to a diagonal.
    let near = std::f32::consts::FRAC_PI_8.tan();
    if ay <= ax * near {
        egui::vec2(d.x, 0.0)
    } else if ax <= ay * near {
        egui::vec2(0.0, d.y)
    } else {
        let m = (ax + ay) * 0.5;
        egui::vec2(m * d.x.signum(), m * d.y.signum())
    }
}

/// Outline points for an ellipse bounded by `a` and `b`.
fn ellipse_points(a: Pos2, b: Pos2) -> Vec<Pos2> {
    let (l, r) = (a.x.min(b.x), a.x.max(b.x));
    let (t, b_y) = (a.y.min(b.y), a.y.max(b.y));
    let rx = (r - l) * 0.5;
    let ry = (b_y - t) * 0.5;
    if rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
    let cx = l + rx;
    let cy = t + ry;
    let n = ((rx + ry) * 0.5).clamp(32.0, 128.0) as usize;
    let mut pts: Vec<Pos2> = (0..n)
        .map(|i| {
            let angle = i as f32 * std::f32::consts::TAU / n as f32;
            egui::pos2(cx + rx * angle.cos(), cy + ry * angle.sin())
        })
        .collect();
    if let Some(&first) = pts.first() {
        pts.push(first);
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::render_view;

    fn editor_with_selection() -> Editor {
        let (w, h) = (600, 400);
        let image =
            omapix_engine::Raster::new(w, h, vec![[30000, 30000, 30000, 65535]; (w * h) as usize]);
        let doc = Document::from_image(
            "t.tif".into(),
            &image,
            omapix_engine::ColorProfile::srgb(),
            16,
        );
        let mut editor = Editor::new(doc).unwrap();
        editor.edit("Rectangular Marquee", |doc, _| {
            doc.selection = Some(Selection::rectangle(w, h, (100.0, 100.0), (200.0, 200.0)))
        });
        editor
    }

    #[test]
    fn cut_and_paste_moves_the_selection_to_a_new_layer_in_place() {
        let ctx = egui::Context::default();
        let mut editor = editor_with_selection();
        let clip = copy(&editor, false).unwrap();
        fill(&mut editor, "Cut", None, [255, 255, 255]);
        assert_eq!(editor.doc.layers[0].pixels.get(150, 150)[3], 0);
        assert_eq!(editor.doc.layers[0].pixels.get(250, 150)[3], 65535);

        paste(&mut editor, Arc::new(clip), &ctx);
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Paste"));
        assert_eq!(editor.doc.layers.len(), 2);
        let pasted = &editor.doc.layers[1];
        assert_eq!(editor.active, pasted.id);
        assert_eq!(pasted.pixels.get(150, 150), [30000, 30000, 30000, 65535]);
        assert_eq!(pasted.pixels.get(250, 150)[3], 0);
        assert!(editor.doc.selection.is_none());
        // Together they look as they did before the cut.
        assert_eq!(
            editor.doc.composite().get(150, 150),
            [30000, 30000, 30000, 65535]
        );
    }

    #[test]
    fn cutting_from_a_mask_copies_grey_and_clears_to_the_background() {
        let mut editor = editor_with_selection();
        editor.edit("Add Layer Mask", |doc, _| {
            doc.layers[0].mask = Some(Mask::white(600, 400))
        });
        editor.target = Target::Mask;
        let clip = copy(&editor, false).unwrap();
        assert_eq!(clip.pixels.get(150, 150), [65535; 4]);
        fill(&mut editor, "Cut", None, [0, 0, 0]);
        let mask = &editor.doc.layers[0].mask.as_ref().unwrap().pixels;
        assert_eq!((mask.get(150, 150), mask.get(250, 150)), (0, 65535));
        // Copy Merged copies what's visible, which the mask now hides.
        assert!(copy(&editor, true).is_none());
    }

    #[test]
    fn locked_transparency_keeps_alpha_for_fills_and_strokes() {
        let ctx = egui::Context::default();
        let mut editor = editor_with_selection();
        let background = editor.active;
        run_on_editor(&mut editor, Command::NewLayer, &ctx);
        let empty = editor.active;
        run_on_editor(&mut editor, Command::LockTransparent, &ctx);
        assert!(editor.doc.layer(empty).unwrap().lock_alpha);
        assert_eq!(editor.undo_label(), Some("Lock Transparent Pixels"));

        // Nothing there to fill or paint on.
        fill(&mut editor, "Fill", Some([255, 0, 0]), [255, 255, 255]);
        let settings = omapix_engine::brush::BrushSettings::default();
        assert!(editor.begin_stroke(settings, Paint::Color([0, 0, 0, 65535]), false));
        editor.stroke_to(300.0, 300.0);
        editor.end_stroke();
        let pixels = &editor.doc.layer(empty).unwrap().pixels;
        assert_eq!((pixels.get(150, 150)[3], pixels.get(300, 300)[3]), (0, 0));

        // On an opaque layer, Delete fills with the background colour.
        editor.select_layers(background, Vec::new());
        run_on_editor(&mut editor, Command::LockTransparent, &ctx);
        fill(&mut editor, "Clear", None, [255, 255, 255]);
        let pixels = &editor.doc.layer(background).unwrap().pixels;
        assert_eq!(pixels.get(150, 150), [65535; 4]);
        assert_eq!(pixels.get(250, 150), [30000, 30000, 30000, 65535]);

        run_on_editor(&mut editor, Command::LockTransparent, &ctx);
        assert_eq!(editor.undo_label(), Some("Unlock Transparent Pixels"));
        assert!(!editor.doc.layer(background).unwrap().lock_alpha);
    }

    #[test]
    fn constrain_square_makes_1_to_1() {
        let p0 = egui::pos2(100.0, 100.0);
        let p1 = egui::pos2(150.0, 120.0);
        let c = constrain_square(p0, p1);
        assert_eq!(c, egui::pos2(150.0, 150.0));

        let p1_neg = egui::pos2(50.0, 80.0);
        let c_neg = constrain_square(p0, p1_neg);
        assert_eq!(c_neg, egui::pos2(50.0, 50.0));
    }

    #[test]
    fn constrain_45_snaps_to_axes_and_diagonals() {
        assert_eq!(
            constrain_45(egui::vec2(100.0, 20.0)),
            egui::vec2(100.0, 0.0)
        );
        assert_eq!(
            constrain_45(egui::vec2(-10.0, -90.0)),
            egui::vec2(0.0, -90.0)
        );
        assert_eq!(
            constrain_45(egui::vec2(-60.0, 40.0)),
            egui::vec2(-50.0, 50.0)
        );
    }

    #[test]
    fn ellipse_points_produces_closed_loop() {
        let pts = ellipse_points(egui::pos2(10.0, 20.0), egui::pos2(110.0, 120.0));
        assert!(pts.len() >= 32);
        assert_eq!(pts.first(), pts.last());
    }

    #[test]
    fn degenerate_ellipse_points_is_empty() {
        assert!(ellipse_points(egui::pos2(10.0, 10.0), egui::pos2(10.0, 50.0)).is_empty());
        assert!(ellipse_points(egui::pos2(10.0, 10.0), egui::pos2(50.0, 10.0)).is_empty());
    }

    #[test]
    fn new_selective_color_and_channel_mixer_create_adjustment_layers() {
        let ctx = egui::Context::default();
        let mut editor = editor_with_selection();
        run_on_editor(&mut editor, Command::NewSelectiveColor, &ctx);
        assert_eq!(editor.doc.layers.len(), 2);
        assert!(matches!(
            editor.doc.layers[1].adjustment,
            Some(omapix_engine::adjust::Adjustment::SelectiveColor(_))
        ));
        assert_eq!(editor.target, Target::Mask);

        run_on_editor(&mut editor, Command::NewChannelMixer, &ctx);
        assert_eq!(editor.doc.layers.len(), 3);
        assert!(matches!(
            editor.doc.layers[2].adjustment,
            Some(omapix_engine::adjust::Adjustment::ChannelMixer(_))
        ));
        assert_eq!(editor.target, Target::Mask);

        run_on_editor(&mut editor, Command::NewColorLookup, &ctx);
        assert_eq!(editor.doc.layers.len(), 4);
        assert!(matches!(
            editor.doc.layers[3].adjustment,
            Some(omapix_engine::adjust::Adjustment::ColorLookup(_))
        ));
        assert_eq!(editor.target, Target::Mask);
    }

    fn test_app() -> App {
        let (_tx, rx) = channel();
        App {
            theme: Theme::default(),
            theme_rx: rx,
            editor: Some(editor_with_selection()),
            layers: LayersPanel::default(),
            properties: PropertiesPanel::default(),
            history: HistoryPanel::default(),
            recent: RecentStore::default(),
            right_tab: RightTab::default(),
            tools: Tools::default(),
            opening: None,
            picking: None,
            file_job: None,
            dialog: None,
            status: None,
            blur_radius: 2.0,
            unsharp_mask: DEFAULT_UNSHARP_MASK,
            high_pass_radius: 2.0,
            feather_radius: 5.0,
            separation_radius: None,
            noise_options: NoiseOptions::default(),
            allow_close: false,
            title: String::new(),
            drawing: None,
            move_from: None,
            script: VecDeque::new(),
            clipboard: Clipboard::new(false),
            pasting: None,
            v_down: false,
        }
    }

    #[test]
    fn unsharp_mask_previews_then_applies_and_remembers_its_settings() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active = app.editor.as_ref().unwrap().active;
        app.run(Command::UnsharpMask, &ctx);
        let Some(Dialog::Filter { filter, preview }) = &mut app.dialog else {
            panic!("no filter dialog");
        };
        assert!(*preview);
        assert_eq!(*filter, DEFAULT_UNSHARP_MASK);
        let stronger = LayerFilter::UnsharpMask {
            amount: 2.0,
            radius: 3.0,
            threshold: 0.0,
        };
        *filter = stronger;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| app.dialogs(ctx));
        output.textures_delta.clear();
        let view = app.editor.as_ref().unwrap().view();
        assert_eq!(
            view,
            View::Filter {
                layer: active,
                filter: stronger,
                mask: false,
            }
        );

        app.dialog = None;
        app.apply_filter(stronger, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Unsharp Mask"));
        app.run(Command::UnsharpMask, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::Filter { filter, .. }) if filter == stronger));
    }

    #[test]
    fn filters_on_a_targeted_mask_change_the_mask_not_the_pixels() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let editor = app.editor.as_mut().unwrap();
        let id = editor.active;
        let (w, h) = (editor.doc.width, editor.doc.height);
        editor.doc.selection = None;
        editor.doc.layer_mut(id).unwrap().mask = Some(Mask {
            pixels: Tiled::new(w, h, 32768),
            enabled: true,
        });
        editor.target = Target::Mask;
        let pixels = editor.doc.layer(id).unwrap().pixels.to_vec();

        // Add Noise on a mask goes straight into it, through the filter dialog.
        app.run(Command::AddNoise, &ctx);
        let Some(Dialog::Filter { filter, .. }) = app.dialog.take() else {
            panic!("no filter dialog");
        };
        assert!(matches!(filter, LayerFilter::AddNoise(_)));
        app.apply_filter(filter, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Add Noise"));
        assert_eq!(editor.doc.layers.len(), 1, "no Grain layer");
        let layer = editor.doc.layer(id).unwrap();
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert!((0..w).any(|x| mask.get(x, 10) != 32768));
        assert_eq!(layer.pixels.to_vec(), pixels);
    }

    #[test]
    fn gaussian_blur_dialog_shows_live_preview_and_reverts_on_cancel() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active_id = app.editor.as_ref().unwrap().active;

        app.run(Command::GaussianBlur, &ctx);
        assert!(matches!(
            app.dialog,
            Some(Dialog::Filter {
                filter: LayerFilter::GaussianBlur { radius: 2.0 },
                preview: true,
            })
        ));

        // Running dialogs updates the editor view to View::Filter
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            app.dialogs(ctx);
        });
        output.textures_delta.clear();
        assert_eq!(
            app.editor.as_ref().unwrap().view(),
            View::Filter {
                layer: active_id,
                filter: LayerFilter::GaussianBlur { radius: 2.0 },
                mask: false,
            }
        );

        // Toggling preview off reverts to View::Image
        if let Some(Dialog::Filter { preview, .. }) = &mut app.dialog {
            *preview = false;
        }
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            app.dialogs(ctx);
        });
        output.textures_delta.clear();
        assert_eq!(app.editor.as_ref().unwrap().view(), View::Image);

        // Toggling preview back on updates to View::Filter
        if let Some(Dialog::Filter { preview, filter }) = &mut app.dialog {
            *preview = true;
            *filter = LayerFilter::GaussianBlur { radius: 4.5 };
        }
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            app.dialogs(ctx);
        });
        output.textures_delta.clear();
        assert_eq!(
            app.editor.as_ref().unwrap().view(),
            View::Filter {
                layer: active_id,
                filter: LayerFilter::GaussianBlur { radius: 4.5 },
                mask: false,
            }
        );

        // Esc cancels the dialog and returns to the image.
        let escape = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(escape, |ctx| {
            app.dialogs(ctx);
        });
        output.textures_delta.clear();
        assert!(app.dialog.is_none());
        assert_eq!(app.editor.as_ref().unwrap().view(), View::Image);
    }

    #[test]
    fn add_noise_dialog_shows_live_preview_and_applies() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active_id = app.editor.as_ref().unwrap().active;

        app.run(Command::AddNoise, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::AddNoise { .. })));

        // Running dialogs updates the editor view to View::AddNoise
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            app.dialogs(ctx);
        });
        output.textures_delta.clear();
        assert!(matches!(
            app.editor.as_ref().unwrap().view(),
            View::AddNoise { layer, .. } if layer == active_id
        ));

        // Toggling preview off reverts to View::Image
        if let Some(Dialog::AddNoise { preview, .. }) = &mut app.dialog {
            *preview = false;
        }
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            app.dialogs(ctx);
        });
        output.textures_delta.clear();
        assert_eq!(app.editor.as_ref().unwrap().view(), View::Image);

        // Toggling preview back on updates to View::AddNoise with updated settings
        if let Some(Dialog::AddNoise { preview, options }) = &mut app.dialog {
            *preview = true;
            options.amount = 35.0;
        }
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            app.dialogs(ctx);
        });
        output.textures_delta.clear();
        assert!(matches!(
            app.editor.as_ref().unwrap().view(),
            View::AddNoise { layer, options } if layer == active_id && (options.amount - 35.0).abs() < 1e-4
        ));

        // Enter submits the dialog
        let enter = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(enter, |ctx| {
            app.dialogs(ctx);
        });
        output.textures_delta.clear();
        assert!(app.dialog.is_none());
        assert_eq!(app.editor.as_ref().unwrap().view(), View::Image);

        // Wait for background edit to complete
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(5));
            editor.update(&ctx);
        }

        // The Grain layer is added on top of active layer, in Overlay mode, with opacity 0.35
        assert_eq!(editor.doc.layers.len(), 2);
        let grain = editor.doc.layers.last().unwrap();
        assert_eq!(grain.name, "Grain");
        assert_eq!(grain.blend, omapix_engine::BlendMode::Overlay);
        assert!((grain.opacity - 0.35).abs() < 1e-4);

        // One undo step ("Add Noise") reverts it
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);
    }

    #[test]
    fn previews_set_without_a_dialog_stay_up() {
        // Scripts (`View texture r`, `View blur r`) show previews with no
        // dialog open; the next frame mustn't take them down.
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active = app.editor.as_ref().unwrap().active;
        for view in [
            View::Separation {
                radius: 5.0,
                texture: true,
            },
            View::Filter {
                layer: active,
                filter: LayerFilter::GaussianBlur { radius: 3.0 },
                mask: false,
            },
            View::AddNoise {
                layer: active,
                options: NoiseOptions::default(),
            },
        ] {
            app.editor.as_mut().unwrap().set_view(view);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
                app.dialogs(ctx);
            });
            output.textures_delta.clear();
            assert_eq!(app.editor.as_ref().unwrap().view(), view);
        }
    }

    #[test]
    fn script_step_view_blur_sets_active_layer_blur_view() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active_id = app.editor.as_ref().unwrap().active;

        let (render, _) = render_view(
            &app.editor.as_ref().unwrap().doc,
            View::Image,
            [0; 4],
            None,
        );
        app.editor
            .as_mut()
            .unwrap()
            .canvas
            .set_render(Arc::new(render));

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            ui.allocate_ui(egui::vec2(800.0, 600.0), |ui| {
                let editor = app.editor.as_mut().unwrap();
                let overlay = crate::canvas::Overlay {
                    tool: false,
                    alt_samples: false,
                    samples: false,
                    brush: None,
                    moves: false,
                    source: None,
                    selection: &[],
                    drawing: None,
                    badge: None,
                };
                editor.canvas.show(ui, egui::Color32::BLACK, overlay);
            });
        });
        output.textures_delta.clear();

        let step = ScriptStep::parse("View blur 3.5").expect("failed to parse View blur");
        app.script.push_back(step);
        app.run_script(&ctx);

        assert_eq!(
            app.editor.as_ref().unwrap().view(),
            View::Filter {
                layer: active_id,
                filter: LayerFilter::GaussianBlur { radius: 3.5 },
                mask: false,
            }
        );
    }

    #[test]
    fn group_commands_build_and_take_apart_groups() {
        let ctx = egui::Context::default();
        let mut editor = editor_with_selection();
        let background = editor.active;
        let names = |e: &Editor| -> Vec<String> {
            e.doc
                .layers
                .iter()
                .map(|l| match l.parent.and_then(|p| e.doc.layer(p)) {
                    Some(g) => format!("{}({})", l.name, g.name),
                    None => l.name.clone(),
                })
                .collect()
        };
        run_on_editor(&mut editor, Command::NewLayer, &ctx);
        run_on_editor(&mut editor, Command::GroupLayers, &ctx);
        let group = editor.active;
        assert_eq!(names(&editor), ["Background", "Layer 1(Group 1)", "Group 1"]);
        assert_eq!(editor.undo_label(), Some("Group Layers"));

        // With the group selected, new layers go in at its top.
        run_on_editor(&mut editor, Command::NewLayer, &ctx);
        assert_eq!(
            names(&editor),
            ["Background", "Layer 1(Group 1)", "Layer 2(Group 1)", "Group 1"]
        );
        // Bring Forward takes it out of the top of the group.
        run_on_editor(&mut editor, Command::RaiseLayer, &ctx);
        assert_eq!(
            names(&editor),
            ["Background", "Layer 1(Group 1)", "Group 1", "Layer 2"]
        );

        editor.active = group;
        run_on_editor(&mut editor, Command::DuplicateLayer, &ctx);
        assert_eq!(editor.doc.layer(editor.active).unwrap().name, "Group 1 copy");
        assert_eq!(editor.doc.layers.len(), 6);
        run_on_editor(&mut editor, Command::DeleteLayer, &ctx);
        assert_eq!(editor.doc.layers.len(), 4, "the copy and what's in it");
        assert_eq!(editor.active, group, "the layer below is selected");

        run_on_editor(&mut editor, Command::UngroupLayers, &ctx);
        assert_eq!(names(&editor), ["Background", "Layer 1", "Layer 2"]);
        assert_eq!(editor.doc.layer(editor.active).unwrap().name, "Layer 1");

        // Ctrl+E on a group merges the group.
        editor.active = background;
        run_on_editor(&mut editor, Command::GroupLayers, &ctx);
        run_on_editor(&mut editor, Command::MergeDown, &ctx);
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Merge Group"));
        assert_eq!(names(&editor), ["Group 1", "Layer 1", "Layer 2"]);
        assert!(!editor.doc.layers[0].is_group);
        assert_eq!(editor.doc.layers[0].pixels.get(5, 5), [30000, 30000, 30000, 65535]);
    }

    #[test]
    fn high_pass_sharpening_asks_for_a_radius_then_adds_its_layer() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.run(Command::HighPassSharpening, &ctx);
        let Some(Dialog::Radius {
            command, radius, ..
        }) = app.dialog.take()
        else {
            panic!("no radius dialog");
        };
        app.apply_radius(command, radius, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("High Pass Sharpening"));
        let layer = editor.doc.layer(editor.active).unwrap();
        assert_eq!(layer.name, "High Pass Sharpening");
        assert_eq!(layer.blend, omapix_engine::blend::BlendMode::Overlay);
    }

    #[test]
    fn commands_act_on_all_the_selected_layers() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let editor = app.editor.as_mut().unwrap();
        let background = editor.active;
        let names = |e: &Editor| -> Vec<String> {
            e.doc
                .layers
                .iter()
                .map(|l| match l.parent.and_then(|p| e.doc.layer(p)) {
                    Some(g) => format!("{}({})", l.name, g.name),
                    None => l.name.clone(),
                })
                .collect()
        };
        for _ in 0..3 {
            run_on_editor(editor, Command::NewLayer, &ctx);
        }
        let [_, one, two, three] = [0, 1, 2, 3].map(|i| editor.doc.layers[i].id);
        editor.select_layers(two, vec![one, three]);

        // Ctrl+J copies them all, and selects the copies.
        run_on_editor(editor, Command::DuplicateLayer, &ctx);
        assert_eq!(editor.undo_label(), Some("Duplicate Layers"));
        assert_eq!(editor.selected().len(), 3);
        assert_eq!(editor.doc.layer(editor.active).unwrap().name, "Layer 2 copy");
        // Delete deletes them all, rather than clearing the active one.
        editor.doc.selection = None;
        app.run(Command::Clear, &ctx);
        let editor = app.editor.as_mut().unwrap();
        assert_eq!(editor.undo_label(), Some("Delete Layers"));
        assert_eq!(names(editor), ["Background", "Layer 1", "Layer 2", "Layer 3"]);
        assert_eq!(editor.active, one, "the layer below the lowest is selected");

        // Ctrl+G puts them all in one group, where the top one was.
        editor.select_layers(three, vec![one]);
        run_on_editor(editor, Command::GroupLayers, &ctx);
        assert_eq!(
            names(editor),
            ["Background", "Layer 2", "Layer 1(Group 1)", "Layer 3(Group 1)", "Group 1"]
        );
        assert_eq!(editor.selected().len(), 1, "the group");
        editor.undo();

        // Ctrl+E merges them into one, with the top one's name.
        editor.select_layers(three, vec![one, background]);
        assert!(app.enabled(Command::MergeDown));
        let editor = app.editor.as_mut().unwrap();
        run_on_editor(editor, Command::MergeDown, &ctx);
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Merge Layers"));
        assert_eq!(names(editor), ["Layer 2", "Layer 3"]);
        assert_eq!(editor.doc.layers[1].pixels.get(5, 5), [30000, 30000, 30000, 65535]);

        // Something must be left.
        editor.select_layers(two, vec![three]);
        assert!(!app.enabled(Command::DeleteLayer));
    }

    #[test]
    fn bring_to_front_and_send_to_back_commands() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let names = |app: &App| -> Vec<String> {
            let e = app.editor.as_ref().unwrap();
            e.doc
                .layers
                .iter()
                .map(|l| match l.parent.and_then(|p| e.doc.layer(p)) {
                    Some(g) => format!("{}({})", l.name, g.name),
                    None => l.name.clone(),
                })
                .collect()
        };

        // Create Layer 1, Layer 2, Layer 3
        for _ in 0..3 {
            app.run(Command::NewLayer, &ctx);
        }
        let editor = app.editor.as_mut().unwrap();
        let [background, one, two, three] = [0, 1, 2, 3].map(|i| editor.doc.layers[i].id);
        assert_eq!(names(&app), ["Background", "Layer 1", "Layer 2", "Layer 3"]);

        // Layer 3 is active at the top of the root.
        assert!(!app.enabled(Command::BringToFront));
        assert!(app.enabled(Command::SendToBack));

        // Send Layer 3 to back.
        app.run(Command::SendToBack, &ctx);
        assert_eq!(names(&app), ["Layer 3", "Background", "Layer 1", "Layer 2"]);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Send to Back"));
        assert!(!app.enabled(Command::SendToBack));
        assert!(app.enabled(Command::BringToFront));

        // Undo brings it back to the top.
        app.run(Command::Undo, &ctx);
        assert_eq!(names(&app), ["Background", "Layer 1", "Layer 2", "Layer 3"]);

        // Bring Layer 1 to front.
        let editor = app.editor.as_mut().unwrap();
        editor.active = one;
        assert!(app.enabled(Command::BringToFront));
        app.run(Command::BringToFront, &ctx);
        assert_eq!(names(&app), ["Background", "Layer 2", "Layer 3", "Layer 1"]);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Bring to Front"));

        // Inside a group:
        // Group Layer 2 and Layer 3:
        let editor = app.editor.as_mut().unwrap();
        editor.select_layers(three, vec![two]);
        app.run(Command::GroupLayers, &ctx);
        assert_eq!(
            names(&app),
            ["Background", "Layer 2(Group 1)", "Layer 3(Group 1)", "Group 1", "Layer 1"]
        );

        // Active is Layer 2 inside Group 1.
        let editor = app.editor.as_mut().unwrap();
        editor.active = two;
        assert!(app.enabled(Command::BringToFront));
        assert!(!app.enabled(Command::SendToBack));

        // Bring to Front brings Layer 2 to the top of Group 1 (not out of it!).
        app.run(Command::BringToFront, &ctx);
        assert_eq!(
            names(&app),
            ["Background", "Layer 3(Group 1)", "Layer 2(Group 1)", "Group 1", "Layer 1"]
        );
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Bring to Front"));

        // Send to Back sends Layer 2 back to the bottom of Group 1.
        app.run(Command::SendToBack, &ctx);
        assert_eq!(
            names(&app),
            ["Background", "Layer 2(Group 1)", "Layer 3(Group 1)", "Group 1", "Layer 1"]
        );
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Send to Back"));

        // Several selected layers:
        // Select Layer 1 and Background:
        let editor = app.editor.as_mut().unwrap();
        editor.select_layers(background, vec![one]);
        // Send to back moves both to the bottom:
        app.run(Command::SendToBack, &ctx);
        assert_eq!(
            names(&app),
            ["Background", "Layer 1", "Layer 2(Group 1)", "Layer 3(Group 1)", "Group 1"]
        );
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Send to Back"));
        assert!(!app.enabled(Command::SendToBack));
        assert!(app.enabled(Command::BringToFront));

        // Bring to front moves both to the front of root:
        app.run(Command::BringToFront, &ctx);
        assert_eq!(
            names(&app),
            ["Layer 2(Group 1)", "Layer 3(Group 1)", "Group 1", "Background", "Layer 1"]
        );
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Bring to Front"));
    }

    #[test]
    fn select_layer_above_and_below_commands() {
        let ctx = egui::Context::default();
        let mut app = test_app();

        // Start with Background. Create Layer 1, Layer 2.
        app.run(Command::NewLayer, &ctx);
        app.run(Command::NewLayer, &ctx);
        let editor = app.editor.as_mut().unwrap();
        let [background, one, two] = [0, 1, 2].map(|i| editor.doc.layers[i].id);

        // Group Layer 1:
        editor.active = one;
        app.run(Command::GroupLayers, &ctx);
        let group = app.editor.as_ref().unwrap().active;
        // Stack top first: Layer 2, Group 1, [Layer 1], Background
        // By default, group starts closed.
        app.layers.set_group_expanded(group, false);

        // Set active to Background (bottom row).
        let editor = app.editor.as_mut().unwrap();
        editor.select_layers(background, Vec::new());

        assert!(!app.enabled(Command::SelectLayerBelow));
        assert!(app.enabled(Command::SelectLayerAbove));

        // Alt+[ at bottom does nothing (no wrap).
        app.run(Command::SelectLayerBelow, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, background);

        // Alt+] from Background: Group 1 is closed, so skips Layer 1 and selects Group 1!
        app.run(Command::SelectLayerAbove, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, group);

        // Alt+] from Group 1: selects Layer 2!
        app.run(Command::SelectLayerAbove, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, two);
        assert_eq!(app.editor.as_ref().unwrap().target, Target::Pixels);

        // At top: Alt+] does nothing (no wrap).
        assert!(!app.enabled(Command::SelectLayerAbove));
        app.run(Command::SelectLayerAbove, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, two);

        // Alt+[ from Layer 2: selects Group 1.
        app.run(Command::SelectLayerBelow, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, group);

        // Now open Group 1:
        app.layers.set_group_expanded(group, true);

        // Alt+[ steps into open group -> Layer 1!
        app.run(Command::SelectLayerBelow, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, one);
        assert_eq!(app.editor.as_ref().unwrap().target, Target::Pixels);

        // Alt+[ from Layer 1 -> Background!
        app.run(Command::SelectLayerBelow, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, background);

        // Alt+] from Background -> Layer 1!
        app.run(Command::SelectLayerAbove, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, one);

        // Alt+] from Layer 1 -> Group 1!
        app.run(Command::SelectLayerAbove, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, group);

        // Alt+] from Group 1 -> Layer 2!
        app.run(Command::SelectLayerAbove, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().active, two);

        // Selecting this way clears multiple selection:
        let editor = app.editor.as_mut().unwrap();
        editor.select_layers(two, vec![background]);
        assert_eq!(editor.selected().len(), 2);
        app.run(Command::SelectLayerBelow, &ctx);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.active, group);
        assert_eq!(editor.selected(), [group], "selects just that one layer");
    }

    #[test]
    fn right_tab_switches_between_layers_and_history() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        assert_eq!(app.right_tab, RightTab::Layers);

        app.run(Command::ShowHistory, &ctx);
        assert_eq!(app.right_tab, RightTab::History);

        app.run(Command::ShowLayers, &ctx);
        assert_eq!(app.right_tab, RightTab::Layers);
    }

    #[test]
    fn reopen_last_command_and_recent_files() {
        let ctx = egui::Context::default();
        let mut app = test_app();

        // Initially recent is empty, ReopenLast is disabled.
        assert!(!app.enabled(Command::ReopenLast));

        // Add a recent file.
        let dir = std::env::temp_dir().join(format!("omapix_app_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_img.tif");
        let _ = std::fs::write(&path, b"dummy");

        app.recent.add(&path);
        assert!(app.enabled(Command::ReopenLast));
        assert_eq!(app.recent.last(), Some(&std::fs::canonicalize(&path).unwrap()));

        // Run ReopenLast: triggers opening.
        app.editor.as_mut().unwrap().modified = false;
        app.run(Command::ReopenLast, &ctx);
        assert!(app.opening.is_some());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn script_step_history_jumps_undo_states() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let editor = app.editor.as_mut().unwrap();
        let base_index = editor.history_active_index();
        editor.edit("Change 1", |doc, _| doc.layers[0].opacity = 0.7);
        editor.edit("Change 2", |doc, _| doc.layers[0].opacity = 0.3);
        assert_eq!(editor.history_active_index(), base_index + 2);
        assert_eq!(editor.doc.layers[0].opacity, 0.3);

        // Layout canvas so canvas.ready() returns true for run_script.
        let (render, _) = render_view(
            &app.editor.as_ref().unwrap().doc,
            View::Image,
            [0; 4],
            None,
        );
        app.editor
            .as_mut()
            .unwrap()
            .canvas
            .set_render(Arc::new(render));

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            ui.allocate_ui(egui::vec2(800.0, 600.0), |ui| {
                let editor = app.editor.as_mut().unwrap();
                let overlay = crate::canvas::Overlay {
                    tool: false,
                    alt_samples: false,
                    samples: false,
                    brush: None,
                    moves: false,
                    source: None,
                    selection: &[],
                    drawing: None,
                    badge: None,
                };
                editor.canvas.show(ui, egui::Color32::BLACK, overlay);
            });
        });
        output.textures_delta.clear();

        app.script.push_back(ScriptStep::History(base_index + 1));
        app.run_script(&ctx);
        assert_eq!(app.editor.as_ref().unwrap().history_active_index(), base_index + 1);
        assert_eq!(app.editor.as_ref().unwrap().doc.layers[0].opacity, 0.7);
    }

    #[test]
    fn clipping_mask_command_clips_and_releases() {
        let ctx = egui::Context::default();
        let mut editor = editor_with_selection();
        assert!(!editor.doc.can_clip(0), "nothing below the background");
        run_on_editor(&mut editor, Command::NewLayer, &ctx);
        let layer = editor.active;
        run_on_editor(&mut editor, Command::ClippingMask, &ctx);
        assert!(editor.doc.layer(layer).unwrap().clipped);
        assert_eq!(editor.undo_label(), Some("Create Clipping Mask"));
        // A new layer above a clipped one joins the clipping mask.
        run_on_editor(&mut editor, Command::NewLayer, &ctx);
        assert!(editor.doc.layer(editor.active).unwrap().clipped);
        editor.active = layer;
        run_on_editor(&mut editor, Command::ClippingMask, &ctx);
        assert_eq!(editor.undo_label(), Some("Release Clipping Mask"));
        assert!(editor.doc.layers.iter().all(|l| !l.clipped));
        editor.undo();
        assert!(editor.doc.layer(layer).unwrap().clipped);
    }

    #[test]
    fn dodge_and_burn_curves_selects_the_dodge_mask() {
        let ctx = egui::Context::default();
        let mut editor = editor_with_selection();
        run_on_editor(&mut editor, Command::DodgeAndBurnCurves, &ctx);
        let names: Vec<_> = editor.doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Background", "Burn", "Dodge", "Dodge & Burn"]);
        assert_eq!(editor.doc.layer(editor.active).unwrap().name, "Dodge");
        assert_eq!(editor.target, Target::Mask);
        assert_eq!(editor.undo_label(), Some("Dodge & Burn Curves"));
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);
    }

    #[test]
    fn magic_wand_tool_selects_and_deselects() {
        let mut app = test_app();
        app.tools.select(crate::tools::Tool::MagicWand);
        assert_eq!(app.tools.tool, crate::tools::Tool::MagicWand);

        // Click at (50, 50)
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)),
            egui::Modifiers::NONE,
        );
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);

        assert!(app.editor.as_ref().unwrap().doc.selection.is_some());
        let sel = app.editor.as_ref().unwrap().doc.selection.as_ref().unwrap();
        assert_eq!(sel.at(50, 50), 1.0);

        // Click outside image deselects
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(700.0, 500.0)),
            egui::Modifiers::NONE,
        );
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
        assert!(app.editor.as_ref().unwrap().doc.selection.is_none());
    }

    #[test]
    fn curves_eyedropper_canvas_click_sets_points_with_undo() {
        use omapix_engine::adjust::{Adjustment, Curves, Eyedropper};

        let mut app = test_app();
        let (w, h) = (
            app.editor.as_ref().unwrap().doc.width,
            app.editor.as_ref().unwrap().doc.height,
        );
        let curves_layer = Layer::adjustment(200, Adjustment::Curves(Curves::default()), w, h);
        app.editor.as_mut().unwrap().doc.layers.push(curves_layer);
        app.editor.as_mut().unwrap().active = 200;

        // Arm Black Point eyedropper and click canvas
        app.properties.eyedropper = Some(Eyedropper::Black);
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)),
            egui::Modifiers::NONE,
        );
        assert_eq!(
            app.editor.as_ref().unwrap().undo_label(),
            Some("Set Black Point")
        );
        if let Some(Adjustment::Curves(c)) = &app.editor.as_ref().unwrap().doc.layer(200).unwrap().adjustment {
            assert!(c.red.points[0].0 > 0.0);
        } else {
            panic!("expected Curves adjustment");
        }

        // Arm White Point eyedropper and click canvas
        app.properties.eyedropper = Some(Eyedropper::White);
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(60.0, 60.0)),
            egui::Modifiers::NONE,
        );
        assert_eq!(
            app.editor.as_ref().unwrap().undo_label(),
            Some("Set White Point")
        );

        // Arm Gray Point eyedropper and click canvas
        app.properties.eyedropper = Some(Eyedropper::Gray);
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(70.0, 70.0)),
            egui::Modifiers::NONE,
        );
        assert_eq!(
            app.editor.as_ref().unwrap().undo_label(),
            Some("Set Gray Point")
        );
    }

    #[test]
    fn eyedropper_armed_prevents_painting_and_selection() {
        use omapix_engine::adjust::{Adjustment, Curves, Eyedropper};

        let mut app = test_app();
        let (w, h) = (
            app.editor.as_ref().unwrap().doc.width,
            app.editor.as_ref().unwrap().doc.height,
        );
        let curves_layer = Layer::adjustment(200, Adjustment::Curves(Curves::default()), w, h);
        app.editor.as_mut().unwrap().doc.layers.push(curves_layer);
        app.editor.as_mut().unwrap().active = 200;

        // Ensure no initial selection
        app.editor.as_mut().unwrap().doc.selection = None;

        // Select Marquee tool
        app.tools.select(crate::tools::Tool::Marquee);
        app.properties.eyedropper = Some(Eyedropper::Black);

        // Click on canvas
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)),
            egui::Modifiers::NONE,
        );
        // Eyedropper handled the click, did not start a selection
        assert!(app.editor.as_ref().unwrap().doc.selection.is_none());
        assert_eq!(
            app.editor.as_ref().unwrap().undo_label(),
            Some("Set Black Point")
        );
    }

    #[test]
    fn levels_eyedropper_canvas_click_sets_points_with_undo() {
        use omapix_engine::adjust::{Adjustment, Eyedropper, Levels};

        let mut app = test_app();
        let (w, h) = (
            app.editor.as_ref().unwrap().doc.width,
            app.editor.as_ref().unwrap().doc.height,
        );
        let levels_layer = Layer::adjustment(201, Adjustment::Levels(Levels::default()), w, h);
        app.editor.as_mut().unwrap().doc.layers.push(levels_layer);
        app.editor.as_mut().unwrap().active = 201;

        // Arm Black Point eyedropper and click canvas
        app.properties.eyedropper = Some(Eyedropper::Black);
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)),
            egui::Modifiers::NONE,
        );
        assert_eq!(
            app.editor.as_ref().unwrap().undo_label(),
            Some("Set Black Point")
        );
        if let Some(Adjustment::Levels(l)) = &app.editor.as_ref().unwrap().doc.layer(201).unwrap().adjustment {
            assert!(l.in_black > 0.0);
        } else {
            panic!("expected Levels adjustment");
        }

        // Arm White Point eyedropper and click canvas
        app.properties.eyedropper = Some(Eyedropper::White);
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(60.0, 60.0)),
            egui::Modifiers::NONE,
        );
        assert_eq!(
            app.editor.as_ref().unwrap().undo_label(),
            Some("Set White Point")
        );

        // Arm Gray Point eyedropper and click canvas
        app.properties.eyedropper = Some(Eyedropper::Gray);
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(70.0, 70.0)),
            egui::Modifiers::NONE,
        );
        assert_eq!(
            app.editor.as_ref().unwrap().undo_label(),
            Some("Set Gray Point")
        );
    }

    #[test]
    fn eyedropper_escape_key_disarms() {
        use omapix_engine::adjust::{Adjustment, Curves, Eyedropper};

        let mut app = test_app();
        let (w, h) = (
            app.editor.as_ref().unwrap().doc.width,
            app.editor.as_ref().unwrap().doc.height,
        );
        let curves_layer = Layer::adjustment(200, Adjustment::Curves(Curves::default()), w, h);
        app.editor.as_mut().unwrap().doc.layers.push(curves_layer);
        app.editor.as_mut().unwrap().active = 200;

        app.properties.eyedropper = Some(Eyedropper::Black);
        assert_eq!(app.properties.eyedropper, Some(Eyedropper::Black));

        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut out = ctx.run_ui(input, |ui| {
            app.check_escape(ui.ctx());
        });
        out.textures_delta.clear();
        assert_eq!(app.properties.eyedropper, None);
    }

    struct SelectionEdgesHarness {
        ctx: egui::Context,
        app: App,
        time: f64,
    }

    impl SelectionEdgesHarness {
        fn new() -> Self {
            let app = test_app();
            assert!(app.editor.as_ref().unwrap().doc.selection.is_some());
            Self {
                ctx: egui::Context::default(),
                app,
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.time += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let mut output = self.ctx.run_ui(input, |ui| {
                for cmd in Command::pressed(ui.ctx(), &mut self.app.v_down) {
                    let ctx = ui.ctx().clone();
                    self.app.run(cmd, &ctx);
                }
                self.app.menu_bar(ui);
                self.app.status_bar(ui);
            });
            output.textures_delta.clear();
            output
        }

        fn press_ctrl_h(&mut self) {
            self.frame(vec![
                egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
                egui::Event::Key {
                    key: egui::Key::H,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
            ]);
        }

        fn output_contains_text(&self, output: &egui::FullOutput, needle: &str) -> bool {
            fn shape_contains(shape: &egui::Shape, needle: &str) -> bool {
                match shape {
                    egui::Shape::Text(t) => t.galley.text().contains(needle),
                    egui::Shape::Vec(v) => v.iter().any(|s| shape_contains(s, needle)),
                    _ => false,
                }
            }
            output.shapes.iter().any(|cs| shape_contains(&cs.shape, needle))
        }

        fn find_text_pos(&self, output: &egui::FullOutput, text: &str) -> Option<egui::Pos2> {
            fn shape_pos(shape: &egui::Shape, text: &str) -> Option<egui::Pos2> {
                match shape {
                    egui::Shape::Text(t) => {
                        if t.galley.text() == text {
                            Some(t.pos)
                        } else {
                            None
                        }
                    }
                    egui::Shape::Vec(v) => v.iter().find_map(|s| shape_pos(s, text)),
                    _ => None,
                }
            }
            output.shapes.iter().find_map(|cs| shape_pos(&cs.shape, text))
        }

        fn click(&mut self, pos: egui::Pos2) {
            self.time += 0.5;
            self.frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            self.frame(vec![
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }

        fn open_view_menu(&mut self) -> egui::FullOutput {
            let out = self.frame(vec![]);
            let pos = self
                .find_text_pos(&out, "View")
                .expect("View menu button not found");
            self.click(pos);
            self.frame(vec![])
        }

        fn close_menu(&mut self) {
            self.frame(vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
    }

    #[test]
    fn selection_edges_ctrl_h_toggles_and_updates_status_bar() {
        let mut h = SelectionEdgesHarness::new();
        assert!(!h.app.editor.as_ref().unwrap().hide_selection_edges);

        // Initially visible: status bar does not have hidden message
        let out = h.frame(vec![]);
        assert!(!h.output_contains_text(&out, "Selection edges hidden"));

        // Press Ctrl+H to hide selection edges
        h.press_ctrl_h();
        assert!(h.app.editor.as_ref().unwrap().hide_selection_edges);

        // Status bar now displays the hidden message
        let out = h.frame(vec![]);
        assert!(h.output_contains_text(&out, "Selection edges hidden — press Ctrl+H to show"));

        // Press Ctrl+H again to show edges
        h.press_ctrl_h();
        assert!(!h.app.editor.as_ref().unwrap().hide_selection_edges);

        let out = h.frame(vec![]);
        assert!(!h.output_contains_text(&out, "Selection edges hidden"));
    }

    #[test]
    fn selection_edges_view_menu_label_reflects_state() {
        let mut h = SelectionEdgesHarness::new();

        // When visible, open View menu and verify it offers to hide
        let out = h.open_view_menu();
        assert!(h.output_contains_text(&out, "Hide Selection Edges"));
        assert!(!h.output_contains_text(&out, "Show Selection Edges"));

        // Close menu
        h.close_menu();

        // When hidden, open View menu and verify it offers to show
        h.press_ctrl_h();
        let out = h.open_view_menu();
        assert!(h.output_contains_text(&out, "Show Selection Edges"));
        assert!(!h.output_contains_text(&out, "Hide Selection Edges"));
    }

    #[test]
    fn selection_edges_new_selection_shows_edges_again() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.run(Command::SelectionEdges, &ctx);
        assert!(app.editor.as_ref().unwrap().hide_selection_edges);

        // Select All resets hidden state
        app.run(Command::SelectAll, &ctx);
        assert!(!app.editor.as_ref().unwrap().hide_selection_edges);

        // Hide edges again, then InvertSelection
        app.run(Command::SelectionEdges, &ctx);
        assert!(app.editor.as_ref().unwrap().hide_selection_edges);
        app.run(Command::InvertSelection, &ctx);
        assert!(!app.editor.as_ref().unwrap().hide_selection_edges);

        // Hide edges again, then draw a Marquee
        app.run(Command::SelectionEdges, &ctx);
        assert!(app.editor.as_ref().unwrap().hide_selection_edges);
        app.tools.select(crate::tools::Tool::Marquee);
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(10.0, 10.0)),
            egui::Modifiers::NONE,
        );
        app.tool_input(
            ToolInput::StrokeMove(egui::pos2(50.0, 50.0)),
            egui::Modifiers::NONE,
        );
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
        assert!(!app.editor.as_ref().unwrap().hide_selection_edges);

        // Hide edges again, then use Magic Wand
        app.run(Command::SelectionEdges, &ctx);
        assert!(app.editor.as_ref().unwrap().hide_selection_edges);
        app.tools.select(crate::tools::Tool::MagicWand);
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(20.0, 20.0)),
            egui::Modifiers::NONE,
        );
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
        assert!(!app.editor.as_ref().unwrap().hide_selection_edges);
    }

    #[test]
    fn selection_edges_deselect_keeps_hidden_state_irrelevant() {
        let mut h = SelectionEdgesHarness::new();
        h.press_ctrl_h();
        assert!(h.app.editor.as_ref().unwrap().hide_selection_edges);

        // With edges hidden and selection active, status bar message is shown
        let out = h.frame(vec![]);
        assert!(h.output_contains_text(&out, "Selection edges hidden — press Ctrl+H to show"));

        // Deselect
        let ctx = egui::Context::default();
        h.app.run(Command::Deselect, &ctx);
        assert!(h.app.editor.as_ref().unwrap().doc.selection.is_none());

        // Status bar message must not appear when there is no selection
        let out = h.frame(vec![]);
        assert!(!h.output_contains_text(&out, "Selection edges hidden"));
    }

    #[test]
    fn selection_edges_canvas_stops_repaints_when_hidden_and_draws_during_drag() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };

        // Settle any background tile rendering
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(20));
            app.editor.as_mut().unwrap().update(&ctx);
            let mut out = ctx.run_ui(input.clone(), |ui| {
                let editor = app.editor.as_mut().unwrap();
                let overlay = crate::canvas::Overlay::default();
                editor.canvas.show(ui, egui::Color32::BLACK, overlay);
            });
            out.textures_delta.clear();
        }

        // 1. Initially with edges visible, canvas requests repaint for marching ants
        let mut out = ctx.run_ui(input.clone(), |ui| {
            let editor = app.editor.as_mut().unwrap();
            let outlines = if editor.hide_selection_edges {
                &[][..]
            } else {
                editor.doc.selection.as_ref().map_or(&[][..], |s| &s.outlines[..])
            };
            let overlay = crate::canvas::Overlay {
                selection: outlines,
                drawing: None,
                ..Default::default()
            };
            editor.canvas.show(ui, egui::Color32::BLACK, overlay);
        });
        out.textures_delta.clear();
        assert!(out.viewport_output[&egui::ViewportId::ROOT].repaint_delay <= Duration::from_millis(100));

        // 2. Hide selection edges: canvas must go idle (no repaint requested)
        app.editor.as_mut().unwrap().hide_selection_edges = true;
        let mut out = ctx.run_ui(input.clone(), |ui| {
            let editor = app.editor.as_mut().unwrap();
            let outlines = if editor.hide_selection_edges {
                &[][..]
            } else {
                editor.doc.selection.as_ref().map_or(&[][..], |s| &s.outlines[..])
            };
            let overlay = crate::canvas::Overlay {
                selection: outlines,
                drawing: None,
                ..Default::default()
            };
            editor.canvas.show(ui, egui::Color32::BLACK, overlay);
        });
        out.textures_delta.clear();
        assert_eq!(out.viewport_output[&egui::ViewportId::ROOT].repaint_delay, Duration::MAX);

        // 3. While dragging a marquee, shape is drawn and repaints resume
        let drag_shape = [egui::pos2(10.0, 10.0), egui::pos2(100.0, 100.0)];
        let mut out = ctx.run_ui(input, |ui| {
            let editor = app.editor.as_mut().unwrap();
            let outlines = if editor.hide_selection_edges {
                &[][..]
            } else {
                editor.doc.selection.as_ref().map_or(&[][..], |s| &s.outlines[..])
            };
            let overlay = crate::canvas::Overlay {
                selection: outlines,
                drawing: Some(&drag_shape),
                ..Default::default()
            };
            editor.canvas.show(ui, egui::Color32::BLACK, overlay);
        });
        out.textures_delta.clear();
        assert!(out.viewport_output[&egui::ViewportId::ROOT].repaint_delay <= Duration::from_millis(100));
    }

    #[test]
    fn eyedropper_tool_shortcut_and_selection() {
        let mut app = test_app();
        assert_ne!(app.tools.tool, crate::tools::Tool::Eyedropper);
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::I,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |_| {});
        out.textures_delta.clear();
        app.tools.keys(&ctx);
        assert_eq!(app.tools.tool, crate::tools::Tool::Eyedropper);
    }

    #[test]
    fn eyedropper_samples_foreground_and_background_with_click_and_drag() {
        let mut app = test_app();
        app.tools.select(crate::tools::Tool::Eyedropper);

        let render = Arc::new(crate::canvas::Render::new(
            app.editor.as_ref().unwrap().doc.composite(),
        ));
        app.editor.as_mut().unwrap().canvas.set_render(render);

        app.tools.foreground = [0, 0, 0];
        app.tools.background = [255, 255, 255];

        // 1. Plain click samples foreground
        app.tool_input(
            crate::canvas::ToolInput::Sample(egui::pos2(50.0, 50.0)),
            egui::Modifiers::NONE,
        );
        assert_ne!(app.tools.foreground, [0, 0, 0]);
        assert_eq!(app.tools.background, [255, 255, 255]);

        // 2. Alt+click samples background
        app.tools.background = [0, 0, 0];
        app.tool_input(
            crate::canvas::ToolInput::Sample(egui::pos2(50.0, 50.0)),
            egui::Modifiers::ALT,
        );
        assert_ne!(app.tools.background, [0, 0, 0]);

        // 3. Dragging continues to sample live
        let prev_fg = app.tools.foreground;
        app.tool_input(
            crate::canvas::ToolInput::Sample(egui::pos2(60.0, 60.0)),
            egui::Modifiers::NONE,
        );
        assert_eq!(app.tools.foreground, prev_fg);

        // 4. Painting tool (Brush) Alt+click samples foreground using sample size
        app.tools.select(crate::tools::Tool::Brush);
        app.tools.foreground = [12, 34, 56];
        app.tool_input(
            crate::canvas::ToolInput::Sample(egui::pos2(50.0, 50.0)),
            egui::Modifiers::ALT,
        );
        assert_ne!(app.tools.foreground, [12, 34, 56]);
    }

    #[test]
    fn load_selection_command_and_undo() {
        let ctx = egui::Context::default();
        let mut app = test_app();

        // Deselect first so we start clean
        app.run(Command::Deselect, &ctx);
        assert!(app.editor.as_ref().unwrap().doc.selection.is_none());

        // Hide edges first with Ctrl+H
        app.run(Command::SelectionEdges, &ctx);
        assert!(app.editor.as_ref().unwrap().hide_selection_edges);

        // Run LoadSelectionTransparency on the background layer
        app.run(Command::LoadSelectionTransparency, &ctx);
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.doc.selection.is_some());
        assert_eq!(editor.undo_label(), Some("Load Selection"));
        // Edges should show again even if Ctrl+H hid them previously
        assert!(!app.editor.as_ref().unwrap().hide_selection_edges);

        // Undo reverts the selection
        app.run(Command::Undo, &ctx);
        assert!(app.editor.as_ref().unwrap().doc.selection.is_none());

        // Redo restores it
        app.run(Command::Redo, &ctx);
        assert!(app.editor.as_ref().unwrap().doc.selection.is_some());

        // Background has pixels, but no mask
        assert!(app.enabled(Command::LoadSelectionTransparency));
        assert!(!app.enabled(Command::LoadSelectionLayerMask));

        // Add Curves adjustment layer (has mask, but no pixels)
        app.run(Command::NewCurves, &ctx);
        assert!(!app.enabled(Command::LoadSelectionTransparency));
        assert!(app.enabled(Command::LoadSelectionLayerMask));

        // Loading selection from channel (e.g. Red) works
        app.run(Command::LoadSelectionRed, &ctx);
        assert!(app.editor.as_ref().unwrap().doc.selection.is_some());
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Load Selection"));
    }
}

