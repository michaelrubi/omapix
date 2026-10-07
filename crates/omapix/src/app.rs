use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::{Align, Align2, Button, Layout, Pos2, RichText, Sense, Ui, Vec2, pos2, vec2};
use omapix_engine::adjust::Eyedropper;
use omapix_engine::brush::Paint;
use omapix_engine::clip::{self, Clip, PasteKind};
use omapix_engine::filters::LayerFilter;
use omapix_engine::layer::{Layer, Locks, Mask};
use omapix_engine::selection::{Channel, Combine, Selection};
use omapix_engine::tiled::{Orientation, Tiled};
use omapix_engine::{
    ColorProfile, DisplayTransform, Document, NoiseDistribution, NoiseOptions, Proof,
    ReduceNoiseOptions, SharpenRemove, SmartBlurMode, SmartBlurOptions, SmartBlurQuality,
    SmartSharpenOptions, align, export, ops, ora,
};

use crate::canvas::ToolInput;
use crate::settings::FilterSettings;
use crate::tablet::Tablet;
use crate::content_fill::ContentFill;
use crate::select_subject::SelectSubject;
use crate::face_selection::{FaceSelection, Part};
use crate::preview_box::PreviewBox;
use crate::clipboard::Clipboard;
use crate::commands::Command;
use crate::editor::{Editor, SeparationBand, Target, View};
use crate::histogram_panel::HistogramPanel;
use crate::history_panel::HistoryPanel;
use crate::layers_panel::LayersPanel;
use crate::presets::Preset;
use crate::navigator_panel::NavigatorPanel;
use crate::properties_panel::PropertiesPanel;
use crate::recent::RecentStore;
use crate::theme::{self, Theme};
use crate::tools::{Sample, Tools};

const OPEN_EXTENSIONS: [&str; 7] = ["ora", "tif", "tiff", "png", "jpg", "jpeg", "psd"];
const JPEG_QUALITY: u8 = 92;
/// Points a side of an exported LUT (33 is the usual size).
const LUT_SIZE: usize = 33;
/// How near Free Transform's handles the pointer grabs them, in points.
const HANDLE_REACH: f32 = 8.0;

/// A photo to merge with others: its name and how it looks.
type Frame = (String, Tiled<omapix_engine::Pixel>);
/// Photos merged into a new image, and the names of those left out.
type Merged = (Document, Vec<String>);

/// What a file dialog was opened for.
#[derive(Clone, Copy)]
enum Purpose {
    Open,
    SaveAs,
    ExportTiff,
    ExportJpeg,
    ExportPng,
    ExportLut,
    ProofProfile,
    Photomerge,
}

/// Something to do once unsaved changes are dealt with.
#[derive(Clone)]
enum Then {
    Quit,
    Close,
}

/// An open image that isn't the one showing, with its panels as they were.
/// Soft proofing, as the View menu has it.
struct ProofView {
    /// Proof Colors.
    colors: bool,
    gamut_warning: bool,
    /// Proof Setup's profile: sRGB (the web), or the ICC profile named in
    /// the settings.
    profile: ColorProfile,
}

impl Default for ProofView {
    fn default() -> Self {
        Self {
            colors: false,
            gamut_warning: false,
            profile: ColorProfile::srgb(),
        }
    }
}

impl ProofView {
    /// What to show, if anything's on.
    fn proof(&self) -> Option<Proof<'_>> {
        (self.colors || self.gamut_warning).then_some(Proof {
            profile: &self.profile,
            colors: self.colors,
            gamut_warning: self.gamut_warning,
        })
    }

    /// The profile's name: "sRGB", not what a file with no profile is
    /// said to be.
    fn name(&self) -> &str {
        match self.profile.icc() {
            Some(_) => self.profile.description(),
            None => "sRGB",
        }
    }

    /// An ICC profile to proof for, if it's one that can be.
    fn read(path: &Path) -> Result<ColorProfile, String> {
        let icc = std::fs::read(path).map_err(|e| e.to_string())?;
        let profile = ColorProfile::from_icc(icc).map_err(|e| e.to_string())?;
        let proof = Proof {
            profile: &profile,
            colors: true,
            gamut_warning: true,
        };
        let srgb = ColorProfile::srgb();
        DisplayTransform::new(&srgb, &srgb, Some(proof)).map_err(|e| e.to_string())?;
        Ok(profile)
    }
}

struct Parked {
    editor: Editor,
    layers: LayersPanel,
    properties: PropertiesPanel,
}

/// An image's name on its tab and in the window's title.
fn tab_name(editor: &Editor) -> String {
    format!("{}{}", editor.doc.file_name(), if editor.modified { " •" } else { "" })
}

struct PendingClip {
    clip: Clip,
    name: Option<String>,
    kind: PasteKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TopTab {
    Navigator,
    Histogram,
}

impl TopTab {
    /// Show `tab` above the layers, or hide it if it's showing.
    fn toggle(shown: &mut Option<TopTab>, tab: TopTab) {
        *shown = (*shown != Some(tab)).then_some(tab);
    }

    const ALL: [(TopTab, Command); 2] = [
        (TopTab::Navigator, Command::ShowNavigator),
        (TopTab::Histogram, Command::ShowHistogram),
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum RightTab {
    #[default]
    Layers,
    Channels,
    History,
}

impl RightTab {
    const ALL: [(RightTab, Command); 3] = [
        (RightTab::Layers, Command::ShowLayers),
        (RightTab::Channels, Command::ShowChannels),
        (RightTab::History, Command::ShowHistory),
    ];
}

enum Dialog {
    /// Ask for a radius, then run a filter. For frequency separation,
    /// `preview` shows the texture layer, mid layer, colour/tone layer
    /// or the image (`None`) while adjusting.
    Radius {
        command: Command,
        radius: f32,
        coarse: Option<f32>,
        preview: Option<SeparationBand>,
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
    SelectAndMask(crate::select_and_mask::SelectAndMask),
    AutoRetouch(crate::auto_retouch::AutoRetouch),
    HealBlemishes(crate::heal_blemishes::HealBlemishes),
    SmoothSkin(crate::smooth_skin::SmoothSkin),
    EvenTone(crate::even_tone::EvenTone),
    Denoise(crate::denoise::Denoise),
    GenerativeFill(crate::generative_fill::GenerativeFill),
    /// Name the selected adjustment layers to keep as a preset.
    SavePreset {
        name: String,
    },
    /// Ask before deleting a preset.
    DeletePreset {
        name: String,
    },
    /// File › New: the new image's size in pixels.
    NewImage {
        width: u32,
        height: u32,
    },
    /// Image › Image Size, in pixels; `constrain` keeps the proportions,
    /// and `ai` enlarges with the model (`crate::upscale`).
    ImageSize {
        width: u32,
        height: u32,
        constrain: bool,
        ai: bool,
    },
    /// Image › Canvas Size: the image goes at `anchor` (0–2 across, 0–2
    /// down), and new areas of the bottom layer are `extension`.
    CanvasSize {
        width: u32,
        height: u32,
        anchor: (u8, u8),
        extension: Extension,
    },
    /// File › Batch Export: the files chosen, the settings, and a file
    /// dialog open in the background (for the output folder, if `true`).
    BatchExport {
        files: Vec<PathBuf>,
        settings: crate::settings::BatchExport,
        picking: Option<(bool, Receiver<Option<Vec<PathBuf>>>)>,
    },
    /// Help › AI Models: where each of `omapix_ai::models::models()` was
    /// found on disk, if it was, when the dialog opened.
    AiModels(Vec<Option<HashMap<&'static str, PathBuf>>>),
}

/// The models that find faces, their points and their skin.
const FACE_MODELS: &[&str] = &[omapix_ai::face::DETECTOR, omapix_ai::face::LANDMARKER, omapix_ai::face::SEGMENTER];
/// The one that finds people and their joints.
const POSE_MODELS: &[&str] = &[omapix_ai::pose::MODEL];

/// The AI models `cmd` needs.
fn models_for(cmd: Command) -> &'static [&'static str] {
    match cmd {
        Command::SelectSubject => &[omapix_ai::subject::MODEL],
        Command::ContentAwareFill => &[omapix_ai::lama::MODEL],
        Command::GenerativeFill => &[omapix_ai::flux::MODEL],
        Command::Denoise => &[omapix_ai::denoise::MODEL],
        Command::SelectSkin
        | Command::SelectHair
        | Command::SelectEyes
        | Command::SelectLips
        | Command::SelectTeeth
        | Command::AutoRetouch
        | Command::HealBlemishes
        | Command::SmoothSkin
        | Command::EvenTone
        | Command::ReduceShine
        | Command::LightenUnderEyes
        | Command::WhitenTeeth
        | Command::WhitenEyes => FACE_MODELS,
        _ => &[],
    }
}

/// The names of the models `cmd` needs that aren't on disk, as last
/// checked.
fn missing_models(cmd: Command, on_disk: &HashMap<&'static str, bool>) -> Vec<&'static str> {
    missing(models_for(cmd), on_disk)
}

/// The names of those of the models `ids` that aren't on disk, as last
/// checked.
fn missing(ids: &[&str], on_disk: &HashMap<&'static str, bool>) -> Vec<&'static str> {
    let models = omapix_ai::models::models();
    ids.iter()
        .filter(|id| on_disk.get(*id) == Some(&false))
        .filter_map(|id| models.iter().find(|m| m.id == *id).map(|m| m.name))
        .collect()
}

/// Which AI models are on disk, found on a thread of its own.
fn check_models(ctx: &egui::Context) -> Receiver<HashMap<&'static str, bool>> {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let models = omapix_ai::models::models();
        let _ = tx.send(models.iter().map(|m| (m.id, omapix_ai::find_model(m.id).is_some())).collect());
        ctx.request_repaint();
    });
    rx
}

/// A drag on the Crop tool's box: on a handle or inside (with the box as
/// it was), or outside, drawing a new box from there.
#[derive(Clone, Copy)]
enum CropDrag {
    Box(crate::free_transform::Drag, [f64; 4]),
    New(Pos2),
}

/// What Canvas Size fills new areas of the bottom layer with (Photoshop's
/// "Canvas extension color").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Extension {
    Background,
    Foreground,
    White,
    Black,
    Transparent,
}

/// The 100 % preview box in a filter's dialog: where it looks, and what it
/// shows there with the settings it was made for, and without the filter.
struct FilterPreview {
    centre: Pos2,
    key: (LayerFilter, (u32, u32)),
    filtered: egui::TextureHandle,
    unfiltered: egui::TextureHandle,
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

/// A Batch Export running in the background: one result per file, and
/// the files that failed so far, with why.
struct BatchJob {
    total: usize,
    done: usize,
    failed: Vec<String>,
    rx: Receiver<Result<PathBuf, String>>,
}

/// One step of an `OMAPIX_SCRIPT` (comma-separated steps):
/// - a command name, e.g. `FrequencySeparation` (radius commands use their
///   default radius);
/// - `Stroke x0 y0 x1 y1`: a brush stroke with the current tool, in image
///   pixels, through the same path as mouse strokes;
/// - `Tool Move|Brush|Eraser|Clone|Heal|SpotHeal|Marquee|Lasso|Crop`, `Size n`, `Opacity percent`,
///   `Color r g b` (sRGB), `Source x y` (clone/heal source, like Alt+click),
///   `Look x y` (centre the view on an image point at 100 %),
///   `View image|mask|overlay|texture r|tone r|blur r` (what the canvas shows);
/// - `AutoRetouch Natural|Standard|Strong`: Auto Retouch with that preset
///   (or, with none, as last used), OK'd once the faces are found;
/// - `GenerativeFill a prompt`: Generative Fill of the selection (with no
///   prompt, what's selected is removed), OK'd once a result is made.
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
    AutoRetouch(Option<crate::auto_retouch::Preset>),
    GenerativeFill(String),
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
            ("AutoRetouch", _) => ScriptStep::AutoRetouch(match words.next() {
                Some(name) => Some(crate::auto_retouch::Preset::from_name(name)?),
                None => None,
            }),
            ("GenerativeFill", _) => ScriptStep::GenerativeFill(words.collect::<Vec<_>>().join(" ")),
            ("BlendIf", &[a, b, c, d]) => {
                let under = step.split_whitespace().any(|w| w == "under");
                ScriptStep::BlendIf(under, [a / 255.0, b / 255.0, c / 255.0, d / 255.0])
            }
            ("View", nums) => match (words.next()?, nums) {
                ("image", _) => ScriptStep::View(View::Image),
                ("mask", _) => ScriptStep::View(View::Mask(0)),
                ("overlay", _) => ScriptStep::View(View::MaskOverlay(0)),
                ("quickmask", _) => ScriptStep::View(View::QuickMask),
                ("texture", &[radius]) => ScriptStep::View(View::Separation {
                    fine: radius,
                    coarse: None,
                    band: SeparationBand::Texture,
                }),
                ("mid", &[fine, coarse]) => ScriptStep::View(View::Separation {
                    fine,
                    coarse: Some(coarse),
                    band: SeparationBand::Mid,
                }),
                ("tone", &[radius]) => ScriptStep::View(View::Separation {
                    fine: radius,
                    coarse: None,
                    band: SeparationBand::Tone,
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
                "Gradient" => ScriptStep::Tool(crate::tools::Tool::Gradient),
                "PaintBucket" | "Bucket" => ScriptStep::Tool(crate::tools::Tool::PaintBucket),
                "Crop" => ScriptStep::Tool(crate::tools::Tool::Crop),
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
    /// The image showing.
    editor: Option<Editor>,
    /// The other open images, in the order of their tabs.
    parked: Vec<Parked>,
    /// Where the image showing goes among them.
    tab: usize,
    layers: LayersPanel,
    properties: PropertiesPanel,
    history: HistoryPanel,
    recent: RecentStore,
    top_tab: Option<TopTab>,
    histogram: HistogramPanel,
    navigator: NavigatorPanel,
    right_tab: RightTab,
    tools: Tools,
    /// Where the tool options are kept, and what was last written there.
    tools_path: Option<PathBuf>,
    saved_tools: String,
    /// Files being opened in the background, shown in the order asked for.
    opening: VecDeque<(PathBuf, Receiver<Opened>)>,
    /// A file dialog open in the background.
    picking: Option<(Purpose, Receiver<Option<Vec<PathBuf>>>)>,
    /// Photos being merged into a new image: what's doing it, and the image
    /// with the names of the photos it left out.
    merging: Option<(Command, Receiver<Result<Merged, String>>)>,
    file_job: Option<FileJob>,
    batch_export: Option<BatchJob>,
    dialog: Option<Dialog>,
    filter_preview: Option<FilterPreview>,
    /// A transient message for the status bar, and whether it's an error.
    status: Option<(String, bool, Instant)>,
    /// Filter settings as last used, kept between runs.
    filters: FilterSettings,
    /// What the dialogs' Defaults buttons go back to.
    defaults: FilterSettings,
    /// The user chose to discard changes, so the next close goes through.
    allow_close: bool,
    title: String,
    /// A selection being drawn: its points so far and how it will combine
    /// with the current selection.
    drawing: Option<(Vec<Pos2>, Combine)>,
    /// Where a Move tool drag started, in image pixels.
    move_from: Option<Pos2>,
    /// A drag on Free Transform's box.
    transform_drag: Option<crate::free_transform::Drag>,
    /// The Crop tool's box, (x0, y0, x1, y1) in image pixels; `None` is
    /// round the whole image.
    crop: Option<[f64; 4]>,
    crop_drag: Option<CropDrag>,
    /// Where Liquify's brush is while its button is held.
    liquify_at: Option<Pos2>,
    /// The Object Selection tool's model, running on its own thread.
    objects: crate::object_selection::ObjectSelection,
    /// Steps to run once an image is open, from `OMAPIX_SCRIPT`. For testing
    /// the real UI without a mouse; see [`ScriptStep`].
    script: VecDeque<ScriptStep>,
    clipboard: Clipboard,
    /// An image being read from the system clipboard or dropped to paste or place.
    pasting: Option<Receiver<Result<PendingClip, String>>>,
    /// A new image being made from the clipboard.
    from_clipboard: Option<Receiver<Result<Document, String>>>,
    /// Dropped files queued to place once opening finishes.
    pending_drops: Vec<PathBuf>,
    /// Whether V is held, for spotting Ctrl+V (see [`Command::pressed`]).
    v_down: bool,
    /// A pen tablet, on Wayland.
    tablet: Option<Tablet>,
    /// The monitor Omapix is on, for showing images in its colours.
    monitor: Option<crate::monitor::Watch>,
    proof: ProofView,
    content_fill: ContentFill,
    denoising: crate::denoise::Denoising,
    upscaling: crate::upscale::Upscaling,
    /// Auto-Align Layers under way: the names of the layers nothing lined
    /// up with, once it's done (`None` if there weren't two to line up).
    aligning: Option<Receiver<Option<Vec<String>>>>,
    select_subject: SelectSubject,
    face_selection: FaceSelection,
    whitening: crate::whiten::Whitening,
    face_liquify: crate::face_liquify::FaceLiquify,
    body_liquify: crate::body_liquify::BodyLiquify,
    /// Which AI models are on disk, by id, as last checked: on a thread at
    /// startup (finding a shared model means reading it through), and
    /// whenever Help › AI Models opens.
    models: HashMap<&'static str, bool>,
    models_check: Option<Receiver<HashMap<&'static str, bool>>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>, round_trip: bool) -> Self {
        let ctx = &cc.egui_ctx;
        // Ctrl+= / Ctrl+- zoom the image, not the interface.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        theme::install_font(ctx);
        if let Some(render_state) = &cc.wgpu_render_state {
            crate::gpu::install(render_state);
        }
        crate::drop::listen(ctx.clone());
        let theme = Theme::load();
        ctx.set_visuals(theme.visuals());

        let (filters, defaults) = FilterSettings::load();
        let tools_path = crate::recent::config_dir().map(|dir| dir.join("tools.toml"));
        let tools = tools_path
            .as_deref()
            .map_or_else(Tools::default, |path| crate::settings::load_from(path, &Tools::default()));
        let saved_tools = toml::to_string(&tools).unwrap_or_default();
        let (mut filters, mut proof) = (filters, ProofView::default());
        if let Some(path) = filters.proof_profile.take() {
            match ProofView::read(&path) {
                Ok(profile) => (proof.profile, filters.proof_profile) = (profile, Some(path)),
                Err(e) => log::warn!("can't proof for {}: {e}", path.display()),
            }
        }
        let mut app = Self {
            theme,
            theme_rx: theme::watch(ctx.clone()),
            editor: None,
            parked: Vec::new(),
            tab: 0,
            layers: LayersPanel::default(),
            properties: PropertiesPanel::default(),
            history: HistoryPanel,
            recent: RecentStore::load(),
            top_tab: None,
            histogram: HistogramPanel::default(),
            navigator: NavigatorPanel::default(),
            right_tab: RightTab::default(),
            drawing: None,
            move_from: None,
            transform_drag: None,
            crop: None,
            crop_drag: None,
            liquify_at: None,
            objects: Default::default(),
            tools,
            tools_path,
            saved_tools,
            opening: VecDeque::new(),
            picking: None,
            merging: None,
            file_job: None,
            batch_export: None,
            dialog: None,
            filter_preview: None,
            status: None,
            filters,
            defaults,
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
            from_clipboard: None,
            pending_drops: Vec::new(),
            v_down: false,
            tablet: Tablet::connect(cc),
            monitor: Some(crate::monitor::Watch::new()),
            proof,
            content_fill: ContentFill::default(),
            denoising: Default::default(),
            upscaling: Default::default(),
            aligning: None,
            select_subject: SelectSubject::default(),
            face_selection: FaceSelection::default(),
            whitening: Default::default(),
            face_liquify: Default::default(),
            body_liquify: Default::default(),
            models: HashMap::new(),
            models_check: Some(check_models(ctx)),
        };
        let warnings = crate::hotkeys::load();
        if !warnings.is_empty() {
            for w in &warnings {
                log::warn!("{w}");
            }
            app.message(warnings.join("; "), true);
        }

        for path in paths {
            if round_trip {
                app.open_with(path, omapix_engine::io::load_round_trip, ctx);
            } else {
                app.open(path, ctx);
            }
        }
        app
    }

    fn message(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some((text.into(), error, Instant::now()));
    }

    fn modified(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.modified)
    }

    /// The open images in the order of their tabs.
    fn tabs(&self) -> impl Iterator<Item = &Editor> {
        let (before, after) = self.parked.split_at(self.tab.min(self.parked.len()));
        let before = before.iter().map(|p| &p.editor);
        before.chain(&self.editor).chain(after.iter().map(|p| &p.editor))
    }

    /// Why the image showing can't be left for another just now: work on
    /// it that would end up on the other one.
    fn held(&self) -> Option<&'static str> {
        let editor = self.editor.as_ref()?;
        let busy = self.dialog.is_some()
            || self.pasting.is_some()
            || self.picking.as_ref().is_some_and(|(purpose, _)| !matches!(purpose, Purpose::Open))
            || editor.busy().is_some()
            || editor.liquifying()
            || self.objects.busy()
            || self.content_fill.busy()
            || self.denoising.busy().is_some()
            || self.upscaling.busy().is_some()
            || self.select_subject.busy()
            || self.face_selection.busy().is_some()
            || self.whitening.busy().is_some();
        busy.then_some("This image is still busy — try again when it's done")
    }

    /// Put the image showing among the others, with its panels as they are.
    fn park(&mut self) {
        let Some(editor) = self.editor.take() else { return };
        let (layers, properties) = (std::mem::take(&mut self.layers), std::mem::take(&mut self.properties));
        self.parked.insert(self.tab, Parked { editor, layers, properties });
        // Drags and the Crop tool's box were that image's.
        self.drawing = None;
        self.move_from = None;
        self.transform_drag = None;
        self.crop = None;
        self.crop_drag = None;
    }

    /// Show the image at `index` of the others, which is where its tab is.
    fn unpark(&mut self, index: usize) {
        let Parked { editor, layers, properties } = self.parked.remove(index);
        (self.editor, self.layers, self.properties) = (Some(editor), layers, properties);
        self.tab = index;
    }

    /// Show the image in tab `index`. Says why not, when the image showing
    /// can't be left.
    fn show_tab(&mut self, index: usize) -> bool {
        if index == self.tab {
            return true;
        }
        if let Some(why) = self.held() {
            self.message(why, true);
            return false;
        }
        self.park();
        self.unpark(index);
        true
    }

    /// Show `editor` in a new tab at the end, or leave it there behind an
    /// image that can't be left.
    fn add_tab(&mut self, mut editor: Editor) {
        let display = self.monitor.as_ref().and_then(|m| m.profile());
        if (display.is_some() || self.proof.proof().is_some())
            && let Err(e) = editor.set_display(display, self.proof.proof())
        {
            log::warn!("can't show {} in the display's colours: {e}", editor.doc.file_name());
        }
        if self.held().is_some() {
            let (layers, properties) = Default::default();
            self.parked.push(Parked { editor, layers, properties });
            return;
        }
        self.park();
        self.tab = self.parked.len();
        self.editor = Some(editor);
    }

    /// Show every open image in the display's colours as they are now: the
    /// monitor's profile, and the proof if one's on.
    fn redisplay(&mut self) {
        let display = self.monitor.as_ref().and_then(|m| m.profile());
        let proof = self.proof.proof();
        let mut failed = None;
        for editor in self.editor.iter_mut().chain(self.parked.iter_mut().map(|p| &mut p.editor)) {
            failed = editor.set_display(display, proof).err().or(failed);
        }
        self.layers.forget_thumbnails();
        self.parked.iter_mut().for_each(|p| p.layers.forget_thumbnails());
        self.navigator.forget_thumbnail();
        if let Some(e) = failed {
            self.message(format!("Couldn't show it in the display's colours: {e}"), true);
        }
    }

    /// Proof Setup › Custom Profile: proof for the ICC profile at `path`
    /// from now on, and turn Proof Colors on, as Photoshop does.
    fn set_proof_profile(&mut self, path: PathBuf) {
        match ProofView::read(&path) {
            Ok(profile) => {
                self.proof.profile = profile;
                self.message(format!("Proofing for {}", self.proof.name()), false);
                self.proof.colors = true;
                self.filters.proof_profile = Some(path);
                self.filters.save();
                self.redisplay();
            }
            Err(e) => self.message(format!("Can't proof for {}: {e}", path.display()), true),
        }
    }

    /// Close the image in tab `index`, asking about unsaved changes first.
    fn close_tab(&mut self, index: usize, ctx: &egui::Context) {
        if index != self.tab && self.tabs().nth(index).is_some_and(|e| !e.modified) {
            self.parked.remove(index - usize::from(index > self.tab));
            self.tab -= usize::from(index < self.tab);
        } else if self.show_tab(index) {
            match self.held() {
                Some(why) => self.message(why, true),
                None => self.guard(Then::Close, ctx),
            }
        }
    }

    /// Show a new image in a tab of its own.
    fn new_image(&mut self, doc: Document) {
        match Editor::new(doc) {
            Ok(editor) => self.add_tab(editor),
            Err(err) => self.message(format!("Couldn't make a new image: {err}"), true),
        }
    }

    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.open_with(path, omapix_engine::io::load, ctx);
    }

    /// Open `path` with `load` in the background, in a new tab. An image
    /// that's already open is shown instead.
    fn open_with(
        &mut self,
        path: PathBuf,
        load: fn(&Path) -> omapix_engine::Result<Document>,
        ctx: &egui::Context,
    ) {
        let open = |e: &Editor| [Some(&e.doc.path), e.doc.saved_path.as_ref(), e.doc.round_trip.as_ref()].contains(&Some(&path));
        let open = self.tabs().position(open);
        if let Some(index) = open {
            self.show_tab(index);
            return;
        }
        if self.opening.iter().any(|(opening, _)| *opening == path) {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let target = path.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = load(&target)
                .and_then(|mut doc| {
                    let converted = ops::prepare_for_editing(&mut doc)?;
                    Ok((doc, converted))
                })
                .map_err(|e| e.to_string());
            log::info!("opened {} in {:?}", target.display(), started.elapsed());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.opening.push_back((path, rx));
    }

    fn place_files(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for path in paths {
                let res = load_clip(&path).map(|(clip, name)| PendingClip {
                    clip,
                    name: Some(name),
                    kind: PasteKind::Normal,
                });
                let _ = tx.send(res);
                ctx.request_repaint();
            }
        });
        self.pasting = Some(rx);
    }

    /// Photomerge: `paths` lined up as the layers of a new image, in a tab
    /// of its own, put together in the background.
    fn photomerge(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        if paths.len() < 2 {
            self.message("Photomerge needs two or more photos", true);
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let merged = load_frames(&paths).and_then(|(frames, profile)| {
                let (mut doc, left) = align::photomerge(&frames, profile)?;
                // Saved beside the photos, unless somewhere else is chosen.
                doc.path = paths[0].with_file_name("Panorama");
                Ok((doc, left))
            });
            log::info!("merged {} photos in {:?}", paths.len(), started.elapsed());
            let _ = tx.send(merged);
            ctx.request_repaint();
        });
        self.merging = Some((Command::Photomerge, rx));
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
                .add_filter("Images", &OPEN_EXTENSIONS)
                .add_filter("Photoshop", &["psd"]),
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
            Purpose::ExportPng => dialog
                .set_title("Export as PNG")
                .add_filter("PNG", &["png"])
                .set_file_name(format!("{stem}.png")),
            Purpose::ExportLut => dialog
                .set_title("Export Adjustments as LUT")
                .add_filter("Cube LUT", &["cube"])
                .set_file_name(format!("{}.cube", self.preset_name())),
            Purpose::Photomerge => dialog.set_title("Photomerge").add_filter("Images", &OPEN_EXTENSIONS),
            Purpose::ProofProfile => {
                // Where the last one was, or where profiles are installed.
                let last = self.filters.proof_profile.as_deref().and_then(Path::parent);
                let installed = Path::new("/usr/share/color/icc");
                let dialog = dialog.set_title("Proof Profile").add_filter("ICC profile", &["icc", "icm"]);
                match last.or(installed.is_dir().then_some(installed)) {
                    Some(dir) => dialog.set_directory(dir),
                    None => dialog,
                }
            }
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let files = match purpose {
                Purpose::Photomerge => dialog.pick_files(),
                Purpose::Open | Purpose::ProofProfile => dialog.pick_file().map(|f| vec![f]),
                _ => dialog.save_file().map(|f| vec![f]),
            };
            let _ = tx.send(files);
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
            Purpose::ExportTiff | Purpose::ExportJpeg | Purpose::ExportPng => "Exporting",
            _ => "Saving",
        };
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = match purpose {
                Purpose::ExportTiff => export::tiff(&doc, &path).map(|_| false),
                Purpose::ExportJpeg => export::jpeg(&doc, &path, JPEG_QUALITY).map(|_| false),
                Purpose::ExportPng => export::png(&doc, &path).map(|_| false),
                // A round trip from darktable also updates its TIFF.
                _ => ora::save(&doc, &path)
                    .and_then(|_| doc.round_trip.as_ref().map_or(Ok(()), |tiff| export::tiff(&doc, tiff)))
                    .map(|_| true),
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
        // Quitting closes every image: each with unsaved changes is shown
        // and asked about in turn.
        let unsaved = self.tabs().position(|e| e.modified);
        if matches!(then, Then::Quit)
            && !self.modified()
            && let Some(index) = unsaved
            && !self.show_tab(index)
        {
            return;
        }
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
            Then::Close => self.close_document(),
        }
    }

    /// Make a new image of what's on the clipboard, read in the background.
    fn new_from_clipboard(&mut self, ctx: &egui::Context) {
        let ours = self.clipboard.current();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let clip = ours.map_or_else(|| crate::clipboard::read_system().map(Arc::new), Ok);
            let _ = tx.send(clip.and_then(|clip| clip.document().map_err(|e| e.to_string())));
            ctx.request_repaint();
        });
        self.from_clipboard = Some(rx);
    }

    fn close_document(&mut self) {
        self.editor = None;
        self.layers = LayersPanel::default();
        self.properties = PropertiesPanel::default();
        self.history = HistoryPanel;
        self.drawing = None;
        self.move_from = None;
        self.transform_drag = None;
        self.crop = None;
        self.pasting = None;
        self.dialog = None;
        self.pending_drops.clear();
        // The tab after it takes its place, or the one before.
        if !self.parked.is_empty() {
            self.unpark(self.tab.min(self.parked.len() - 1));
        }
    }

    /// Pick up results from background work.
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.models_check
            && let Ok(models) = rx.try_recv()
        {
            self.models = models;
            self.models_check = None;
        }
        if let Some(theme) = self.theme_rx.try_iter().last() {
            ctx.set_visuals(theme.visuals());
            self.theme = theme;
        }
        // On another monitor, every image is shown in its colours.
        if self.monitor.as_mut().is_some_and(|m| m.check(ctx)) {
            self.redisplay();
        }
        if let Some(editor) = &mut self.editor {
            // The threshold slider cuts the last AI selection again.
            if let Some(commit) = self.tools.threshold_moved.take() {
                self.objects.threshold(editor, self.tools.ai_threshold, commit);
            }
            let errors = [
                self.objects.poll(ctx, editor),
                self.content_fill.poll(editor),
                self.denoising.poll(editor),
                self.upscaling.poll(editor),
                self.select_subject.poll(editor),
                self.face_selection.poll(editor),
                self.whitening.poll(editor),
            ];
            for e in errors.into_iter().flatten() {
                self.message(e, true);
            }
            if let Some(left) = self.aligning.as_ref().and_then(|rx| rx.try_recv().ok()) {
                self.aligning = None;
                match left {
                    None => self.message("Auto-Align Layers needs two layers that aren't locked", true),
                    Some(left) if !left.is_empty() => {
                        self.message(format!("Nothing lines up with {}", left.join(", ")), true);
                    }
                    Some(_) => {}
                }
            }
        }
        if let Some((purpose, rx)) = &self.picking
            && let Ok(result) = rx.try_recv()
        {
            let purpose = *purpose;
            self.picking = None;
            let result = match (purpose, result) {
                (Purpose::Photomerge, Some(paths)) => {
                    self.photomerge(paths, ctx);
                    None
                }
                (_, result) => result.and_then(|paths| paths.into_iter().next()),
            };
            if let Some(path) = result {
                match purpose {
                    Purpose::Open => self.open(path, ctx),
                    Purpose::SaveAs => self.write(purpose, path.with_extension("ora"), ctx),
                    Purpose::ExportLut => self.export_lut(&path.with_extension("cube")),
                    Purpose::ProofProfile => self.set_proof_profile(path),
                    _ => self.write(purpose, path, ctx),
                }
            }
        }
        if let Some((cmd, rx)) = &self.merging
            && let Ok(result) = rx.try_recv()
        {
            let name = cmd.label().trim_end_matches('…');
            self.merging = None;
            match result {
                Ok((doc, left)) => {
                    self.new_image(doc);
                    if !left.is_empty() {
                        self.message(format!("{name} left out {}: nothing lines up with them", left.join(", ")), true);
                    }
                }
                Err(err) => self.message(format!("{name}: {err}"), true),
            }
        }
        while let Some(result) = self.opening.front().and_then(|(_, rx)| rx.try_recv().ok()) {
            let Some((path, _)) = self.opening.pop_front() else { break };
            match result.and_then(|(doc, converted)| Ok((Editor::new(doc)?, converted))) {
                Ok((editor, converted)) => {
                    self.recent.add(&path);
                    if let Some(original) = converted {
                        let now = editor.doc.profile.description().to_owned();
                        self.message(format!("Converted {original} to {now} for editing"), false);
                    }
                    self.add_tab(editor);
                    if !self.pending_drops.is_empty() {
                        let paths = std::mem::take(&mut self.pending_drops);
                        self.place_files(paths, ctx);
                    }
                }
                Err(err) => {
                    self.pending_drops.clear();
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
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let verb = if native { "Saved" } else { "Exported" };
                    let mut text = format!("{verb} {name}");
                    // The image saved may be in another tab by now, or closed.
                    let parked = self.parked.iter_mut().map(|p| &mut p.editor);
                    let saved = self.editor.iter_mut().chain(parked).find(|e| native && e.owns(revision));
                    if let Some(editor) = saved {
                        editor.doc.saved_path = Some(path.clone());
                        editor.mark_saved(revision);
                        self.recent.add(&path);
                        if let Some(tiff) = editor.doc.round_trip.as_ref().and_then(|t| t.file_name()) {
                            text += &format!(", and {} for darktable", tiff.to_string_lossy());
                        }
                    }
                    self.message(text, false);
                }
                Err(err) => self.message(format!("{label} failed: {err}"), true),
            }
        }
        if let Some(job) = &mut self.batch_export {
            for result in job.rx.try_iter() {
                job.done += 1;
                job.failed.extend(result.err());
            }
            if job.done == job.total {
                let written = job.total - job.failed.len();
                let plural = if written == 1 { "" } else { "s" };
                let text = match job.failed.as_slice() {
                    [] => format!("Exported {written} file{plural}"),
                    failed => format!("Exported {written} file{plural}; {} failed: {}", failed.len(), failed.join("; ")),
                };
                let failed = !job.failed.is_empty();
                self.batch_export = None;
                self.message(text, failed);
            }
        }
        if let Some(rx) = &self.from_clipboard
            && let Ok(result) = rx.try_recv()
        {
            self.from_clipboard = None;
            match result {
                Ok(doc) => self.new_image(doc),
                Err(err) => self.message(err, true),
            }
        }
        if let Some(rx) = &self.pasting
            && self.editor.as_ref().is_some_and(|e| e.busy().is_none())
        {
            match rx.try_recv() {
                Ok(Ok(item)) => {
                    if let Some(editor) = &mut self.editor {
                        paste(editor, Arc::new(item.clip), item.name, item.kind, ctx);
                    }
                }
                Ok(Err(err)) => self.message(err, true),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pasting = None,
            }
        }
        let (dropped_paths, shift) = ctx.input(|i| {
            (
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .collect::<Vec<PathBuf>>(),
                i.modifiers.shift,
            )
        });
        if !dropped_paths.is_empty() {
            let mut image_files = Vec::new();
            for path in dropped_paths {
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
                } else {
                    image_files.push(path);
                }
            }
            if !image_files.is_empty() {
                if shift {
                    for path in image_files {
                        self.open(path, ctx);
                    }
                } else if self.editor.is_some() {
                    self.place_files(image_files, ctx);
                } else {
                    let first = image_files.remove(0);
                    self.open(first, ctx);
                    self.pending_drops = image_files;
                }
            }
        }
        // Closing the window with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.tabs().any(|e| e.modified) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.guard(Then::Quit, ctx);
        }
    }

    fn enabled(&self, cmd: Command) -> bool {
        let Some(editor) = &self.editor else {
            return matches!(
                cmd,
                Command::New
                    | Command::NewFromClipboard
                    | Command::Paste
                    | Command::Open
                    | Command::Quit
                    | Command::BatchExport
                    | Command::Photomerge
                    | Command::AiModels
            )
                || (cmd == Command::ReopenLast && self.recent.last().is_some());
        };
        let view = matches!(
            cmd,
            Command::ZoomIn | Command::ZoomOut | Command::FitOnScreen | Command::ActualPixels
        );
        if editor.busy().is_some() && !view && !matches!(cmd, Command::Quit) {
            return false;
        }
        if !missing_models(cmd, &self.models).is_empty() {
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
            Command::Close => self.held().is_none(),
            Command::NextImage | Command::PreviousImage => !self.parked.is_empty(),
            Command::ShowLayers
            | Command::ShowChannels
            | Command::ShowHistory
            | Command::ShowNavigator
            | Command::ShowHistogram => true,
            Command::SaveSelection => editor.doc.selection.is_some(),
            Command::SaveAdjustmentPreset | Command::ExportAdjustmentLut => {
                Preset::from_layers(doc, &editor.selected()).is_some()
            }
            Command::DeleteChannel => matches!(editor.view(), View::Alpha(_)),
            Command::Undo => editor.undo_label().is_some() || editor.transform().is_some(),
            Command::FreeTransform => layer.is_some() && editor.transform().is_none(),
            Command::AutoAlignLayers => {
                let pixels = |id: &u64| doc.layer(*id).is_some_and(|l| l.has_pixels());
                editor.selected().iter().filter(|id| pixels(id)).count() > 1
            }
            Command::Redo => editor.redo_label().is_some(),
            // Something must be left, and layer must be deletable.
            Command::DeleteLayer => {
                doc.removed_count(&editor.selected()) < doc.layers.len()
                    && !editor.selected().iter().any(|&id| doc.layer(id).is_some_and(|l| !l.can_delete()))
            }
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
            Command::AddMask | Command::AddMaskHideAll => !has_mask,
            Command::LockTransparent | Command::LockPixels => !no_pixels,
            Command::LockPosition | Command::LockAll => layer.is_some(),
            Command::LoadSelectionTransparency => !no_pixels,
            Command::LoadSelectionLayerMask => has_mask,
            Command::Deselect
            | Command::InvertSelection
            | Command::BorderSelection
            | Command::SmoothSelection
            | Command::ExpandSelection
            | Command::ContractSelection
            | Command::Feather => {
                editor.doc.selection.is_some()
            }
            Command::ContentAwareFill => editor.doc.selection.is_some() && !self.content_fill.busy(),
            Command::Denoise => self.denoising.busy().is_none(),
            Command::ImageSize => self.upscaling.busy().is_none(),
            Command::SelectSubject => !self.select_subject.busy(),
            Command::SelectSkin
            | Command::SelectHair
            | Command::SelectEyes
            | Command::SelectLips
            | Command::SelectTeeth => self.face_selection.busy().is_none(),
            Command::ReduceShine | Command::LightenUnderEyes | Command::WhitenTeeth | Command::WhitenEyes => {
                self.whitening.busy().is_none()
            }
            Command::Crop => editor.doc.selection.is_some(),
            Command::Liquify => editor.target == Target::Pixels && !no_pixels && !editor.liquifying(),
            Command::SelectAndMask => editor.doc.selection.is_some(),
            Command::FillForeground
            | Command::FillBackground
            | Command::Clear
            | Command::Cut
            | Command::Copy => {
                editor.target == Target::Mask || editor.target == Target::QuickMask || !no_pixels
            }
            Command::Paste | Command::PasteInPlace => self.pasting.is_none(),
            Command::PasteInto => editor.doc.selection.is_some() && self.pasting.is_none(),
            Command::DeleteMask
            | Command::ToggleMask
            | Command::MaskOverlay
            | Command::MaskDensity => has_mask,
            Command::GaussianBlur
            | Command::SmartBlur
            | Command::HighPass
            | Command::UnsharpMask
            | Command::SmartSharpen
            | Command::ReduceNoise => {
                if editor.target == Target::Mask {
                    has_mask
                } else {
                    !no_pixels
                }
            }
            Command::AddNoise => editor.target == Target::Pixels || has_mask,
            Command::Invert => {
                editor.target == Target::Mask || editor.target == Target::QuickMask || !no_pixels
            }
            _ => true,
        }
    }

    fn run(&mut self, cmd: Command, ctx: &egui::Context) {
        // Delete with several layers selected deletes them, as in Photoshop.
        let cmd = match (cmd, &self.editor) {
            (Command::Clear, Some(e)) if e.several_selected() => Command::DeleteLayer,
            // With nothing open, pasting makes a new image.
            (Command::Paste, None) => Command::NewFromClipboard,
            _ => cmd,
        };
        if !self.enabled(cmd) {
            return;
        }
        // A Free Transform is applied before the document is saved or closed.
        let finishing = [
            Command::Close,
            Command::Quit,
            Command::Save,
            Command::SaveAs,
            Command::ExportTiff,
            Command::ExportJpeg,
            Command::ExportPng,
            Command::Rotate180,
            Command::Rotate90Cw,
            Command::Rotate90Ccw,
            Command::FlipCanvasHorizontal,
            Command::FlipCanvasVertical,
            Command::ImageSize,
            Command::CanvasSize,
            Command::Crop,
        ];
        // Liquify is applied before anything but zooming, and undoes its
        // own strokes.
        let zoom = [Command::ZoomIn, Command::ZoomOut, Command::FitOnScreen, Command::ActualPixels, Command::Undo, Command::Redo];
        if !zoom.contains(&cmd)
            && let Some(editor) = &mut self.editor
        {
            editor.commit_liquify();
        }
        if finishing.contains(&cmd) {
            (self.transform_drag, self.crop) = (None, None);
            if let Some(editor) = &mut self.editor {
                editor.commit_transform();
            }
        }
        match cmd {
            Command::New => {
                let size = self.editor.as_ref().map_or(NEW_IMAGE_SIZE, |e| (e.doc.width, e.doc.height));
                self.dialog = Some(Dialog::NewImage { width: size.0, height: size.1 });
            }
            Command::NewFromClipboard => self.new_from_clipboard(ctx),
            Command::Open => self.pick(Purpose::Open, ctx),
            Command::Photomerge => self.pick(Purpose::Photomerge, ctx),
            Command::Close => self.guard(Then::Close, ctx),
            Command::ReopenLast => {
                if let Some(path) = self.recent.last().cloned() {
                    self.open(path, ctx);
                }
            }
            Command::NextImage | Command::PreviousImage => {
                let tabs = self.parked.len() + 1;
                let step = if cmd == Command::NextImage { 1 } else { tabs - 1 };
                // Not in the middle of a stroke or a drag.
                if !ctx.input(|i| i.pointer.any_down()) {
                    self.show_tab((self.tab + step) % tabs);
                }
            }
            Command::ShowLayers => self.right_tab = RightTab::Layers,
            Command::ShowChannels => self.right_tab = RightTab::Channels,
            Command::ShowHistory => self.right_tab = RightTab::History,
            Command::ShowNavigator => TopTab::toggle(&mut self.top_tab, TopTab::Navigator),
            Command::ShowHistogram => TopTab::toggle(&mut self.top_tab, TopTab::Histogram),
            Command::Quit => self.guard(Then::Quit, ctx),
            Command::Save => self.save(ctx),
            Command::SaveAs => self.pick(Purpose::SaveAs, ctx),
            Command::ExportTiff => self.pick(Purpose::ExportTiff, ctx),
            Command::ExportJpeg => self.pick(Purpose::ExportJpeg, ctx),
            Command::ExportPng => self.pick(Purpose::ExportPng, ctx),
            Command::ExportAdjustmentLut => self.pick(Purpose::ExportLut, ctx),
            Command::BatchExport => {
                self.dialog = Some(Dialog::BatchExport {
                    files: Vec::new(),
                    settings: self.filters.batch_export.clone(),
                    picking: None,
                });
            }
            Command::AiModels => {
                let models = omapix_ai::models::models();
                let found: Vec<_> = models.iter().map(|m| omapix_ai::find_model(m.id)).collect();
                self.models = models.iter().zip(&found).map(|(m, f)| (m.id, f.is_some())).collect();
                self.dialog = Some(Dialog::AiModels(found));
            }
            Command::Finish => self.finish(),
            // On a mask, noise goes straight into it; on pixels, onto a
            // Grain layer.
            Command::AddNoise if self.editor.as_ref().is_some_and(|e| e.target == Target::Mask) => {
                self.dialog = Some(Dialog::Filter {
                    filter: LayerFilter::AddNoise(self.filters.noise),
                    preview: true,
                });
            }
            Command::AddNoise => {
                self.dialog = Some(Dialog::AddNoise {
                    options: self.filters.noise,
                    preview: true,
                });
            }
            Command::Liquify => {
                if let Some(editor) = &mut self.editor
                    && let Err(why) = editor.begin_liquify()
                {
                    self.message(why, true);
                }
            }
            Command::ImageSize | Command::CanvasSize => {
                let Some(doc) = self.editor.as_ref().map(|e| &e.doc) else {
                    return;
                };
                let (width, height) = (doc.width, doc.height);
                self.dialog = Some(if cmd == Command::ImageSize {
                    Dialog::ImageSize { width, height, constrain: true, ai: false }
                } else {
                    Dialog::CanvasSize { width, height, anchor: (1, 1), extension: Extension::Background }
                });
            }
            Command::MaskDensity => {
                if let Some(editor) = &mut self.editor {
                    editor.target = Target::Mask;
                }
                self.dialog = Some(Dialog::Filter {
                    filter: LayerFilter::MaskDensity {
                        density: self.filters.mask_density,
                    },
                    preview: true,
                });
            }
            Command::GaussianBlur
            | Command::SmartBlur
            | Command::HighPass
            | Command::UnsharpMask
            | Command::SmartSharpen
            | Command::ReduceNoise => {
                if let Some(editor) = &self.editor
                    && editor.target == Target::Pixels
                    && editor.doc.layer(editor.active).is_some_and(|l| !l.can_paint_pixels())
                {
                    self.message("Could not use the filter because the layer is locked", true);
                    return;
                }
                let settings = &self.filters;
                let filter = match cmd {
                    Command::GaussianBlur => LayerFilter::GaussianBlur { radius: settings.blur_radius },
                    Command::SmartBlur => LayerFilter::SmartBlur(settings.smart_blur),
                    Command::HighPass => LayerFilter::HighPass { radius: settings.high_pass_radius },
                    Command::UnsharpMask => settings.unsharp_mask.into(),
                    Command::SmartSharpen => LayerFilter::SmartSharpen(settings.smart_sharpen),
                    Command::ReduceNoise => LayerFilter::ReduceNoise(settings.reduce_noise),
                    _ => unreachable!(),
                };
                self.dialog = Some(Dialog::Filter {
                    filter,
                    preview: true,
                });
            }
            Command::BorderSelection
            | Command::SmoothSelection
            | Command::ExpandSelection
            | Command::ContractSelection
            | Command::Feather => {
                let radius = modify_radius(&mut self.filters, cmd).map_or(1.0, |r| *r);
                self.dialog = Some(Dialog::Radius {
                    command: cmd,
                    radius,
                    coarse: None,
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
            Command::SelectAndMask => {
                let (options, output) = (self.filters.select_and_mask, self.filters.select_and_mask_output);
                self.dialog = self
                    .editor
                    .as_ref()
                    .and_then(|editor| crate::select_and_mask::SelectAndMask::open(ctx, editor, options, output))
                    .map(Dialog::SelectAndMask);
            }
            Command::SaveAdjustmentPreset => {
                self.dialog = Some(Dialog::SavePreset {
                    name: self.preset_name(),
                });
            }
            Command::ContentAwareFill => {
                if let Some(editor) = &self.editor
                    && let Err(e) = self.content_fill.start(ctx, editor)
                {
                    self.message(e, true);
                }
            }
            Command::GenerativeFill => {
                let Some(editor) = &mut self.editor else { return };
                match crate::generative_fill::GenerativeFill::open(ctx, editor) {
                    Ok(dialog) => self.dialog = Some(Dialog::GenerativeFill(dialog)),
                    Err(e) => self.message(e, true),
                }
            }
            Command::SelectSubject => {
                if let Some(editor) = &self.editor
                    && let Err(e) = self.select_subject.start(ctx, editor)
                {
                    self.message(e, true);
                }
            }
            Command::SelectSkin
            | Command::SelectHair
            | Command::SelectEyes
            | Command::SelectLips
            | Command::SelectTeeth => {
                let part = match cmd {
                    Command::SelectSkin => Part::Skin,
                    Command::SelectHair => Part::Hair,
                    Command::SelectEyes => Part::Eyes,
                    Command::SelectLips => Part::Lips,
                    _ => Part::Teeth,
                };
                // Shift adds to the selection, Alt takes away, as with the
                // marquees.
                let modifiers = ctx.input(|i| i.modifiers);
                let how = Combine::from_modifiers(modifiers.shift, modifiers.alt);
                if let Some(editor) = &self.editor
                    && let Err(e) = self.face_selection.start(ctx, editor, part, how)
                {
                    self.message(e, true);
                }
            }
            Command::FillForeground | Command::FillBackground | Command::Clear => {
                let Some(editor) = &mut self.editor else {
                    return;
                };
                if editor.target == Target::Pixels
                    && editor.doc.layer(editor.active).is_some_and(|l| !l.can_paint_pixels())
                {
                    let msg = match cmd {
                        Command::Clear => "Could not clear because the layer is locked",
                        _ => "Could not fill because the layer is locked",
                    };
                    self.message(msg, true);
                    return;
                }
                let colour = match cmd {
                    Command::FillForeground => Some(self.tools.foreground),
                    Command::FillBackground => Some(self.tools.background),
                    _ => None,
                };
                let background = self.tools.background;
                let label = if colour.is_some() { "Fill" } else { "Clear" };
                fill(editor, label, colour, background);
            }
            Command::Cut | Command::Copy | Command::CopyMerged => {
                let Some(editor) = &mut self.editor else {
                    return;
                };
                if cmd == Command::Cut
                    && editor.target == Target::Pixels
                    && editor.doc.layer(editor.active).is_some_and(|l| !l.can_paint_pixels())
                {
                    self.message("Could not cut because the layer is locked", true);
                    return;
                }
                let Some(clip) = copy(editor, cmd == Command::CopyMerged) else {
                    self.message("Nothing to copy: the selected area is empty", true);
                    return;
                };
                self.clipboard.set(clip);
                if cmd == Command::Cut {
                    fill(editor, "Cut", None, self.tools.background);
                }
            }
            Command::Paste | Command::PasteInPlace | Command::PasteInto => {
                let kind = match cmd {
                    Command::PasteInto => PasteKind::Into,
                    Command::PasteInPlace => PasteKind::InPlace,
                    _ => PasteKind::Normal,
                };
                let Some(editor) = &mut self.editor else {
                    return;
                };
                match self.clipboard.current() {
                    Some(clip) => paste(editor, clip, None, kind, ctx),
                    None => {
                        let (tx, rx) = channel();
                        let ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let res = crate::clipboard::read_system().map(|clip| PendingClip {
                                clip,
                                name: None,
                                kind,
                            });
                            let _ = tx.send(res);
                            ctx.request_repaint();
                        });
                        self.pasting = Some(rx);
                    }
                }
            }
            Command::HighPassSharpening => {
                self.dialog = Some(Dialog::Radius {
                    command: cmd,
                    radius: self.filters.high_pass_radius,
                    coarse: None,
                    preview: None,
                });
            }
            Command::Denoise => {
                let Some(editor) = &self.editor else { return };
                let (luminance, color) = (self.filters.denoise_luminance, self.filters.denoise_color);
                match crate::denoise::Denoise::open(ctx, editor, luminance, color) {
                    Ok(dialog) => self.dialog = Some(Dialog::Denoise(dialog)),
                    Err(e) => self.message(e, true),
                }
            }
            Command::AutoRetouch => {
                let Some(editor) = &self.editor else { return };
                match crate::auto_retouch::AutoRetouch::open(ctx, editor, self.filters.auto_retouch) {
                    Ok(dialog) => self.dialog = Some(Dialog::AutoRetouch(dialog)),
                    Err(e) => self.message(e, true),
                }
            }
            Command::HealBlemishes => {
                let Some(editor) = &self.editor else { return };
                match crate::heal_blemishes::HealBlemishes::open(ctx, editor, self.filters.blemish_sensitivity) {
                    Ok(dialog) => self.dialog = Some(Dialog::HealBlemishes(dialog)),
                    Err(e) => self.message(e, true),
                }
            }
            Command::SmoothSkin => {
                let Some(editor) = &self.editor else { return };
                match crate::smooth_skin::SmoothSkin::open(ctx, editor, self.filters.smooth_skin) {
                    Ok(dialog) => self.dialog = Some(Dialog::SmoothSkin(dialog)),
                    Err(e) => self.message(e, true),
                }
            }
            Command::EvenTone => {
                let Some(editor) = &self.editor else { return };
                match crate::even_tone::EvenTone::open(ctx, editor, self.filters.even_tone) {
                    Ok(dialog) => self.dialog = Some(Dialog::EvenTone(dialog)),
                    Err(e) => self.message(e, true),
                }
            }
            Command::ReduceShine | Command::LightenUnderEyes | Command::WhitenTeeth | Command::WhitenEyes => {
                use crate::whiten::Part;
                use omapix_engine::whiten::Whiten;
                let part = match cmd {
                    Command::ReduceShine => Part::Shine,
                    Command::LightenUnderEyes => Part::UnderEyes,
                    Command::WhitenTeeth => Part::Whites(Whiten::Teeth),
                    _ => Part::Whites(Whiten::Eyes),
                };
                if let Some(editor) = &self.editor
                    && let Err(e) = self.whitening.start(ctx, editor, part)
                {
                    self.message(e, true);
                }
            }
            Command::FrequencySeparation => {
                let Some(editor) = &self.editor else { return };
                let radius = self.filters.separation_radius.unwrap_or_else(|| separation_radius(editor));
                self.dialog = Some(Dialog::Radius {
                    command: cmd,
                    radius,
                    coarse: None,
                    preview: Some(SeparationBand::Texture),
                });
            }
            Command::FrequencySeparation3 => {
                let Some(editor) = &self.editor else { return };
                let (fine, coarse) = three_band_radii(separation_radius(editor));
                let fine = self.filters.separation3_fine.unwrap_or(fine);
                let coarse = self.filters.separation3_coarse.unwrap_or(coarse);
                self.dialog = Some(Dialog::Radius {
                    command: cmd,
                    radius: fine,
                    coarse: Some(coarse),
                    preview: Some(SeparationBand::Texture),
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
            Command::FreeTransform => {
                let background = crate::tools::grey(self.tools.background);
                if let Some(editor) = &mut self.editor
                    && let Err(why) = editor.begin_transform(background)
                {
                    self.message(why, true);
                }
            }
            Command::AutoAlignLayers => {
                if let Some(editor) = &mut self.editor {
                    let selected = editor.selected();
                    let (tx, rx) = channel();
                    editor.edit_in_background(
                        cmd.label(),
                        move |doc, _| {
                            let _ = tx.send(align::auto_align(doc, &selected));
                        },
                        ctx,
                    );
                    self.aligning = Some(rx);
                }
            }
            // Undo while transforming cancels it, as in Photoshop.
            Command::Undo if self.editor.as_ref().is_some_and(|e| e.transform().is_some()) => {
                self.transform_drag = None;
                if let Some(editor) = &mut self.editor {
                    editor.cancel_transform();
                }
            }
            Command::SelectionEdges => {
                if let Some(editor) = &mut self.editor {
                    editor.hide_selection_edges = !editor.hide_selection_edges;
                }
            }
            Command::ProofColors => {
                self.proof.colors = !self.proof.colors;
                self.redisplay();
            }
            Command::GamutWarning => {
                self.proof.gamut_warning = !self.proof.gamut_warning;
                self.redisplay();
            }
            Command::ProofSetupWeb => {
                self.proof.profile = ColorProfile::srgb();
                self.filters.proof_profile = None;
                self.filters.save();
                self.redisplay();
            }
            Command::ProofSetupCustom => self.pick(Purpose::ProofProfile, ctx),
            _ => {
                if let Some(editor) = &mut self.editor {
                    run_on_editor(editor, cmd, ctx);
                }
            }
        }
    }

    /// Image › Image Size: resample everything to `width` × `height`.
    fn resize_image(&mut self, width: u32, height: u32) {
        if let Some(editor) = &mut self.editor
            && (width, height) != (editor.doc.width, editor.doc.height)
        {
            editor.edit("Image Size", |doc, _| doc.resize_image(width, height));
        }
    }

    /// Image › Image Size with Enlarge with AI: the model starts on what
    /// the image shows, and the document is resized when it's done.
    fn upscale_image(&mut self, width: u32, height: u32, ctx: &egui::Context) {
        if let Some(editor) = &self.editor
            && self.upscaling.busy().is_none()
        {
            self.upscaling.start(ctx, editor, width, height);
        }
    }

    /// Image › Canvas Size: a `width` × `height` canvas with the image at
    /// `anchor` (0–2 across and down).
    fn resize_canvas(&mut self, width: u32, height: u32, (col, row): (u8, u8), extension: Extension) {
        let colour = match extension {
            Extension::Background => Some(self.tools.background),
            Extension::Foreground => Some(self.tools.foreground),
            Extension::White => Some([255; 3]),
            Extension::Black => Some([0; 3]),
            Extension::Transparent => None,
        };
        let Some(editor) = &mut self.editor else {
            return;
        };
        let doc = &editor.doc;
        if (width, height) == (doc.width, doc.height) {
            return;
        }
        let offset = |new: u32, old: u32, at: u8| ((i64::from(new) - i64::from(old)) * i64::from(at) / 2) as i32;
        let (dx, dy) = (offset(width, doc.width, col), offset(height, doc.height, row));
        let pixel = colour.map(|rgb| doc.profile.from_srgb8(rgb).unwrap_or([0, 0, 0, u16::MAX]));
        editor.edit("Canvas Size", |doc, _| doc.resize_canvas(width, height, dx, dy, pixel));
    }

    /// Retouch › Finish: the visible image sharpened with Unsharp Mask's
    /// settings on a Sharpen layer, then grain with Add Noise's on a Grain
    /// layer, as one step.
    fn finish(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let (sharpen, grain): (LayerFilter, _) = (self.filters.unsharp_mask.into(), self.filters.noise);
        editor.target = Target::Pixels;
        editor.edit("Finish", |doc, active| {
            *active = ops::finish(doc, Some(&sharpen), Some(&grain)).unwrap_or(*active);
        });
    }

    /// Export `files` one after another in the background, as `settings` say.
    fn batch_export(&mut self, files: Vec<PathBuf>, settings: crate::settings::BatchExport, ctx: &egui::Context) {
        let Some(dir) = settings.folder.clone() else {
            return;
        };
        let sharpen: Option<LayerFilter> = settings.sharpen.then(|| self.filters.unsharp_mask.into());
        let grain = settings.grain.then_some(self.filters.noise);
        let long_edge = settings.resize.then_some(settings.long_edge);
        let quality = settings.jpeg.then_some(settings.quality);
        let (tx, rx) = channel();
        let total = files.len();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for file in files {
                let result = omapix_engine::export::batch_file(&file, &dir, long_edge, sharpen.as_ref(), grain.as_ref(), quality);
                let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if tx.send(result.map_err(|e| format!("{name}: {e}"))).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        self.filters.batch_export = settings;
        self.filters.save();
        self.batch_export = Some(BatchJob { total, done: 0, failed: Vec::new(), rx });
    }

    fn apply_add_noise(&mut self, options: NoiseOptions, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        self.filters.noise = options;
        self.filters.save();
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
self.filters.remember(&filter);
        let Some(editor) = &mut self.editor else {
            return;
        };
        let (id, mask) = (editor.active, editor.target == Target::Mask);
        if !mask && editor.doc.layer(id).is_some_and(|l| !l.can_paint_pixels()) {
            self.message("Could not use the filter because the layer is locked", true);
            return;
        }
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

    fn apply_radius(&mut self, command: Command, radius: f32, coarse: Option<f32>, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let index = editor.active_index().unwrap_or(0);
        match command {
            Command::BorderSelection
            | Command::SmoothSelection
            | Command::ExpandSelection
            | Command::ContractSelection
            | Command::Feather => {
                if let Some(remembered) = modify_radius(&mut self.filters, command) {
                    *remembered = radius;
                }
                self.filters.save();
                editor.hide_selection_edges = false;
                let label = match command {
                    Command::BorderSelection => "Border Selection",
                    Command::SmoothSelection => "Smooth Selection",
                    Command::ExpandSelection => "Expand Selection",
                    Command::ContractSelection => "Contract Selection",
                    _ => "Feather",
                };
                editor.edit_in_background(
                    label,
                    move |doc, _| {
                        doc.selection = doc.selection.as_ref().map(|s| match command {
                            Command::BorderSelection => s.border(radius),
                            Command::SmoothSelection => s.smooth(radius),
                            Command::ExpandSelection => s.expand(radius),
                            Command::ContractSelection => s.contract(radius),
                            _ => s.feather(radius),
                        });
                    },
                    ctx,
                );
            }
            Command::HighPassSharpening => {
                self.filters.high_pass_radius = radius;
                self.filters.save();
                editor.target = Target::Pixels;
                editor.edit_in_background(
                    "High Pass Sharpening",
                    move |doc, active| *active = ops::high_pass_sharpening(doc, index, radius),
                    ctx,
                );
            }
            Command::FrequencySeparation => {
                self.filters.separation_radius = Some(radius);
                self.filters.save();
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
            Command::FrequencySeparation3 => {
                let coarse = coarse.unwrap_or(radius);
                self.filters.separation3_fine = Some(radius);
                self.filters.separation3_coarse = Some(coarse);
                self.filters.save();
                editor.target = Target::Pixels;
                editor.edit_in_background(
                    "Frequency Separation (3 Bands)",
                    move |doc, active| {
                        let (_, mid, _) = ops::frequency_separation_3(doc, index, radius, coarse);
                        *active = mid;
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
        let mut response = ui.add_enabled(self.enabled(cmd), button);
        let missing = missing_models(cmd, &self.models);
        if !missing.is_empty() {
            let plural = if missing.len() > 1 { "s" } else { "" };
            let text = format!("Needs the {} model{plural}: see Help › AI Models", missing.join(", "));
            response = response.on_disabled_hover_text(text);
        }
        if response.clicked() {
            let ctx = ui.ctx().clone();
            self.run(cmd, &ctx);
        }
    }

    /// Right-clicking the image, as in Photoshop: what's handy for the
    /// selection, or with none, for the layer.
    fn canvas_menu(&mut self, ui: &mut Ui) {
        if self.editor.as_ref().is_some_and(|e| e.doc.selection.is_some()) {
            self.menu_item(ui, Command::Deselect, None);
            self.menu_item(ui, Command::InvertSelection, Some("Select Inverse".into()));
            self.menu_item(ui, Command::Feather, None);
            self.menu_item(ui, Command::SelectAndMask, None);
            self.menu_item(ui, Command::SaveSelection, None);
            ui.separator();
            self.menu_item(ui, Command::ContentAwareFill, None);
            self.menu_item(ui, Command::GenerativeFill, None);
            self.menu_item(ui, Command::FillForeground, None);
            self.menu_item(ui, Command::FillBackground, None);
            self.menu_item(ui, Command::Clear, None);
            ui.separator();
            self.menu_item(ui, Command::Copy, None);
            self.menu_item(ui, Command::Cut, None);
        } else {
            self.menu_item(ui, Command::SelectAll, Some("Select All".into()));
            self.menu_item(ui, Command::Paste, None);
        }
        self.menu_item(ui, Command::FreeTransform, None);
    }

    /// Image › Adjustments › Presets: add a saved preset's layers.
    fn presets_menu(&mut self, ui: &mut Ui) {
        let names = crate::presets::dir().as_deref().map(crate::presets::names).unwrap_or_default();
        let enabled = self.editor.as_ref().is_some_and(|e| e.busy().is_none());
        ui.add_enabled_ui(enabled && !names.is_empty(), |ui| {
            ui.menu_button("Presets", |ui| {
                for name in &names {
                    if ui.button(name).clicked() {
                        match crate::presets::path(name).as_deref().map(Preset::load) {
                            Some(Ok(preset)) => self.apply_preset(name, &preset),
                            Some(Err(e)) => self.message(format!("Could not read the preset “{name}”: {e}"), true),
                            None => {}
                        }
                        ui.close();
                    }
                }
            });
        });
        ui.add_enabled_ui(!names.is_empty(), |ui| {
            ui.menu_button("Delete Preset", |ui| {
                for name in &names {
                    if ui.button(name).clicked() {
                        self.dialog = Some(Dialog::DeletePreset { name: name.clone() });
                        ui.close();
                    }
                }
            });
        });
    }

    /// A name for a preset or LUT of the selected layers: the top one's.
    fn preset_name(&self) -> String {
        let Some(editor) = &self.editor else {
            return String::new();
        };
        let top = editor.selected().last().copied().unwrap_or(editor.active);
        editor.doc.layer(top).map(|l| l.name.replace('/', "-")).unwrap_or_default()
    }

    /// Bake the selected adjustment layers into a .cube LUT at `path`.
    fn export_lut(&mut self, path: &Path) {
        let Some(editor) = &self.editor else { return };
        let Some(preset) = Preset::from_layers(&editor.doc, &editor.selected()) else {
            return;
        };
        let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let cube = preset.lut(&editor.doc.profile, LUT_SIZE, &title).to_cube();
        match std::fs::write(path, cube) {
            Ok(()) => self.message(format!("Exported {}", path.display()), false),
            Err(e) => self.message(format!("Could not export the LUT: {e}"), true),
        }
    }

    fn save_preset(&mut self, name: &str) {
        let Some(editor) = &self.editor else { return };
        let (Some(preset), Some(path)) = (Preset::from_layers(&editor.doc, &editor.selected()), crate::presets::path(name))
        else {
            return;
        };
        match preset.save(&path) {
            Ok(()) => self.message(format!("Saved the preset “{name}”"), false),
            Err(e) => self.message(format!("Could not save the preset: {e}"), true),
        }
    }

    fn delete_preset(&mut self, name: &str) {
        let Some(path) = crate::presets::path(name) else { return };
        match std::fs::remove_file(path) {
            Ok(()) => self.message(format!("Deleted the preset “{name}”"), false),
            Err(e) => self.message(format!("Could not delete the preset “{name}”: {e}"), true),
        }
    }

    fn apply_preset(&mut self, name: &str, preset: &Preset) {
        let Some(editor) = &mut self.editor else { return };
        let index = editor.active_index().unwrap_or(0);
        editor.edit(&format!("Preset {name}"), |doc, active| {
            if let Some(top) = preset.apply(doc, index) {
                *active = top;
            }
        });
        // Painting on an adjustment layer paints its mask.
        let adjusts = editor.doc.layer(editor.active).is_some_and(|l| l.adjustment.is_some());
        editor.target = if adjusts { Target::Mask } else { Target::Pixels };
    }

    fn menu_bar(&mut self, ui: &mut Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                self.menu_item(ui, Command::New, None);
                self.menu_item(ui, Command::NewFromClipboard, None);
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
                            self.open(path, &ctx);
                        }
                    }
                });
                self.menu_item(ui, Command::ReopenLast, None);
                self.menu_item(ui, Command::Close, None);
                ui.separator();
                self.menu_item(ui, Command::Save, None);
                self.menu_item(ui, Command::SaveAs, None);
                ui.separator();
                self.menu_item(ui, Command::ExportTiff, None);
                self.menu_item(ui, Command::ExportJpeg, None);
                self.menu_item(ui, Command::ExportPng, None);
                self.menu_item(ui, Command::BatchExport, None);
                ui.menu_button("Automate", |ui| {
                    self.menu_item(ui, Command::Photomerge, None);
                });
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
                ui.menu_button("Paste Special", |ui| {
                    self.menu_item(ui, Command::PasteInPlace, None);
                    self.menu_item(ui, Command::PasteInto, None);
                });
                ui.separator();
                self.menu_item(ui, Command::FreeTransform, None);
                self.menu_item(ui, Command::AutoAlignLayers, None);
                ui.separator();
                self.menu_item(ui, Command::FillForeground, None);
                self.menu_item(ui, Command::FillBackground, None);
                self.menu_item(ui, Command::Clear, None);
                self.menu_item(ui, Command::ContentAwareFill, None);
                self.menu_item(ui, Command::GenerativeFill, None);
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
                    self.presets_menu(ui);
                    self.menu_item(ui, Command::SaveAdjustmentPreset, None);
                    self.menu_item(ui, Command::ExportAdjustmentLut, None);
                    ui.separator();
                    self.menu_item(ui, Command::Invert, None);
                });
                ui.separator();
                self.menu_item(ui, Command::ImageSize, None);
                self.menu_item(ui, Command::CanvasSize, None);
                ui.menu_button("Image Rotation", |ui| {
                    self.menu_item(ui, Command::Rotate180, None);
                    self.menu_item(ui, Command::Rotate90Cw, None);
                    self.menu_item(ui, Command::Rotate90Ccw, None);
                    ui.separator();
                    self.menu_item(ui, Command::FlipCanvasHorizontal, None);
                    self.menu_item(ui, Command::FlipCanvasVertical, None);
                });
                self.menu_item(ui, Command::Crop, None);
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
                let locks = self.editor.as_ref().and_then(|e| e.doc.layer(e.active)).map(|l| l.locks);
                for cmd in LOCKS {
                    let mut locks = locks.unwrap_or_default();
                    let unlock = (*lock_flag(&mut locks, cmd)).then(|| cmd.label().replacen("Lock", "Unlock", 1));
                    self.menu_item(ui, cmd, unlock);
                }
                ui.separator();
                self.menu_item(ui, Command::BlendingOptions, None);
                ui.separator();
                self.menu_item(ui, Command::AddMask, None);
                let has_selection = self.editor.as_ref().is_some_and(|e| e.doc.selection.is_some());
                let hide = has_selection.then(|| "Add Layer Mask (Hide Selection)".to_owned());
                self.menu_item(ui, Command::AddMaskHideAll, hide);
                self.menu_item(ui, Command::ToggleMask, None);
                self.menu_item(ui, Command::DeleteMask, None);
                self.menu_item(ui, Command::MaskDensity, None);
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
                self.menu_item(ui, Command::SelectSubject, None);
                self.menu_item(ui, Command::SelectSkin, None);
                self.menu_item(ui, Command::SelectHair, None);
                self.menu_item(ui, Command::SelectEyes, None);
                self.menu_item(ui, Command::SelectLips, None);
                self.menu_item(ui, Command::SelectTeeth, None);
                ui.separator();
                self.menu_item(ui, Command::SelectAndMask, None);
                ui.menu_button("Modify", |ui| {
                    self.menu_item(ui, Command::BorderSelection, None);
                    self.menu_item(ui, Command::SmoothSelection, None);
                    self.menu_item(ui, Command::ExpandSelection, None);
                    self.menu_item(ui, Command::ContractSelection, None);
                    self.menu_item(ui, Command::Feather, None);
                });
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
                self.menu_item(ui, Command::SaveSelection, None);
                ui.separator();
                let in_qm = self
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.view() == View::QuickMask);
                let tick = if in_qm { "✓" } else { "  " };
                self.menu_item(
                    ui,
                    Command::QuickMask,
                    Some(format!("{tick} {}", Command::QuickMask.label())),
                );
            });
            ui.menu_button("Filter", |ui| {
                self.menu_item(ui, Command::Liquify, None);
                ui.separator();
                ui.menu_button("Noise", |ui| {
                    self.menu_item(ui, Command::AddNoise, None);
                    self.menu_item(ui, Command::ReduceNoise, None);
                    self.menu_item(ui, Command::Denoise, None);
                });
                ui.menu_button("Blur", |ui| {
                    self.menu_item(ui, Command::GaussianBlur, None);
                    self.menu_item(ui, Command::SmartBlur, None);
                });
                ui.menu_button("Sharpen", |ui| {
                    self.menu_item(ui, Command::UnsharpMask, None);
                    self.menu_item(ui, Command::SmartSharpen, None);
                });
                self.menu_item(ui, Command::HighPass, None);
            });
            ui.menu_button("Retouch", |ui| {
                self.menu_item(ui, Command::AutoRetouch, None);
                ui.separator();
                self.menu_item(ui, Command::HealBlemishes, None);
                self.menu_item(ui, Command::SmoothSkin, None);
                self.menu_item(ui, Command::EvenTone, None);
                self.menu_item(ui, Command::ReduceShine, None);
                self.menu_item(ui, Command::LightenUnderEyes, None);
                self.menu_item(ui, Command::WhitenTeeth, None);
                self.menu_item(ui, Command::WhitenEyes, None);
                self.menu_item(ui, Command::FrequencySeparation, None);
                self.menu_item(ui, Command::FrequencySeparation3, None);
                self.menu_item(ui, Command::DodgeAndBurn, None);
                self.menu_item(ui, Command::DodgeAndBurnCurves, None);
                ui.separator();
                self.menu_item(ui, Command::HighPassSharpening, None);
                self.menu_item(ui, Command::Finish, None);
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
                ui.separator();
                let tick = |on: bool, label: &str| Some(format!("{} {label}", if on { "✓" } else { "  " }));
                let custom = self.filters.proof_profile.is_some();
                ui.menu_button("Proof Setup", |ui| {
                    self.menu_item(ui, Command::ProofSetupWeb, tick(!custom, Command::ProofSetupWeb.label()));
                    let label = if custom {
                        tick(true, &format!("{}…", self.proof.name()))
                    } else {
                        tick(false, Command::ProofSetupCustom.label())
                    };
                    self.menu_item(ui, Command::ProofSetupCustom, label);
                });
                self.menu_item(ui, Command::ProofColors, tick(self.proof.colors, Command::ProofColors.label()));
                let warning = self.proof.gamut_warning;
                self.menu_item(ui, Command::GamutWarning, tick(warning, Command::GamutWarning.label()));
            });
            ui.menu_button("Window", |ui| {
                for (tab, command) in TopTab::ALL {
                    let tick = if self.top_tab == Some(tab) { "✓" } else { "  " };
                    self.menu_item(ui, command, Some(format!("{tick} {}", command.label())));
                }
                ui.separator();
                for (tab, command) in RightTab::ALL {
                    let tick = if self.right_tab == tab { "✓" } else { "  " };
                    self.menu_item(ui, command, Some(format!("{tick} {}", command.label())));
                }
                ui.separator();
                self.menu_item(ui, Command::NextImage, None);
                self.menu_item(ui, Command::PreviousImage, None);
                // The open images, as at the bottom of Photoshop's Window menu.
                let names: Vec<String> = self.tabs().map(tab_name).collect();
                for (index, name) in names.into_iter().enumerate() {
                    let tick = if index == self.tab { "✓" } else { "  " };
                    if ui.button(format!("{tick} {name}")).clicked() {
                        self.show_tab(index);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Help", |ui| {
                self.menu_item(ui, Command::AiModels, None);
            });
        });
    }

    /// The open images' tabs, above the canvas when there's more than one.
    /// Clicking a tab shows its image; its ✕, or a middle click, closes it.
    fn tab_bar(&mut self, ui: &mut Ui) {
        let tabs: Vec<(String, String)> = self
            .tabs()
            .map(|e| (tab_name(e), e.doc.saved_path.as_ref().unwrap_or(&e.doc.path).to_string_lossy().into_owned()))
            .collect();
        let (mut show, mut close) = (None, None);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for (index, (name, path)) in tabs.into_iter().enumerate() {
                    let text = if index == self.tab {
                        RichText::new(name).color(self.theme.foreground).strong()
                    } else {
                        RichText::new(name).color(self.theme.dark_foreground)
                    };
                    let tab = ui.add(Button::new(text).frame(false));
                    let tab = if path.is_empty() { tab } else { tab.on_hover_text(path) };
                    if tab.clicked() {
                        show = Some(index);
                    }
                    if tab.middle_clicked() || ui.small_button("✕").on_hover_text("Close").clicked() {
                        close = Some(index);
                    }
                    ui.add_space(8.0);
                }
            });
        });
        let ctx = ui.ctx().clone();
        if let Some(index) = close {
            self.close_tab(index, &ctx);
        } else if let Some(index) = show {
            self.show_tab(index);
        }
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
                    View::QuickMask if editor.previewing_selection() => {
                        Some("Select and Mask — red shows what isn't selected")
                    }
                    View::QuickMask => Some("Quick Mask — press Q to exit"),
                    View::Channel(_) | View::Alpha(_) => {
                        Some("Viewing one channel — click RGB in Channels, or press Ctrl+2 or Esc to return")
                    }
                    _ => None,
                };
                if let Some(text) = showing {
                    ui.label(RichText::new(text).color(self.theme.accent));
                    ui.separator();
                }
                if let Some(proof) = self.proof.proof() {
                    let what = match (proof.colors, proof.gamut_warning) {
                        (true, true) => "Proof and gamut warning",
                        (true, false) => "Proof",
                        _ => "Gamut warning",
                    };
                    let text = format!("{what}: {}", self.proof.name());
                    ui.label(RichText::new(text).color(self.theme.accent));
                    ui.separator();
                }
                if self.objects.busy() {
                    ui.label(RichText::new("Finding the object…").color(self.theme.accent));
                    ui.separator();
                }
                if self.content_fill.busy() {
                    ui.label(RichText::new("Filling the selection…").color(self.theme.accent));
                    ui.separator();
                }
                if let Some((done, total)) = self.denoising.busy() {
                    ui.label(RichText::new(format!("Denoising… {done} of {total}")).color(self.theme.accent));
                    ui.separator();
                }
                if let Some((done, total)) = self.upscaling.busy() {
                    ui.label(RichText::new(format!("Enlarging with AI… {done} of {total}")).color(self.theme.accent));
                    ui.separator();
                }
                if self.select_subject.busy() {
                    ui.label(RichText::new("Finding the subject…").color(self.theme.accent));
                    ui.separator();
                }
                if let Some(part) = self.face_selection.busy() {
                    ui.label(RichText::new(part.finding()).color(self.theme.accent));
                    ui.separator();
                }
                if let Some(part) = self.whitening.busy() {
                    ui.label(RichText::new(part.finding()).color(self.theme.accent));
                    ui.separator();
                }
                if let Some((_, t)) = editor.transform() {
                    ui.label(RichText::new(crate::free_transform::readout(&t)).color(self.theme.accent));
                    ui.separator();
                } else if editor.liquifying() {
                    let text = "Liquify — Enter to apply, Esc to cancel";
                    ui.label(RichText::new(text).color(self.theme.accent));
                    ui.separator();
                } else if self.tools.tool == crate::tools::Tool::Crop {
                    let [x0, y0, x1, y1] = self.crop_box();
                    let (w, h) = ((x1 - x0).round(), (y1 - y0).round());
                    let text = format!("Crop: {w} × {h} px — Enter to crop, Esc to exit");
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
                let shown = self.monitor.as_ref().and_then(|m| m.profile()).map_or("sRGB", |p| p.description());
                ui.label(RichText::new(doc.profile.description()).color(dim))
                    .on_hover_text(format!("Shown on this monitor as {shown}"));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let busy = self
                    .opening
                    .front()
                    .map(|(p, _)| {
                        let name = p.file_name().unwrap_or_default().to_string_lossy();
                        format!("Opening {name}…")
                    })
                    .or_else(|| self.batch_export.as_ref().map(|b| format!("Exporting {} of {}…", b.done + 1, b.total)))
                    .or_else(|| self.file_job.as_ref().map(|j| format!("{}…", j.label)))
                    .or_else(|| self.merging.as_ref().map(|(cmd, _)| cmd.label().to_owned()))
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
        if let (Some(Dialog::SelectAndMask(dialog)), Some(editor)) = (&mut self.dialog, &mut self.editor) {
            match dialog.show(ctx, editor, &self.defaults.select_and_mask, &self.theme) {
                Some(true) => {
                    dialog.apply(editor);
                    self.filters.select_and_mask = dialog.options;
                    self.filters.select_and_mask_output = dialog.output;
                    self.filters.save();
                }
                Some(false) => dialog.cancel(editor),
                None => return,
            }
            self.dialog = None;
            return;
        }
        if let (Some(Dialog::Denoise(dialog)), Some(editor)) = (&mut self.dialog, &self.editor) {
            match dialog.show(ctx, editor, &self.theme) {
                Ok(Some(true)) => {
                    dialog.apply(ctx, &mut self.denoising);
                    self.filters.denoise_luminance = dialog.luminance;
                    self.filters.denoise_color = dialog.color;
                    self.filters.save();
                }
                Ok(Some(false)) => {}
                Ok(None) => return,
                Err(e) => self.message(e, true),
            }
            self.dialog = None;
            return;
        }
        if let (Some(Dialog::GenerativeFill(dialog)), Some(editor)) = (&mut self.dialog, &mut self.editor) {
            if dialog.show(ctx, editor, &self.theme).is_some() {
                self.dialog = None;
            }
            return;
        }
        if let (Some(Dialog::AutoRetouch(dialog)), Some(editor)) = (&mut self.dialog, &mut self.editor) {
            match dialog.show(ctx, editor, &self.theme) {
                Ok(Some(true)) => {
                    dialog.apply(ctx, editor);
                    self.filters.auto_retouch = dialog.all;
                    self.filters.save();
                }
                Ok(Some(false)) => {}
                Ok(None) => return,
                Err(e) => self.message(e, true),
            }
            self.dialog = None;
            return;
        }
        if let (Some(Dialog::HealBlemishes(dialog)), Some(editor)) = (&mut self.dialog, &mut self.editor) {
            match dialog.show(ctx, &self.theme) {
                Ok(Some(true)) => {
                    dialog.apply(ctx, editor);
                    self.filters.blemish_sensitivity = dialog.sensitivity;
                    self.filters.save();
                }
                Ok(Some(false)) => {}
                Ok(None) => return,
                Err(e) => self.message(e, true),
            }
            self.dialog = None;
            return;
        }
        if let (Some(Dialog::SmoothSkin(dialog)), Some(editor)) = (&mut self.dialog, &mut self.editor) {
            match dialog.show(ctx, editor, &self.theme) {
                Ok(Some(true)) => {
                    dialog.apply(ctx, editor);
                    self.filters.smooth_skin = dialog.smoothing;
                    self.filters.save();
                }
                Ok(Some(false)) => {}
                Ok(None) => return,
                Err(e) => self.message(e, true),
            }
            self.dialog = None;
            return;
        }
        if let (Some(Dialog::EvenTone(dialog)), Some(editor)) = (&mut self.dialog, &mut self.editor) {
            match dialog.show(ctx, editor, &self.theme) {
                Ok(Some(true)) => {
                    dialog.apply(ctx, editor);
                    self.filters.even_tone = dialog.evening;
                    self.filters.save();
                }
                Ok(Some(false)) => {}
                Ok(None) => return,
                Err(e) => self.message(e, true),
            }
            self.dialog = None;
            return;
        }
        let defaults = &self.defaults;
        let auto_separation = self.editor.as_ref().map(separation_radius);
        let editor = self.editor.as_ref();
        let preview_cache = &mut self.filter_preview;
        let Some(dialog) = &mut self.dialog else {
            self.filter_preview = None;
            return;
        };
        if !matches!(dialog, Dialog::Filter { filter, .. } if has_preview_box(filter)) {
            *preview_cache = None;
        }
        let mut close = false;
        let mut action: Option<DialogAction> = None;
        let hint = self.theme.dark_foreground;
        let warning = self.theme.red;
        let name = self
            .editor
            .as_ref()
            .map(|e| e.doc.file_name())
            .unwrap_or_default();
        let (now_w, now_h) = self.editor.as_ref().map_or((1, 1), |e| (e.doc.width, e.doc.height));
        let upscaler_missing = !missing(&[omapix_ai::upscale::MODEL], &self.models).is_empty();
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
                    coarse,
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
                    if matches!(command, Command::FrequencySeparation | Command::FrequencySeparation3) {
                        let text = if coarse.is_some() {
                            "Fine: just above the pores, so only they are in texture.\n\
                             Coarse: until blotches vanish from color/tone."
                        } else {
                            "Raise the radius until skin blotches vanish from the\n\
                             color/tone layer and only pores remain in texture."
                        };
                        ui.label(RichText::new(text).color(hint));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label("Preview");
                            ui.selectable_value(preview, Some(SeparationBand::Texture), "Texture");
                            if *command == Command::FrequencySeparation3 {
                                ui.selectable_value(preview, Some(SeparationBand::Mid), "Mid");
                            }
                            ui.selectable_value(preview, Some(SeparationBand::Tone), "Color/Tone");
                            ui.selectable_value(preview, None, "Image");
                        });
                        ui.add_space(4.0);
                    }
                    if let Some(coarse) = coarse {
                        radius_field(ui, radius, "Fine", 0.1..=(*coarse).max(0.1));
                        radius_field(ui, coarse, "Coarse", (*radius).min(250.0)..=250.0);
                        if *coarse < *radius {
                            *coarse = *radius;
                        }
                    } else {
                        let (label, range) = match command {
                            Command::BorderSelection => ("Width", 1.0..=200.0),
                            Command::SmoothSelection
                            | Command::ExpandSelection
                            | Command::ContractSelection => ("Radius", 1.0..=100.0),
                            _ => ("Radius", 0.1..=250.0),
                        };
                        radius_field(ui, radius, label, range);
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ok = ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("Defaults").clicked() {
                            *radius = match command {
                                Command::Feather => defaults.feather_radius,
                                Command::BorderSelection => defaults.border_width,
                                Command::SmoothSelection => defaults.smooth_radius,
                                Command::ExpandSelection => defaults.expand_radius,
                                Command::ContractSelection => defaults.contract_radius,
                                Command::FrequencySeparation => {
                                    defaults.separation_radius.or(auto_separation).unwrap_or(*radius)
                                }
                                Command::FrequencySeparation3 => {
                                    let auto = auto_separation.map(three_band_radii);
                                    if let Some(c) = coarse {
                                        *c = defaults.separation3_coarse.or(auto.map(|a| a.1)).unwrap_or(*c);
                                    }
                                    defaults.separation3_fine.or(auto.map(|a| a.0)).unwrap_or(*radius)
                                }
                                _ => defaults.high_pass_radius,
                            };
                        }
                        if ok {
                            let (command, radius, coarse) = (*command, *radius, *coarse);
                            action = Some(Box::new(move |app, ctx| {
                                app.apply_radius(command, radius, coarse, ctx)
                            }));
                            close = true;
                        }
                    });
                }
                Dialog::Filter { filter, preview } => {
                    ui.heading(filter.name());
                    ui.add_space(8.0);
                    if has_preview_box(filter) {
                        filter_preview_box(ui, filter, editor, preview_cache);
                    }
                    match filter {
                        LayerFilter::GaussianBlur { radius } | LayerFilter::HighPass { radius } => {
                            radius_field(ui, radius, "Radius", 0.1..=250.0);
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
                            radius_field(ui, radius, "Radius", 0.1..=250.0);
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
                        LayerFilter::SmartSharpen(options) => {
                            smart_sharpen_controls(ui, options, hint);
                        }
                        LayerFilter::ReduceNoise(options) => {
                            reduce_noise_controls(ui, options, hint);
                        }
                        LayerFilter::MaskDensity { density } => {
                            ui.horizontal(|ui| {
                                ui.label("Density");
                                ui.add(
                                    egui::Slider::new(density, 0.0..=100.0)
                                        .suffix(" %")
                                        .fixed_decimals(0),
                                );
                            });
                        }
                        LayerFilter::SmartBlur(options) => {
                            smart_blur_controls(ui, options, hint);
                        }
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
                        if ui.button("Defaults").clicked() {
                            *filter = defaults.filter(filter);
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
                        if ui.button("Defaults").clicked() {
                            *options = defaults.noise;
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
                Dialog::BlendingOptions(_) | Dialog::SelectAndMask(_) | Dialog::AutoRetouch(_) | Dialog::HealBlemishes(_) | Dialog::SmoothSkin(_) | Dialog::EvenTone(_) | Dialog::Denoise(_) | Dialog::GenerativeFill(_) => {}
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
                                // Quitting asks about the next image.
                                app.guard(then, ctx);
                            }));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::SavePreset { name } => {
                    ui.heading("Save Adjustment Preset");
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Keeps the selected adjustment layers and groups, without\n\
                             their masks, to add to other images from Image › Adjustments.",
                        )
                        .color(hint),
                    );
                    ui.add_space(8.0);
                    let field = ui.text_edit_singleline(name);
                    if ui.memory(|m| m.focused().is_none()) {
                        field.request_focus();
                    }
                    let name = name.replace('/', "-").trim().to_owned();
                    let exists = !name.is_empty() && crate::presets::path(&name).is_some_and(|p| p.exists());
                    if exists {
                        ui.add_space(4.0);
                        let text = format!("There's already a preset called “{name}”: saving replaces it.");
                        ui.label(RichText::new(text).color(warning));
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let label = if exists { "Replace" } else { "OK" };
                        let ok = ui.add_enabled(!name.is_empty(), Button::new(label)).clicked()
                            || (!name.is_empty() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ok {
                            action = Some(Box::new(move |app, _| app.save_preset(&name)));
                            close = true;
                        }
                    });
                }
                Dialog::NewImage { width, height } => {
                    ui.heading("New");
                    ui.add_space(8.0);
                    for (label, value) in [("Width", &mut *width), ("Height", &mut *height)] {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            ui.add(egui::DragValue::new(value).range(1..=MAX_SIDE).suffix(" px"));
                        });
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ok = ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ok {
                            let doc = Document::blank(*width, *height);
                            action = Some(Box::new(move |app, _| app.new_image(doc)));
                            close = true;
                        }
                    });
                }
                Dialog::ImageSize { width, height, constrain, ai } => {
                    ui.heading("Image Size");
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("Now {now_w} × {now_h} px")).color(hint));
                    ui.add_space(4.0);
                    let before = (*width, *height);
                    size_field(ui, "Width", width, now_w);
                    size_field(ui, "Height", height, now_h);
                    let scaled = |v: u32, to: u32, from: u32| {
                        ((f64::from(v) * f64::from(to) / f64::from(from)).round() as u32).clamp(1, MAX_SIDE)
                    };
                    if *constrain && *width != before.0 {
                        *height = scaled(*width, now_h, now_w);
                    } else if *constrain && *height != before.1 {
                        *width = scaled(*height, now_w, now_h);
                    }
                    ui.checkbox(constrain, "Constrain Proportions");
                    // The model only enlarges.
                    let larger = *width > now_w || *height > now_h;
                    let why = if upscaler_missing { "Needs the RealPLKSR model: see Help › AI Models" } else { "For a larger size" };
                    ui.add_enabled(larger && !upscaler_missing, egui::Checkbox::new(ai, "Enlarge with AI")).on_disabled_hover_text(why);
                    let with_ai = *ai && larger && !upscaler_missing;
                    if with_ai {
                        ui.label(
                            RichText::new(
                                "RealPLKSR sharpens what the image shows, onto a new\n\
                                 Upscale layer over the layers enlarged as usual.\n\
                                 A large photo takes a few minutes.",
                            )
                            .color(hint),
                        );
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ok = ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ok {
                            let (w, h) = (*width, *height);
                            action = Some(Box::new(move |app, ctx| {
                                if with_ai {
                                    app.upscale_image(w, h, ctx);
                                } else {
                                    app.resize_image(w, h);
                                }
                            }));
                            close = true;
                        }
                    });
                }
                Dialog::CanvasSize { width, height, anchor, extension } => {
                    ui.heading("Canvas Size");
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("Now {now_w} × {now_h} px")).color(hint));
                    ui.add_space(4.0);
                    size_field(ui, "Width", width, now_w);
                    size_field(ui, "Height", height, now_h);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Anchor");
                        egui::Grid::new("anchor").spacing([2.0, 2.0]).show(ui, |ui| {
                            for row in 0..3 {
                                for col in 0..3 {
                                    let on = *anchor == (col, row);
                                    let button = Button::new("").min_size(egui::vec2(20.0, 20.0)).selected(on);
                                    if ui.add(button).clicked() {
                                        *anchor = (col, row);
                                    }
                                }
                                ui.end_row();
                            }
                        });
                    });
                    ui.add_space(4.0);
                    egui::ComboBox::from_label("Canvas extension color")
                        .selected_text(format!("{extension:?}"))
                        .show_ui(ui, |ui| {
                            for e in [
                                Extension::Background,
                                Extension::Foreground,
                                Extension::White,
                                Extension::Black,
                                Extension::Transparent,
                            ] {
                                ui.selectable_value(extension, e, format!("{e:?}"));
                            }
                        });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ok = ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ok {
                            let (w, h, anchor, extension) = (*width, *height, *anchor, *extension);
                            action = Some(Box::new(move |app, _| app.resize_canvas(w, h, anchor, extension)));
                            close = true;
                        }
                    });
                }
                Dialog::AiModels(found) => {
                    ui.heading("AI Models");
                    ui.add_space(8.0);
                    for (model, files) in omapix_ai::models::models().iter().zip(found.iter()) {
                        ui.label(RichText::new(model.name).strong());
                        ui.label(format!("For {}", model.used_by));
                        ui.label(RichText::new(model.licence).color(hint));
                        match files.as_ref().and_then(|f| f.values().next()) {
                            Some(path) => {
                                let runs = match omapix_ai::on_gpu(path) {
                                    Some(true) => " (GPU)",
                                    Some(false) => " (CPU)",
                                    None => "",
                                };
                                let folder = path.parent().unwrap_or(path);
                                ui.label(RichText::new(format!("{}{runs}", folder.display())).color(hint));
                            }
                            None if model.files.iter().all(|f| f.url.is_none()) => {
                                ui.label(RichText::new("Not installed: install it from darktable's AI preferences").color(warning));
                            }
                            None => {
                                let size: u64 = model.files.iter().map(|f| f.bytes).sum();
                                let id = if model.optional { format!(" {}", model.id) } else { String::new() };
                                let text = format!("Not installed: run scripts/fetch-models.sh{id} ({} MB)", size / 1_000_000);
                                ui.label(RichText::new(text).color(warning));
                            }
                        }
                        ui.add_space(8.0);
                    }
                    if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                }
                Dialog::DeletePreset { name } => {
                    ui.heading("Delete Preset");
                    ui.add_space(8.0);
                    ui.label(format!("Delete the preset “{name}”? This can't be undone."));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            let name = name.clone();
                            action = Some(Box::new(move |app, _| app.delete_preset(&name)));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::BatchExport { files, settings, picking } => {
                    if let Some((folder, rx)) = picking
                        && let Ok(picked) = rx.try_recv()
                    {
                        match picked {
                            Some(mut picked) if *folder => settings.folder = picked.pop(),
                            Some(picked) => {
                                // At first, an "export" folder beside them.
                                if settings.folder.is_none() {
                                    settings.folder = picked.first().and_then(|f| f.parent()).map(|d| d.join("export"));
                                }
                                *files = picked;
                            }
                            None => {}
                        }
                        *picking = None;
                    }
                    ui.heading("Batch Export");
                    ui.add_space(8.0);
                    let mut pick = |ui: &mut Ui, folder: bool, label: &str| {
                        if ui.add_enabled(picking.is_none(), Button::new(label)).clicked() {
                            let (tx, rx) = channel();
                            let ctx = ui.ctx().clone();
                            let start = settings.folder.clone();
                            std::thread::spawn(move || {
                                let mut dialog = rfd::FileDialog::new();
                                if let Some(dir) = start {
                                    dialog = dialog.set_directory(dir);
                                }
                                let picked = if folder {
                                    dialog.pick_folder().map(|f| vec![f])
                                } else {
                                    dialog.add_filter("Images", &OPEN_EXTENSIONS).pick_files()
                                };
                                let _ = tx.send(picked);
                                ctx.request_repaint();
                            });
                            *picking = Some((folder, rx));
                        }
                    };
                    ui.horizontal(|ui| {
                        pick(ui, false, "Choose Files…");
                        let chosen = match files.as_slice() {
                            [] => "No files chosen".to_owned(),
                            [one] => one.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                            many => format!("{} files", many.len()),
                        };
                        ui.label(RichText::new(chosen).color(hint));
                    });
                    ui.horizontal(|ui| {
                        pick(ui, true, "Choose Folder…");
                        let folder = settings.folder.as_ref().map_or("No folder chosen".into(), |f| f.display().to_string());
                        ui.label(RichText::new(folder).color(hint));
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut settings.resize, "Resize the long edge to");
                        ui.add_enabled(settings.resize, egui::DragValue::new(&mut settings.long_edge).range(1..=MAX_SIDE).suffix(" px"));
                    });
                    ui.checkbox(&mut settings.sharpen, "Sharpen (Unsharp Mask's settings)");
                    ui.checkbox(&mut settings.grain, "Add grain (Add Noise's settings)");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut settings.jpeg, true, "JPEG");
                        ui.add_enabled(settings.jpeg, egui::Slider::new(&mut settings.quality, 1..=100).text("quality"));
                        ui.radio_value(&mut settings.jpeg, false, "16-bit TIFF");
                    });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ready = !files.is_empty() && settings.folder.is_some();
                        let ok = ui.add_enabled(ready, Button::new("Export")).clicked()
                            || (ready && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ok {
                            let (files, settings) = (std::mem::take(files), settings.clone());
                            action = Some(Box::new(move |app, ctx| app.batch_export(files, settings, ctx)));
                            close = true;
                        }
                    });
                }
            }
        });
        if close || response.should_close() {
            self.dialog = None;
            self.filter_preview = None;
        }
        // Show the separation or blur preview while its dialog is open.
        if let Some(editor) = &mut self.editor {
            match &self.dialog {
                Some(Dialog::Radius {
                    command: Command::FrequencySeparation | Command::FrequencySeparation3,
                    radius,
                    coarse,
                    preview,
                }) => {
                    let view = match preview {
                        Some(band) => View::Separation {
                            fine: *radius,
                            coarse: *coarse,
                            band: *band,
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
                        self.open(path, &ctx);
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

    /// Esc disarms an eyedropper, or leaves mask view, the mask overlay or
    /// a channel view (the editor leaves them itself when what they show
    /// goes away).
    fn check_escape(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let mask_view = matches!(
            editor.view(),
            View::Mask(_) | View::MaskOverlay(_) | View::Channel(_) | View::Alpha(_)
        );
        let escape = (mask_view || self.properties.eyedropper.is_some())
            && self.dialog.is_none()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if escape && self.properties.eyedropper.take().is_none() {
            editor.set_view(View::Image);
        }
    }

    /// Esc deselects, once nothing else has taken it (Quick Mask is the
    /// selection being painted, so it's left alone).
    fn check_deselect(&mut self, ctx: &egui::Context) {
        let Some(editor) = &self.editor else {
            return;
        };
        if editor.doc.selection.is_none()
            || editor.view() == View::QuickMask
            || self.dialog.is_some()
            || ctx.egui_wants_keyboard_input()
            || egui::Popup::is_any_open(ctx)
        {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.run(Command::Deselect, ctx);
        }
    }

    /// Enter applies Free Transform and Esc cancels it.
    fn check_transform_keys(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if editor.transform().is_none() || self.dialog.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let key = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        if key(egui::Key::Enter) {
            editor.commit_transform();
        } else if key(egui::Key::Escape) {
            editor.cancel_transform();
        } else {
            return;
        }
        self.transform_drag = None;
    }

    /// Dragging Free Transform's box: its handles scale, inside moves, and
    /// outside rotates.
    fn transform_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let Some((bounds, t)) = editor.transform() else {
            return;
        };
        match input {
            ToolInput::StrokeBegin(p) => {
                let reach = editor.canvas.image_per_point() * HANDLE_REACH;
                let handle = crate::free_transform::hit(bounds, &t, p, reach);
                self.transform_drag = Some(crate::free_transform::Drag::new(handle, p, t));
            }
            ToolInput::StrokeMove(p) => {
                if let Some(drag) = &self.transform_drag {
                    editor.transform_to(drag.to(bounds, p, modifiers.shift, modifiers.alt));
                }
            }
            ToolInput::StrokeEnd => self.transform_drag = None,
            ToolInput::Sample(_) | ToolInput::BrushDrag { .. } => {}
        }
    }

    /// The Crop tool's box, until it's changed round the selection's
    /// bounds, as in Photoshop, or else the whole image.
    fn crop_box(&self) -> [f64; 4] {
        let start = self.editor.as_ref().map_or([0.0; 4], |e| match e.doc.selection.as_ref().and_then(|s| s.bounds()) {
            Some([x, y, w, h]) => [x, y, x + w, y + h].map(f64::from),
            None => [0.0, 0.0, e.doc.width.into(), e.doc.height.into()],
        });
        self.crop.unwrap_or(start)
    }

    /// Enter crops to the Crop tool's box, and Esc drops it and goes back
    /// to the tool before.
    fn check_crop_keys(&mut self, ctx: &egui::Context) {
        if self.tools.tool != crate::tools::Tool::Crop || self.dialog.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let key = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        if key(egui::Key::Enter) {
            let [x0, y0, x1, y1] = self.crop_box().map(|v| v.round() as i32);
            if let Some(editor) = &mut self.editor {
                crop(editor, x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
            }
        } else if key(egui::Key::Escape) {
            self.tools.leave_crop();
        } else {
            return;
        }
        (self.crop, self.crop_drag) = (None, None);
    }

    /// Dragging the Crop tool's box: its handles resize it (Shift keeps
    /// the proportions), inside moves it, and outside draws a new one.
    fn crop_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        use crate::free_transform::{Drag, Handle, corners, hit};
        use omapix_engine::transform::Affine;
        let Some(editor) = &self.editor else {
            return;
        };
        let rect = self.crop_box();
        let bounding = |a: Pos2, b: Pos2| [a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y)].map(f64::from);
        match input {
            ToolInput::StrokeBegin(p) => {
                let reach = editor.canvas.image_per_point() * HANDLE_REACH;
                self.crop_drag = Some(match hit(rect, &Affine::IDENTITY, p, reach) {
                    Handle::Rotate => CropDrag::New(p),
                    handle => CropDrag::Box(Drag::new(handle, p, Affine::IDENTITY), rect),
                });
            }
            ToolInput::StrokeMove(p) => match self.crop_drag {
                Some(CropDrag::Box(drag, start)) => {
                    // Free Transform keeps the proportions unless Shift is
                    // held; cropping, only while it is.
                    let shift = modifiers.shift != matches!(drag.handle(), Handle::Scale { .. });
                    let c = corners(start, &drag.to(start, p, shift, modifiers.alt));
                    self.crop = Some(bounding(c[0], c[2]));
                }
                Some(CropDrag::New(from)) => {
                    let to = if modifiers.shift { constrain_square(from, p) } else { p };
                    self.crop = Some(bounding(from, to));
                }
                None => {}
            },
            ToolInput::StrokeEnd => {
                self.crop_drag = None;
                // A click outside leaves no box, so it goes back round the image.
                if let Some([x0, y0, x1, y1]) = self.crop
                    && (x1 - x0 < 1.0 || y1 - y0 < 1.0)
                {
                    self.crop = None;
                }
            }
            ToolInput::Sample(_) | ToolInput::BrushDrag { .. } => {}
        }
    }

    /// While Liquify is open: Enter applies it, Esc cancels it, and its
    /// own keys pick a brush and its size.
    fn check_liquify_keys(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if !editor.liquifying() || self.dialog.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let key = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        if key(egui::Key::Enter) {
            editor.commit_liquify();
        } else if key(egui::Key::Escape) {
            editor.cancel_liquify();
        } else {
            self.tools.liquify.keys(ctx);
            return;
        }
        self.liquify_at = None;
    }

    /// Face-Aware Liquify's panel and Body Reshape's, while Liquify is open
    /// and each is switched on (and its models are there).
    fn face_liquify(&mut self, ctx: &egui::Context) {
        let liquify = &mut self.tools.liquify;
        match &mut self.editor {
            Some(editor) if editor.liquifying() => {
                if liquify.face_aware && missing(FACE_MODELS, &self.models).is_empty() {
                    self.face_liquify.show(ctx, editor, &self.theme, &mut liquify.face_aware);
                }
                if liquify.body && missing(POSE_MODELS, &self.models).is_empty() {
                    self.body_liquify.show(ctx, editor, &self.theme, &mut liquify.body);
                }
            }
            _ => (self.face_liquify, self.body_liquify) = Default::default(),
        }
    }

    /// Liquify's brush on the canvas. Forward Warp and Push Left follow the
    /// pointer; the others work while the button is held ([`Self::liquify_held`]).
    fn liquify_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        let pressure = self.liquify_pressure();
        let Some(editor) = &mut self.editor else {
            return;
        };
        let options = self.tools.liquify;
        match input {
            ToolInput::StrokeBegin(p) => self.liquify_at = Some(p),
            ToolInput::StrokeMove(p) => {
                let Some(from) = self.liquify_at.replace(p) else {
                    return;
                };
                if options.brush.continuous() {
                    return;
                }
                // In steps of a quarter of the brush, so fast drags stay smooth.
                let radius = options.size / 2.0;
                let d = p - from;
                let steps = (d.length() / (radius / 4.0).max(1.0)).ceil().max(1.0);
                // Alt pushes right.
                let step = d / steps * if modifiers.alt { -1.0 } else { 1.0 };
                let dabs: Vec<_> = (1..=steps as u32)
                    .map(|n| from + d * (n as f32 / steps))
                    .map(|at| ([at.x, at.y], [step.x, step.y]))
                    .collect();
                editor.liquify(options.brush, &dabs, radius, pressure);
            }
            ToolInput::StrokeEnd => {
                self.liquify_at = None;
                editor.end_liquify_stroke();
            }
            ToolInput::BrushDrag { size, .. } => self.tools.liquify.resize(size),
            ToolInput::Sample(_) => {}
        }
    }

    /// Liquify's pressure, scaled by a pen's.
    fn liquify_pressure(&self) -> f32 {
        let options = self.tools.liquify;
        let pen = self.tablet.as_ref().filter(|_| options.pen_pressure).map_or(1.0, Tablet::pressure);
        options.pressure * pen
    }

    /// Reconstruct, Pucker and Bloat work a little every frame while the
    /// button is held, as in Photoshop. Alt swaps Pucker and Bloat.
    fn liquify_held(&mut self, ctx: &egui::Context) {
        use omapix_engine::warp::Brush;
        let pressure = self.liquify_pressure();
        let (Some(editor), Some(at)) = (&mut self.editor, self.liquify_at) else {
            return;
        };
        let options = self.tools.liquify;
        if !editor.liquifying() || !options.brush.continuous() {
            return;
        }
        let brush = match (options.brush, ctx.input(|i| i.modifiers.alt)) {
            (Brush::Pucker, true) => Brush::Bloat,
            (Brush::Bloat, true) => Brush::Pucker,
            (brush, _) => brush,
        };
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        editor.liquify(brush, &[([at.x, at.y], [0.0; 2])], options.size / 2.0, pressure * dt);
        ctx.request_repaint();
    }

    fn tool_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        if self.editor.as_ref().is_some_and(|e| e.transform().is_some()) {
            self.transform_input(input, modifiers);
            return;
        }
        if self.editor.as_ref().is_some_and(Editor::liquifying) {
            self.liquify_input(input, modifiers);
            return;
        }
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
        if self.tools.tool == crate::tools::Tool::Gradient {
            self.gradient_input(input, modifiers);
            return;
        }
        if self.tools.tool == crate::tools::Tool::Crop {
            self.crop_input(input, modifiers);
            return;
        }
        if self.tools.tool == crate::tools::Tool::PaintBucket {
            self.paint_bucket_input(input);
            return;
        }
        let pressure = self.tablet.as_ref().map_or(1.0, Tablet::pressure);
        match input {
            ToolInput::StrokeBegin(p) => {
                let Some(mut paint) = self.tools.paint(editor.target, &editor.doc.profile, p)
                else {
                    return;
                };
                if editor.target == Target::Pixels
                    && editor.doc.layer(editor.active).is_some_and(|l| !l.can_paint_pixels())
                {
                    let name = self.tools.tool.name().to_lowercase();
                    self.message(format!("Could not use the {name} because the layer is locked"), true);
                    return;
                }
                // With transparency locked, the eraser paints the background
                // colour, as in Photoshop.
                let locked = editor.doc.layer(editor.active).is_some_and(|l| l.lock_alpha());
                if paint == Paint::Erase && locked && editor.target == Target::Pixels {
                    let background = editor.doc.profile.from_srgb8(self.tools.background);
                    paint = Paint::Color(background.unwrap_or([65535; 4]));
                }
                let (settings, sample) = (self.tools.settings(), self.tools.sample_from());
                if editor.begin_stroke(settings, paint, sample) {
                    editor.stroke_to(p.x, p.y, pressure);
                }
            }
            ToolInput::StrokeMove(p) => editor.stroke_to(p.x, p.y, pressure),
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
                        self.tools.sample_from(),
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
                // Ctrl+click picks the layer under the pointer, then moves
                // it (Photoshop's Auto-Select).
                if modifiers.command
                    && p.x >= 0.0
                    && p.y >= 0.0
                    && let Some(id) = editor.doc.layer_at(p.x as u32, p.y as u32)
                {
                    self.layers.pick(editor, id);
                }
                if !editor.begin_move("Move", modifiers.alt, background) {
                    if editor.doc.layer(editor.active).is_some_and(|l| !l.can_move()) {
                        self.message("Could not use the move tool because the layer is locked", true);
                    }
                    return;
                }
                self.move_from = Some(p);
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

    /// Whether the active layer (or its mask, when targeted) can't be filled
    /// with `tool`, saying so.
    fn refuse_locked(&mut self, tool: &str) -> bool {
        let Some(editor) = &self.editor else {
            return true;
        };
        let layer = editor.doc.layer(editor.active);
        let locked = match editor.target {
            Target::Pixels => layer.is_some_and(|l| !l.can_paint_pixels()),
            Target::Mask => layer.is_some_and(|l| l.mask.is_none() || l.locks.all),
            Target::QuickMask => false,
        };
        if locked {
            self.message(format!("Could not use {tool} tool because the layer is locked"), true);
        }
        locked
    }

    /// Dragging with the Gradient tool to fill pixels or a mask.
    fn gradient_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        match input {
            ToolInput::StrokeBegin(p) => {
                if self.refuse_locked("the gradient") {
                    return;
                }
                self.drawing = Some((vec![p], Combine::Replace));
            }
            ToolInput::StrokeMove(p) => {
                if let Some((points, _)) = &mut self.drawing {
                    points.truncate(1);
                    points.push(p);
                }
            }
            ToolInput::StrokeEnd => {
                let Some((points, _)) = self.drawing.take() else {
                    return;
                };
                let p0 = points[0];
                let p1 = if points.len() >= 2 { points[1] } else { p0 };
                let p1 = if modifiers.shift {
                    p0 + constrain_45(p1 - p0)
                } else {
                    p1
                };
                let id = editor.active;
                let target = editor.target;
                let kind = self.tools.gradient_type;
                let colors = self.tools.gradient_colors;
                let reverse = self.tools.gradient_reverse;
                let opacity = self.tools.gradient_opacity;
                let fg = self.tools.foreground;
                let bg = self.tools.background;
                let profile = editor.doc.profile.clone();

                let params =
                    ops::GradientParams::new((p0.x, p0.y), (p1.x, p1.y), kind, reverse);
                // On a mask, the colours' grey levels; to transparent keeps
                // the mask as it is at the far end.
                let v0 = crate::tools::grey(fg);
                let v1 = match colors {
                    crate::tools::GradientColors::ForegroundToBackground => Some(crate::tools::grey(bg)),
                    crate::tools::GradientColors::ForegroundToTransparent => None,
                };
                editor.edit("Gradient", |doc, _| {
                    let selection = doc.selection.clone();
                    match target {
                        Target::QuickMask => {
                            let (w, h) = (doc.width, doc.height);
                            let sel = doc.selection.get_or_insert_with(|| Selection::all(w, h));
                            sel.coverage = ops::apply_gradient_mask(
                                &sel.coverage,
                                params,
                                v0,
                                v1,
                                opacity,
                                None,
                            );
                        }
                        Target::Mask => {
                            let Some(layer) = doc.layer_mut(id) else {
                                return;
                            };
                            if let Some(mask) = layer.mask.as_mut() {
                                mask.pixels = ops::apply_gradient_mask(
                                    &mask.pixels,
                                    params,
                                    v0,
                                    v1,
                                    opacity,
                                    selection.as_ref(),
                                );
                            }
                        }
                        Target::Pixels => {
                            let Some(layer) = doc.layer_mut(id) else {
                                return;
                            };
                            if !layer.can_paint_pixels() {
                                return;
                            }
                            let c0 = profile.from_srgb8(fg).unwrap_or([0, 0, 0, u16::MAX]);
                            let c1 = match colors {
                                crate::tools::GradientColors::ForegroundToBackground => profile
                                    .from_srgb8(bg)
                                    .unwrap_or([u16::MAX, u16::MAX, u16::MAX, u16::MAX]),
                                crate::tools::GradientColors::ForegroundToTransparent => {
                                    [c0[0], c0[1], c0[2], 0]
                                }
                            };
                            layer.pixels = ops::apply_gradient_pixels(
                                &layer.pixels,
                                params,
                                c0,
                                c1,
                                opacity,
                                layer.lock_alpha(),
                                selection.as_ref(),
                            );
                        }
                    }
                });
            }
            ToolInput::Sample(_) | ToolInput::BrushDrag { .. } => {}
        }
    }

    /// Clicking with the Paint Bucket fills the similar colours round the
    /// click with the foreground colour, within the selection.
    fn paint_bucket_input(&mut self, input: ToolInput) {
        let ToolInput::StrokeBegin(p) = input else {
            return;
        };
        if self.refuse_locked("the paint bucket") {
            return;
        }
        let Some(editor) = &mut self.editor else {
            return;
        };
        if p.x < 0.0 || p.y < 0.0 {
            return;
        }
        let t = &self.tools;
        let sample = if t.bucket_all_layers { Sample::All } else { Sample::Current };
        let tolerance = omapix_engine::raster::widen(t.bucket_tolerance);
        let start = (p.x as u32, p.y as u32);
        let Some(mut area) = editor.wand_region(start, tolerance, t.bucket_contiguous, t.bucket_anti_alias, sample)
        else {
            return;
        };
        if let Some(selection) = &editor.doc.selection
            && editor.target != Target::QuickMask
        {
            area = area.combine(selection, Combine::Intersect);
        }
        let (w, h) = (editor.doc.width, editor.doc.height);
        let opacity = Tiled::new(w, h, (t.bucket_opacity.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16);
        let area = area.combine(&Selection::from_coverage(opacity), Combine::Intersect);
        fill_within(editor, "Paint Bucket", Some(t.foreground), t.background, Some(area));
    }

    /// Drawing a selection with the marquee or lasso.
    fn selection_input(&mut self, input: ToolInput, modifiers: egui::Modifiers) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let threshold = self.tools.ai_threshold;
        if self.tools.tool == crate::tools::Tool::QuickSelection {
            let how = Combine::from_modifiers(modifiers.shift, modifiers.alt);
            match input {
                ToolInput::StrokeBegin(p) => self.objects.begin_stroke(editor, p, how, threshold),
                ToolInput::StrokeMove(p) => self.objects.paint(p, self.tools.settings().size / 2.0, threshold),
                ToolInput::StrokeEnd => self.objects.end_stroke(threshold),
                ToolInput::Sample(_) | ToolInput::BrushDrag { .. } => {}
            }
            return;
        }
        match input {
            ToolInput::StrokeBegin(p) => {
                let how = Combine::from_modifiers(modifiers.shift, modifiers.alt);
                self.drawing = Some((vec![p], how));
            }
            ToolInput::StrokeMove(p) => {
                if let Some((points, _)) = &mut self.drawing {
                    match self.tools.tool {
                        crate::tools::Tool::Marquee
                        | crate::tools::Tool::EllipticalMarquee
                        | crate::tools::Tool::ObjectSelection => {
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
                if self.tools.tool == crate::tools::Tool::ObjectSelection {
                    let prompt = crate::object_selection::prompt(points[0], *points.last().expect("a point"));
                    self.objects.click(editor, prompt, how, threshold);
                    return;
                }
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
                        self.tools.wand_sample,
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
        let idle = self.opening.is_empty()
            && self.file_job.is_none()
            && self.dialog.is_none()
            && self.face_selection.busy().is_none()
            && self.whitening.busy().is_none()
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
                    command,
                    radius,
                    coarse,
                    ..
                }) = self.dialog.take()
                {
                    self.apply_radius(command, radius, coarse, ctx);
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
            ScriptStep::Tolerance(t) => {
                self.tools.wand_tolerance = t;
                self.tools.bucket_tolerance = t;
            }
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
            ScriptStep::AutoRetouch(preset) => {
                self.run(Command::AutoRetouch, ctx);
                if let Some(Dialog::AutoRetouch(dialog)) = &mut self.dialog {
                    if let Some(preset) = preset {
                        dialog.all = preset.settings();
                    }
                    dialog.accept = true;
                }
            }
            ScriptStep::GenerativeFill(prompt) => {
                self.run(Command::GenerativeFill, ctx);
                if let Some(Dialog::GenerativeFill(dialog)) = &mut self.dialog {
                    dialog.prompt = prompt;
                    dialog.accept = true;
                }
            }
        }
        ctx.request_repaint();
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.editor {
            Some(e) => format!("{} — Omapix", tab_name(e)),
            None => "Omapix".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    /// Keep the tool options once they've changed, and any drag changing
    /// them is over.
    fn save_tools(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let text = toml::to_string(&self.tools).unwrap_or_default();
        if text != self.saved_tools {
            crate::settings::save(self.tools_path.as_deref(), &self.tools, "tool settings");
            self.saved_tools = text;
        }
    }
}


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

/// Filters judged at 100 %, which show a preview box in their dialogs.
fn has_preview_box(filter: &LayerFilter) -> bool {
    matches!(
        filter,
        LayerFilter::UnsharpMask { .. } | LayerFilter::SmartSharpen(_) | LayerFilter::ReduceNoise(_) | LayerFilter::SmartBlur(_)
    )
}

/// The 100 % preview box in a filter's dialog: the active layer filtered.
/// (Filtering a mask, it still shows the layer's pixels.)
fn filter_preview_box(ui: &mut Ui, filter: &LayerFilter, editor: Option<&Editor>, cache: &mut Option<FilterPreview>) {
    let Some(editor) = editor else {
        return;
    };
    let Some(layer) = editor.doc.layer(editor.active) else {
        return;
    };
    let image = &layer.pixels;
    let mut centre = cache.as_ref().map_or_else(|| editor.canvas.view_centre(), |p| p.centre);
    let preview = PreviewBox::new(ui, &mut centre, (image.width(), image.height()));
    let ((x, y), (w, h)) = (preview.corner, preview.size);
    let key = (*filter, (x, y));
    if cache.as_ref().is_none_or(|p| p.key != key) {
        let texture = |name, pixels: Vec<omapix_engine::Pixel>| preview.texture(ui.ctx(), editor, name, &pixels);
        *cache = Some(FilterPreview {
            centre,
            key,
            filtered: texture("filter-preview", filter.preview(image, x, y, w, h)),
            unfiltered: texture("filter-preview-before", image.crop(x, y, w, h)),
        });
    }
    let Some(cached) = cache.as_mut() else {
        return;
    };
    cached.centre = centre;
    preview.show(ui, &cached.filtered, &cached.unfiltered);
    ui.add_space(8.0);
}

/// Smart Sharpen's settings, for its dialog.
fn smart_sharpen_controls(ui: &mut Ui, options: &mut SmartSharpenOptions, hint: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label("Amount");
        ui.add(
            egui::Slider::new(&mut options.amount, 1.0..=500.0)
                .suffix(" %")
                .fixed_decimals(0),
        );
    });
    radius_field(ui, &mut options.radius, "Radius", 0.1..=250.0);
    ui.horizontal(|ui| {
        ui.label("Reduce Noise");
        ui.add(
            egui::Slider::new(&mut options.reduce_noise, 0.0..=100.0)
                .suffix(" %")
                .fixed_decimals(0),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Remove:");
        ui.radio_value(&mut options.remove, SharpenRemove::GaussianBlur, "Gaussian Blur");
        ui.radio_value(&mut options.remove, SharpenRemove::LensBlur, "Lens Blur");
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Shadows: Fade Amount");
        ui.add(
            egui::Slider::new(&mut options.shadow_fade, 0.0..=100.0)
                .suffix(" %")
                .fixed_decimals(0),
        );
    });
    ui.horizontal(|ui| {
        ui.label("Highlights: Fade Amount");
        ui.add(
            egui::Slider::new(&mut options.highlight_fade, 0.0..=100.0)
                .suffix(" %")
                .fixed_decimals(0),
        );
    });
    ui.label(
        RichText::new("Sharpens luminance only. Judge it at 100 %.")
            .color(hint),
    );
}

/// Reduce Noise's settings, for its dialog.
fn reduce_noise_controls(ui: &mut Ui, options: &mut ReduceNoiseOptions, hint: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label("Strength");
        ui.add(
            egui::Slider::new(&mut options.strength, 0.0..=10.0)
                .fixed_decimals(0),
        );
    });
    for (label, value) in [
        ("Preserve Details", &mut options.preserve_details),
        ("Reduce Color Noise", &mut options.reduce_color_noise),
        ("Sharpen Details", &mut options.sharpen_details),
    ] {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::Slider::new(value, 0.0..=100.0).suffix(" %").fixed_decimals(0));
        });
    }
    ui.label(
        RichText::new("Smooths noise while preserving edges. Judge it at 100 %.")
            .color(hint),
    );
}

/// Smart Blur's settings, for its dialog.
fn smart_blur_controls(ui: &mut Ui, options: &mut SmartBlurOptions, hint: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label("Radius");
        ui.add(
            egui::Slider::new(&mut options.radius, 0.1..=100.0)
                .suffix(" px")
                .fixed_decimals(1),
        );
    });
    ui.horizontal(|ui| {
        ui.label("Threshold");
        ui.add(
            egui::Slider::new(&mut options.threshold, 0.1..=100.0)
                .fixed_decimals(1),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Quality:");
        ui.radio_value(&mut options.quality, SmartBlurQuality::Low, "Low");
        ui.radio_value(&mut options.quality, SmartBlurQuality::Medium, "Medium");
        ui.radio_value(&mut options.quality, SmartBlurQuality::High, "High");
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Mode:");
        ui.radio_value(&mut options.mode, SmartBlurMode::Normal, "Normal");
        ui.radio_value(&mut options.mode, SmartBlurMode::EdgeOnly, "Edge Only");
        ui.radio_value(&mut options.mode, SmartBlurMode::OverlayEdge, "Overlay Edge");
    });
    ui.label(
        RichText::new("Blurs areas of similar tone, keeping edges sharp.")
            .color(hint),
    );
}

/// Frequency Separation's radius until one's chosen: about 8.6 px on a
/// 24 MP frame, scaling with the image's size.
fn separation_radius(editor: &Editor) -> f32 {
    let longest = editor.doc.width.max(editor.doc.height) as f32;
    (longest / 700.0 * 10.0).round() / 10.0
}

/// Frequency Separation (3 Bands)' fine and coarse radii until they're
/// chosen, from the 2-band one's `radius`.
fn three_band_radii(radius: f32) -> (f32, f32) {
    let round = |r: f32| (r * 10.0).round() / 10.0;
    (round(radius / 2.0), round(radius * 3.0))
}

/// Select › Modify's remembered radius (Border's width) for `command`.
fn modify_radius(filters: &mut FilterSettings, command: Command) -> Option<&mut f32> {
    Some(match command {
        Command::BorderSelection => &mut filters.border_width,
        Command::SmoothSelection => &mut filters.smooth_radius,
        Command::ExpandSelection => &mut filters.expand_radius,
        Command::ContractSelection => &mut filters.contract_radius,
        Command::Feather => &mut filters.feather_radius,
        _ => return None,
    })
}

/// A radius (or with `label`, a width) in pixels, for filter and radius
/// dialogs.
fn radius_field(ui: &mut Ui, radius: &mut f32, label: &str, range: std::ops::RangeInclusive<f32>) {
    ui.horizontal(|ui| {
        ui.label(label);
        let value = egui::DragValue::new(radius)
            .range(range)
            .speed(0.1)
            .suffix(" px")
            .fixed_decimals(1);
        ui.add(value);
    });
}

/// Crop the document to `w` × `h` from (`x`, `y`), which can be outside it
/// (extending the canvas, transparent), deselecting.
fn crop(editor: &mut Editor, x: i32, y: i32, w: u32, h: u32) {
    if (x, y, w, h) != (0, 0, editor.doc.width, editor.doc.height) {
        editor.edit("Crop", |doc, _| {
            doc.resize_canvas(w, h, -x, -y, None);
            doc.selection = None;
        });
    }
}

/// The largest width or height Image Size and Canvas Size allow.
const MAX_SIDE: u32 = 30_000;
/// File › New's size with nothing open.
const NEW_IMAGE_SIZE: (u32, u32) = (1920, 1080);

/// A width or height in pixels, with the percentage of `now` it is.
fn size_field(ui: &mut Ui, label: &str, value: &mut u32, now: u32) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).range(1..=MAX_SIDE).suffix(" px"));
        let percent = f64::from(*value) / f64::from(now) * 100.0;
        ui.weak(format!("{percent:.0} %"));
    });
}

/// The Lock commands, in Photoshop's order.
const LOCKS: [Command; 4] = [
    Command::LockTransparent,
    Command::LockPixels,
    Command::LockPosition,
    Command::LockAll,
];

/// The flag a Lock command toggles.
fn lock_flag(locks: &mut Locks, cmd: Command) -> &mut bool {
    match cmd {
        Command::LockTransparent => &mut locks.transparency,
        Command::LockPixels => &mut locks.pixels,
        Command::LockPosition => &mut locks.position,
        _ => &mut locks.all,
    }
}

/// Fill the active layer (or its mask) where selected, like Photoshop's
/// Alt+Backspace, as an undo step called `label`. `None` clears instead
/// (Delete): pixels to transparency, masks to the background colour's grey,
/// as Photoshop does.
fn fill(editor: &mut Editor, label: &str, colour: Option<[u8; 3]>, background: [u8; 3]) {
    fill_within(editor, label, colour, background, None);
}

/// `fill`, but only within `area` (partly where it's partly covered)
/// rather than the selection, when given.
fn fill_within(
    editor: &mut Editor,
    label: &str,
    colour: Option<[u8; 3]>,
    background: [u8; 3],
    area: Option<Selection>,
) {
    let id = editor.active;
    let target = editor.target;
    let profile = editor.doc.profile.clone();
    editor.edit(label, |doc, _| {
        if target == Target::QuickMask {
            let (w, h) = (doc.width, doc.height);
            let sel = doc.selection.get_or_insert_with(|| Selection::all(w, h));
            let grey = crate::tools::grey(colour.unwrap_or(background));
            sel.coverage = ops::fill_mask(&sel.coverage, grey, area.as_ref());
            return;
        }
        let selection = area.or_else(|| doc.selection.clone());
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
                if !layer.can_paint_pixels() {
                    return;
                }
                // With transparency locked, Delete fills with the background
                // colour, and only the colour of what's there changes.
                let colour = colour.or(layer.lock_alpha().then_some(background));
                let pixel =
                    colour.map(|rgb| profile.from_srgb8(rgb).unwrap_or([0, 0, 0, u16::MAX]));
                let filled = ops::fill_pixels(&layer.pixels, pixel, selection.as_ref());
                layer.pixels = if layer.lock_alpha() {
                    ops::keep_alpha(&layer.pixels, filled)
                } else {
                    filled
                };
            }
            Target::QuickMask => unreachable!(),
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
    } else if editor.target == Target::QuickMask {
        let sel = selection?;
        Clip::copy_mask(&sel.coverage, None, &doc.profile)
    } else {
        let layer = doc.layer(editor.active)?;
        match (editor.target, &layer.mask) {
            (Target::Mask, Some(mask)) => Clip::copy_mask(&mask.pixels, selection, &doc.profile),
            _ => Clip::copy(&layer.pixels, selection, &doc.profile),
        }
    }?;
    (!clip.is_empty()).then_some(clip)
}

/// The images at `paths` as they look, each with its file's name, all in
/// the first one's colour space.
fn load_frames(paths: &[PathBuf]) -> Result<(Vec<Frame>, ColorProfile), String> {
    let mut frames = Vec::new();
    let mut profile: Option<ColorProfile> = None;
    for path in paths {
        let (clip, name) = load_clip(path).map_err(|e| format!("couldn't open {}: {e}", path.display()))?;
        let profile = profile.get_or_insert_with(|| clip.profile.clone());
        let [_, _, w, h] = clip.bounds;
        frames.push((name, clip.place(w, h, profile, None).map_err(|e| e.to_string())?));
    }
    Ok((frames, profile.ok_or("no photos")?))
}

fn load_clip(path: &Path) -> Result<(Clip, String), String> {
    let mut doc = omapix_engine::io::load(path).map_err(|e| e.to_string())?;
    let _ = ops::prepare_for_editing(&mut doc);
    let raster = doc.composite();
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Layer".into());
    let clip = Clip {
        bounds: [0, 0, raster.width(), raster.height()],
        pixels: Tiled::from_raster(&raster),
        profile: doc.profile,
        in_place: false,
    };
    Ok((clip, name))
}

/// Paste or place `clip` as a new layer above the active one.
fn paste(
    editor: &mut Editor,
    clip: Arc<Clip>,
    name: Option<String>,
    kind: PasteKind,
    ctx: &egui::Context,
) {
    let index = editor.active_index().unwrap_or(0);
    editor.target = Target::Pixels;
    let label = if name.is_some() {
        "Place"
    } else {
        match kind {
            PasteKind::Normal => "Paste",
            PasteKind::InPlace => "Paste in Place",
            PasteKind::Into => "Paste Into",
        }
    };
    editor.edit_in_background(
        label,
        move |doc, active| match clip::paste(doc, &clip, index, kind) {
            Ok(id) => {
                *active = id;
                if let Some(name) = name
                    && let Some(layer) = doc.layer_mut(id)
                {
                    layer.name = name;
                }
            }
            Err(e) => log::error!("{label} failed: {e}"),
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
            if selected.iter().any(|&s| editor.doc.layer(s).is_some_and(|l| !l.can_delete())) {
                return;
            }
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
        Command::LockTransparent | Command::LockPixels | Command::LockPosition | Command::LockAll => {
            let mut locks = editor.doc.layer(id).map(|l| l.locks).unwrap_or_default();
            let lock = !*lock_flag(&mut locks, cmd);
            let label = if lock {
                cmd.label().to_owned()
            } else {
                cmd.label().replacen("Lock", "Unlock", 1)
            };
            // Only layers with pixels have pixels to lock.
            let pixels = matches!(cmd, Command::LockTransparent | Command::LockPixels);
            editor.edit(&label, |doc, _| {
                for &s in &selected {
                    let Some(l) = doc.layer_mut(s).filter(|l| !pixels || l.has_pixels()) else {
                        continue;
                    };
                    if cmd == Command::LockAll {
                        l.locks.set_all(lock);
                    } else {
                        *lock_flag(&mut l.locks, cmd) = lock;
                        l.locks.all &= lock;
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
        Command::AddMask | Command::AddMaskHideAll => {
            // With a selection, the mask reveals just the selection. Hide All
            // (Alt+click the mask button) is the opposite: black, or hiding
            // just the selection.
            editor.edit("Add Layer Mask", |doc, _| {
                let mut mask = match &doc.selection {
                    Some(sel) => Mask {
                        pixels: sel.coverage.clone(),
                        enabled: true,
                    },
                    None => Mask::white(w, h),
                };
                if cmd == Command::AddMaskHideAll {
                    mask.invert();
                }
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
            if editor.target == Target::QuickMask {
                editor.edit("Invert Selection", |doc, _| {
                    let (w, h) = (doc.width, doc.height);
                    let sel = doc.selection.get_or_insert_with(|| Selection::all(w, h));
                    *sel = sel.invert();
                });
            } else if editor.target == Target::Mask {
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
            editor.load_channel(channel, Combine::Replace);
        }
        Command::SaveSelection => {
            editor.edit("Save Selection", |doc, _| {
                doc.save_selection();
            });
        }
        Command::DeleteChannel => {
            if let View::Alpha(channel) = editor.view() {
                editor.set_view(View::Image);
                editor.edit("Delete Channel", |doc, _| doc.channels.retain(|c| c.id != channel));
            }
        }
        Command::ViewComposite => editor.set_view(View::Image),
        Command::ViewRed => editor.set_view(View::Channel(Channel::Red)),
        Command::ViewGreen => editor.set_view(View::Channel(Channel::Green)),
        Command::ViewBlue => editor.set_view(View::Channel(Channel::Blue)),
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
        Command::Rotate180
        | Command::Rotate90Cw
        | Command::Rotate90Ccw
        | Command::FlipCanvasHorizontal
        | Command::FlipCanvasVertical => {
            let (label, orientation) = match cmd {
                Command::Rotate180 => ("Rotate 180°", Orientation::Rotate180),
                Command::Rotate90Cw => ("Rotate 90° Clockwise", Orientation::Rotate90Cw),
                Command::Rotate90Ccw => ("Rotate 90° Counter Clockwise", Orientation::Rotate90Ccw),
                Command::FlipCanvasHorizontal => ("Flip Canvas Horizontal", Orientation::FlipHorizontal),
                Command::FlipCanvasVertical => ("Flip Canvas Vertical", Orientation::FlipVertical),
                _ => unreachable!(),
            };
            editor.edit(label, |doc, _| {
                doc.apply_orientation(orientation);
            });
        }
        // Image › Crop: to the selection's bounds, deselecting.
        Command::Crop => {
            if let Some([x, y, w, h]) = editor.doc.selection.as_ref().and_then(|s| s.bounds()) {
                crop(editor, x as i32, y as i32, w, h);
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
        Command::QuickMask => {
            editor.toggle_quick_mask();
        }
        _ => {}
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::drop::take(raw_input);
        if let Some(tablet) = &mut self.tablet {
            tablet.take(raw_input, ctx.zoom_factor(), ctx.input(|i| i.modifiers));
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if let Some(editor) = &self.editor {
            self.tools.follow_target(editor.target);
        }
        self.check_liquify_keys(ctx);
        let liquifying = self.editor.as_ref().is_some_and(Editor::liquifying);
        // While a text field (layer rename) has focus, keys edit the text.
        if self.dialog.is_none() && !ctx.egui_wants_keyboard_input() {
            for cmd in Command::pressed(ctx, &mut self.v_down) {
                self.run(cmd, ctx);
            }
            if liquifying {
                // Liquify's keys are its own.
            } else if let Some(opacity) = self.tools.keys(ctx)
                && let Some(editor) = &mut self.editor
            {
                let id = editor.active;
                if editor.doc.layer(id).is_some_and(|l| l.can_modify()) {
                    editor.edit("Opacity", |doc, _| {
                        if let Some(l) = doc.layer_mut(id) {
                            l.opacity = opacity;
                        }
                    });
                }
            }
            if let Some((dx, dy)) = self.tools.nudge(ctx) {
                let background = crate::tools::grey(self.tools.background);
                if let Some(editor) = &mut self.editor {
                    if editor.begin_move("Nudge", false, background) {
                        editor.move_to(dx, dy);
                        editor.end_move();
                    } else if editor.doc.layer(editor.active).is_some_and(|l| !l.can_move()) {
                        self.message("Could not use the move tool because the layer is locked", true);
                    }
                }
            }
        }
        self.check_escape(ctx);
        self.check_transform_keys(ctx);
        self.check_crop_keys(ctx);
        self.check_deselect(ctx);
        self.liquify_held(ctx);
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
        self.save_tools(ctx);
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
        if let Some(editor) = &mut self.editor {
            let target = editor.target;
            egui::Panel::top("options")
                .frame(bar)
                .show(ui, |ui| {
                    if editor.liquifying() {
                        let mut restore = editor.liquify_restore();
                        let missing = [FACE_MODELS, POSE_MODELS].map(|models| missing(models, &self.models));
                        self.tools.liquify.options_bar(ui, &self.theme, &mut restore, [&missing[0], &missing[1]]);
                        if restore != editor.liquify_restore() {
                            editor.set_liquify_restore(restore);
                        }
                    } else {
                        self.tools.options_bar(ui, target, &self.theme);
                    }
                });
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
                    if let Some(top_tab) = self.top_tab {
                        ui.horizontal(|ui| {
                            for (tab, cmd) in TopTab::ALL {
                                let is_active = self.top_tab == Some(tab);
                                let text = if is_active {
                                    RichText::new(cmd.label()).color(self.theme.foreground).strong()
                                } else {
                                    RichText::new(cmd.label()).color(self.theme.dark_foreground)
                                };
                                if ui.add(Button::new(text).frame(false)).clicked() {
                                    TopTab::toggle(&mut self.top_tab, tab);
                                }
                                ui.add_space(8.0);
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("✕").on_hover_text("Close").clicked() {
                                    self.top_tab = None;
                                }
                            });
                        });
                        ui.separator();

                        match top_tab {
                            TopTab::Navigator => self.navigator.show(ui, editor, &self.theme),
                            TopTab::Histogram => self.histogram.show(ui, editor, &self.theme),
                        }
                        ui.separator();
                    }

                    ui.horizontal(|ui| {
                        for (tab, command) in RightTab::ALL {
                            let text = if self.right_tab == tab {
                                RichText::new(command.label()).color(self.theme.foreground).strong()
                            } else {
                                RichText::new(command.label()).color(self.theme.dark_foreground)
                            };
                            if ui.add(Button::new(text).frame(false)).clicked() {
                                self.right_tab = tab;
                            }
                            ui.add_space(8.0);
                        }
                    });
                    ui.separator();

                    match self.right_tab {
                        RightTab::Layers => {
                            self.properties.show(ui, editor, &self.theme);
                            command = self.layers.show(ui, editor, &self.theme);
                        }
                        RightTab::Channels => {
                            command = crate::channels_panel::show(ui, editor, &self.theme);
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
        if !self.parked.is_empty() {
            egui::Panel::top("tabs").frame(bar).show(ui, |ui| self.tab_bar(ui));
        }
        let pasteboard = self.theme.pasteboard();
        let brush = self.tools.settings();
        let source = self.tools.source_marker();
        let tool = self.tools.tool;
        let shift = ui.input(|i| i.modifiers.shift);
        let drawing: Option<Vec<Pos2>> = self.drawing.as_ref().map(|(points, _)| match tool {
            // Object Selection's box, as it's dragged.
            crate::tools::Tool::ObjectSelection if points.len() == 2 => {
                let (a, b) = (points[0], points[1]);
                vec![a, egui::pos2(b.x, a.y), b, egui::pos2(a.x, b.y), a]
            }
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
            // Show the gradient drag as a line (constrained to 45° with Shift).
            crate::tools::Tool::Gradient if points.len() == 2 => {
                let a = points[0];
                let b = if shift {
                    a + constrain_45(points[1] - a)
                } else {
                    points[1]
                };
                vec![a, b]
            }
            _ => points.clone(),
        });
        let crop_box = (tool == crate::tools::Tool::Crop).then(|| self.crop_box());
        let crop_handle = self.crop_drag.map(|d| match d {
            CropDrag::Box(drag, _) => drag.handle(),
            CropDrag::New(_) => crate::free_transform::Handle::Rotate,
        });
        let mut input = None;
        let mut menu_on = None;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Some(editor) = &mut self.editor {
                let idle = editor.busy().is_none();
                // An AI selection being painted or its threshold dragged
                // shows as it will be.
                let selection = self.objects.preview.as_ref().or(editor.doc.selection.as_ref());
                let outlines = if editor.hide_selection_edges || editor.view() == View::QuickMask {
                    &[][..]
                } else {
                    selection.map_or(&[][..], |s| &s.outlines[..])
                };
                let modifiers = ui.input(|i| i.modifiers);
                // An armed Curves or Levels eyedropper, or Free Transform, takes the
                // pointer from the tools.
                let liquify_brush = editor.liquifying().then_some(self.tools.liquify.size);
                let tools_off = self.properties.eyedropper.is_some()
                    || editor.transform().is_some()
                    || liquify_brush.is_some();
                let overlay = crate::canvas::Overlay {
                    tool: idle,
                    alt_samples: !tools_off && tool.paints(),
                    samples: !tools_off && tool == crate::tools::Tool::Eyedropper,
                    // Holding Alt picks up a colour: a crosshair, not the brush,
                    // unless Alt+right-dragging to resize it.
                    brush: (!tools_off
                        && tool.has_brush()
                        && !(modifiers.alt && tool.alt_picks_colour() && !ui.input(|i| i.pointer.secondary_down())))
                    .then_some(brush.size)
                    .or(liquify_brush),
                    moves: !tools_off && tool == crate::tools::Tool::Move,
                    source,
                    selection: outlines,
                    drawing: (!tools_off).then_some(drawing.as_deref()).flatten(),
                    badge: (idle && !tools_off)
                        .then(|| crate::tools::cursor_badge(tool, modifiers))
                        .flatten(),
                    transform: editor.transform().map(|(bounds, t)| {
                        // The handle being dragged, or the one under the pointer.
                        let reach = editor.canvas.image_per_point() * HANDLE_REACH;
                        let handle = self.transform_drag.map(|d| d.handle()).or_else(|| {
                            let p = editor.canvas.pointer?;
                            Some(crate::free_transform::hit(bounds, &t, p, reach))
                        });
                        let cursor = handle.map_or(egui::CursorIcon::Default, |h| {
                            crate::free_transform::cursor(bounds, &t, h)
                        });
                        (crate::free_transform::corners(bounds, &t), cursor)
                    }).or_else(|| {
                        // The Crop tool's box: outside it, a crosshair to draw a new one.
                        let rect = crop_box.filter(|_| !tools_off)?;
                        let t = omapix_engine::transform::Affine::IDENTITY;
                        let reach = editor.canvas.image_per_point() * HANDLE_REACH;
                        let handle = crop_handle.or_else(|| {
                            Some(crate::free_transform::hit(rect, &t, editor.canvas.pointer?, reach))
                        });
                        let cursor = handle.map_or(egui::CursorIcon::Crosshair, |h| {
                            crate::free_transform::cursor(rect, &t, h)
                        });
                        Some((crate::free_transform::corners(rect, &t), cursor))
                    }),
                    crop: crop_box.is_some() && !tools_off,
                    spots: match &self.dialog {
                        Some(Dialog::HealBlemishes(dialog)) => dialog.spots(),
                        Some(Dialog::AutoRetouch(dialog)) => dialog.spots(),
                        _ => &[],
                    },
                };
                let (tool_input, response) = editor.canvas.show(ui, pasteboard, overlay);
                input = tool_input;
                // Alt+right-drag resizes the brush instead.
                menu_on = (!tools_off && !modifiers.alt).then_some(response);
            } else {
                ui.painter().rect_filled(ui.max_rect(), 0.0, pasteboard);
                self.empty_state(ui);
            }
        });
        if let Some(input) = input {
            let modifiers = ui.input(|i| i.modifiers);
            self.tool_input(input, modifiers);
        }
        if let Some(response) = menu_on {
            response.context_menu(|ui| self.canvas_menu(ui));
        }
        let ctx = ui.ctx().clone();
        self.face_liquify(&ctx);
        self.dialogs(&ctx);
        if let Some(tablet) = &self.tablet {
            tablet.set_cursor(ctx.output(|o| o.cursor_icon));
        }
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
pub(crate) fn constrain_45(d: Vec2) -> Vec2 {
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

    #[test]
    fn proof_colors_and_the_gamut_warning_show_through_proof_setups_profile() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let shown = |app: &App| {
            let mut out = [[0u8; 4]];
            app.editor.as_ref().unwrap().canvas.transform().convert(&[[65535, 0, 0, 65535]], &mut out);
            out[0]
        };
        // Proofed for the web, an sRGB image is as it was.
        app.run(Command::ProofColors, &ctx);
        assert!(app.proof.colors);
        assert_eq!(shown(&app), [255, 0, 0, 255]);
        app.run(Command::ProofColors, &ctx);
        assert!(app.proof.proof().is_none());

        // A profile with duller colours than sRGB's (as a monitor's EDID
        // would give them), from a file: choosing it turns the proof on.
        let dir = std::env::temp_dir().join(format!("omapix-proof-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        edid[23] = 120;
        edid[27..35].copy_from_slice(&[153, 89, 76, 140, 41, 25, 80, 84]);
        let dull = ColorProfile::from_edid(&edid, "Dull").unwrap();
        let path = dir.join("dull.icc");
        std::fs::write(&path, dull.icc().unwrap()).unwrap();
        app.set_proof_profile(path.clone());
        assert!(app.proof.colors && app.filters.proof_profile == Some(path));
        assert_eq!(app.proof.name(), "Dull (EDID)");
        let [r, g, b, _] = shown(&app);
        assert!(r > 200 && g > 30 && b > 15, "{:?}", shown(&app));
        // Red is a colour it hasn't got: grey, with or without the proof.
        app.run(Command::GamutWarning, &ctx);
        assert_eq!(shown(&app)[..3], [127; 3]);
        app.run(Command::ProofColors, &ctx);
        assert_eq!(shown(&app)[..3], [127; 3]);
        // An image opened meanwhile is shown the same way.
        app.add_tab(editor_with_selection());
        assert_eq!(shown(&app)[..3], [127; 3]);

        // What isn't a profile is refused, and the web's sRGB has it all.
        let bad = dir.join("bad.icc");
        std::fs::write(&bad, b"not a profile").unwrap();
        app.set_proof_profile(bad);
        assert!(app.status.as_ref().is_some_and(|s| s.1));
        assert_eq!(app.proof.name(), "Dull (EDID)");
        app.run(Command::ProofSetupWeb, &ctx);
        assert_eq!((app.proof.name(), app.filters.proof_profile.as_ref()), ("sRGB", None));
        assert_eq!(shown(&app), [255, 0, 0, 255]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

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

        paste(&mut editor, Arc::new(clip), None, PasteKind::Normal, &ctx);
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
    fn paste_into_adds_layer_with_mask_from_selection_and_targets_pixels() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let original_sel = app.editor.as_ref().unwrap().doc.selection.clone().unwrap();
        let clip = copy(app.editor.as_ref().unwrap(), false).unwrap();
        app.clipboard.set(clip);

        app.run(Command::PasteInto, &ctx);
        while app.editor.as_ref().unwrap().busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            app.editor.as_mut().unwrap().update(&ctx);
        }

        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.undo_label(), Some("Paste Into"));
        assert_eq!(editor.doc.layers.len(), 2);
        let pasted = &editor.doc.layers[1];
        assert_eq!(editor.active, pasted.id);
        assert_eq!(editor.target, Target::Pixels);
        assert!(editor.doc.selection.is_none());

        let mask = pasted.mask.as_ref().expect("paste into must add a layer mask");
        assert!(mask.enabled);
        assert!(mask.pixels.same_tiles(&original_sel.coverage));
    }

    #[test]
    fn paste_into_disabled_without_a_selection() {
        let mut app = test_app();
        assert!(app.enabled(Command::PasteInto));
        app.editor.as_mut().unwrap().doc.selection = None;
        assert!(!app.enabled(Command::PasteInto));
    }

    #[test]
    fn paste_in_place_pastes_an_omapix_copy_where_it_came_from() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let clip = copy(app.editor.as_ref().unwrap(), false).unwrap();
        app.clipboard.set(clip);
        // Clear background where it was copied from so we can confirm it goes back in place.
        fill(app.editor.as_mut().unwrap(), "Cut", None, [255, 255, 255]);
        assert_eq!(app.editor.as_ref().unwrap().doc.layers[0].pixels.get(150, 150)[3], 0);

        app.run(Command::PasteInPlace, &ctx);
        while app.editor.as_ref().unwrap().busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            app.editor.as_mut().unwrap().update(&ctx);
        }

        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.undo_label(), Some("Paste in Place"));
        assert_eq!(editor.doc.layers.len(), 2);
        let pasted = &editor.doc.layers[1];
        assert_eq!(editor.active, pasted.id);
        assert_eq!(pasted.pixels.get(150, 150), [30000, 30000, 30000, 65535]);
        assert_eq!(pasted.pixels.get(250, 150)[3], 0);
        assert!(editor.doc.selection.is_none());
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
        assert!(editor.doc.layer(empty).unwrap().lock_alpha());
        assert_eq!(editor.undo_label(), Some("Lock Transparent Pixels"));

        // Nothing there to fill or paint on.
        fill(&mut editor, "Fill", Some([255, 0, 0]), [255, 255, 255]);
        let settings = omapix_engine::brush::BrushSettings::default();
        assert!(editor.begin_stroke(settings, Paint::Color([0, 0, 0, 65535]), crate::tools::Sample::Current));
        editor.stroke_to(300.0, 300.0, 1.0);
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
        assert!(!editor.doc.layer(background).unwrap().lock_alpha());
    }

    #[test]
    fn image_locked_layer_refuses_stroke_and_fill_with_message() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active = app.editor.as_ref().unwrap().active;

        app.run(Command::LockPixels, &ctx);
        assert!(app.editor.as_ref().unwrap().doc.layer(active).unwrap().locks.pixels);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Lock Image Pixels"));

        let initial_pixel = app.editor.as_ref().unwrap().doc.layer(active).unwrap().pixels.get(150, 150);

        // Brush stroke is refused on locked pixels with status message
        app.tools.select(crate::tools::Tool::Brush);
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(150.0, 150.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeMove(egui::pos2(160.0, 160.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
        assert_eq!(
            app.editor.as_ref().unwrap().doc.layer(active).unwrap().pixels.get(150, 150),
            initial_pixel
        );
        assert_eq!(
            app.status.as_ref().map(|s| s.0.as_str()),
            Some("Could not use the brush because the layer is locked")
        );
        assert!(app.status.as_ref().unwrap().1);

        // Fill is refused with status message
        app.status = None;
        app.run(Command::FillForeground, &ctx);
        assert_eq!(
            app.status.as_ref().map(|s| s.0.as_str()),
            Some("Could not fill because the layer is locked")
        );
        assert_eq!(
            app.editor.as_ref().unwrap().doc.layer(active).unwrap().pixels.get(150, 150),
            initial_pixel
        );

        // Its mask can still be painted
        app.run(Command::AddMask, &ctx);
        app.editor.as_mut().unwrap().target = Target::Mask;
        app.status = None;
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(150.0, 150.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
        assert!(app.status.is_none());
    }

    #[test]
    fn position_locked_layer_refuses_move_tool() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active = app.editor.as_ref().unwrap().active;

        app.run(Command::LockPosition, &ctx);
        assert!(app.editor.as_ref().unwrap().doc.layer(active).unwrap().locks.position);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Lock Position"));

        // Move tool drag is refused with status message
        app.tools.select(crate::tools::Tool::Move);
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(100.0, 100.0)), egui::Modifiers::NONE);
        assert!(app.move_from.is_none());
        assert_eq!(
            app.status.as_ref().map(|s| s.0.as_str()),
            Some("Could not use the move tool because the layer is locked")
        );
        assert!(!app.editor.as_mut().unwrap().begin_move("Move", false, 0));
    }

    #[test]
    fn lock_all_toggles_all_flags_and_blocks_delete() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        run_on_editor(app.editor.as_mut().unwrap(), Command::NewLayer, &ctx);
        let active = app.editor.as_ref().unwrap().active;

        // Lock All sets all lock flags
        app.run(Command::LockAll, &ctx);
        let locks = app.editor.as_ref().unwrap().doc.layer(active).unwrap().locks;
        assert!(locks.all);
        assert!(locks.transparency);
        assert!(locks.pixels);
        assert!(locks.position);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Lock All"));
        assert!(!app.editor.as_ref().unwrap().doc.layer(active).unwrap().can_delete());
        assert!(!app.editor.as_ref().unwrap().doc.layer(active).unwrap().can_modify());

        // Delete is disabled and blocked
        assert!(!app.enabled(Command::DeleteLayer));
        let count = app.editor.as_ref().unwrap().doc.layers.len();
        app.run(Command::DeleteLayer, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().doc.layers.len(), count);
        run_on_editor(app.editor.as_mut().unwrap(), Command::DeleteLayer, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().doc.layers.len(), count);

        // Toggle Lock All off clears all flags and unblocks delete
        app.run(Command::LockAll, &ctx);
        let locks = app.editor.as_ref().unwrap().doc.layer(active).unwrap().locks;
        assert!(!locks.all);
        assert!(!locks.transparency);
        assert!(!locks.pixels);
        assert!(!locks.position);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Unlock All"));
        assert!(app.enabled(Command::DeleteLayer));
        assert!(app.editor.as_ref().unwrap().doc.layer(active).unwrap().can_delete());
        assert!(app.editor.as_ref().unwrap().doc.layer(active).unwrap().can_modify());
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

    #[test]
    fn adjustment_presets_are_saved_from_the_selected_layers_and_added_on_top() {
        let mut app = test_app();
        let ctx = egui::Context::default();
        assert!(!app.enabled(Command::SaveAdjustmentPreset), "a pixel layer");
        app.run(Command::NewCurves, &ctx);
        assert!(app.enabled(Command::SaveAdjustmentPreset));
        app.run(Command::SaveAdjustmentPreset, &ctx);
        assert!(matches!(&app.dialog, Some(Dialog::SavePreset { name }) if name == "Curves"));

        let editor = app.editor.as_mut().unwrap();
        let preset = Preset::from_layers(&editor.doc, &editor.selected()).unwrap();
        let background = editor.doc.layers[0].id;
        editor.active = background;
        editor.target = Target::Pixels;
        let count = editor.doc.layers.len();
        app.apply_preset("Warm", &preset);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.doc.layers.len(), count + 1);
        assert_eq!(editor.doc.index_of(editor.active), Some(1), "just above the background");
        assert_eq!(editor.target, Target::Mask);
        assert_eq!(editor.undo_label(), Some("Preset Warm"));

        // The same layers baked into a .cube.
        let path = std::env::temp_dir().join(format!("omapix-lut-{}.cube", std::process::id()));
        app.export_lut(&path);
        let mut lut = omapix_engine::adjust::ColorLookup::default();
        assert_eq!(lut.load_cube_file(&path), Ok(()));
        std::fs::remove_file(&path).unwrap();
        assert_eq!((lut.size, lut.table.len()), (33, 33 * 33 * 33));
    }

    fn test_app() -> App {
        let (_tx, rx) = channel();
        App {
            theme: Theme::default(),
            theme_rx: rx,
            editor: Some(editor_with_selection()),
            parked: Vec::new(),
            tab: 0,
            layers: LayersPanel::default(),
            properties: PropertiesPanel::default(),
            history: HistoryPanel,
            recent: RecentStore::default(),
            top_tab: None,
            histogram: HistogramPanel::default(),
            navigator: NavigatorPanel::default(),
            right_tab: RightTab::default(),
            tools: Tools::default(),
            tools_path: None,
            saved_tools: String::new(),
            opening: VecDeque::new(),
            picking: None,
            merging: None,
            file_job: None,
            batch_export: None,
            dialog: None,
            filter_preview: None,
            status: None,
            filters: FilterSettings::default(),
            defaults: FilterSettings::default(),
            allow_close: false,
            title: String::new(),
            drawing: None,
            move_from: None,
            transform_drag: None,
            crop: None,
            crop_drag: None,
            liquify_at: None,
            objects: Default::default(),
            script: VecDeque::new(),
            clipboard: Clipboard::new(false),
            pasting: None,
            from_clipboard: None,
            pending_drops: Vec::new(),
            v_down: false,
            tablet: None,
            monitor: None,
            proof: Default::default(),
            content_fill: ContentFill::default(),
            denoising: Default::default(),
            upscaling: Default::default(),
            aligning: None,
            select_subject: SelectSubject::default(),
            face_selection: FaceSelection::default(),
            whitening: Default::default(),
            face_liquify: Default::default(),
            body_liquify: Default::default(),
            models: HashMap::new(),
            models_check: None,
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
        assert_eq!(*filter, LayerFilter::from(FilterSettings::default().unsharp_mask));
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
    fn filter_preview_box_dialog_interaction() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.run(Command::UnsharpMask, &ctx);

        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        fn frame(ctx: &egui::Context, app: &mut App, mut input: egui::RawInput, screen_rect: egui::Rect) -> egui::FullOutput {
            input.screen_rect = Some(screen_rect);
            let mut out = ctx.run_ui(input, |ctx| app.dialogs(ctx));
            out.textures_delta.clear();
            out
        }

        fn find_box(shape: &egui::Shape) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Rect(r) if (r.rect.width() - 240.0).abs() < 1.0 => Some(r.rect),
                egui::Shape::Vec(v) => v.iter().find_map(find_box),
                _ => None,
            }
        }

        // First frame: modal laid out
        frame(&ctx, &mut app, egui::RawInput::default(), screen_rect);
        assert!(app.filter_preview.is_some());
        let center = app.filter_preview.as_ref().unwrap().centre;
        // Image is 600x400, canvas center is (300, 200)
        assert_eq!(center, egui::pos2(300.0, 200.0));

        // Second frame: modal visible, find box center
        let out = frame(&ctx, &mut app, egui::RawInput::default(), screen_rect);
        let box_rect = out.shapes.iter().find_map(|s| find_box(&s.shape)).expect("preview box");
        let drag_start = box_rect.center();

        // Drag inside the preview box:
        frame(
            &ctx,
            &mut app,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(drag_start),
                    egui::Event::PointerButton {
                        pos: drag_start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
                ..Default::default()
            },
            screen_rect,
        );
        frame(
            &ctx,
            &mut app,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(drag_start + egui::vec2(20.0, 10.0)),
                ],
                ..Default::default()
            },
            screen_rect,
        );
        let new_center = app.filter_preview.as_ref().unwrap().centre;
        assert!(new_center.x < center.x);

        // Releasing mouse:
        frame(
            &ctx,
            &mut app,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerButton {
                        pos: drag_start + egui::vec2(20.0, 10.0),
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    },
                ],
                ..Default::default()
            },
            screen_rect,
        );

        // Closing dialog resets preview:
        app.dialog = None;
        frame(&ctx, &mut app, egui::RawInput::default(), screen_rect);
        assert!(app.filter_preview.is_none());

        // GaussianBlur does not have a preview box:
        app.run(Command::GaussianBlur, &ctx);
        frame(&ctx, &mut app, egui::RawInput::default(), screen_rect);
        assert!(app.filter_preview.is_none());
    }

    #[test]
    fn smart_sharpen_previews_then_applies_and_remembers_its_settings() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active = app.editor.as_ref().unwrap().active;
        app.run(Command::SmartSharpen, &ctx);
        let Some(Dialog::Filter { filter, preview }) = &mut app.dialog else {
            panic!("no filter dialog");
        };
        assert!(*preview);
        assert_eq!(*filter, LayerFilter::SmartSharpen(FilterSettings::default().smart_sharpen));
        let custom = LayerFilter::SmartSharpen(SmartSharpenOptions {
            amount: 250.0,
            radius: 2.5,
            reduce_noise: 20.0,
            remove: SharpenRemove::LensBlur,
            shadow_fade: 15.0,
            highlight_fade: 25.0,
        });
        *filter = custom;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| app.dialogs(ctx));
        output.textures_delta.clear();
        let view = app.editor.as_ref().unwrap().view();
        assert_eq!(
            view,
            View::Filter {
                layer: active,
                filter: custom,
                mask: false,
            }
        );

        app.dialog = None;
        app.apply_filter(custom, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Smart Sharpen"));
        app.run(Command::SmartSharpen, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::Filter { filter, .. }) if filter == custom));
    }

    #[test]
    fn reduce_noise_previews_then_applies_and_remembers_its_settings() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active = app.editor.as_ref().unwrap().active;
        app.run(Command::ReduceNoise, &ctx);
        let Some(Dialog::Filter { filter, preview }) = &mut app.dialog else {
            panic!("no filter dialog");
        };
        assert!(*preview);
        assert_eq!(*filter, LayerFilter::ReduceNoise(FilterSettings::default().reduce_noise));
        let custom = LayerFilter::ReduceNoise(ReduceNoiseOptions {
            strength: 7.0,
            preserve_details: 30.0,
            reduce_color_noise: 50.0,
            sharpen_details: 20.0,
        });
        *filter = custom;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| app.dialogs(ctx));
        output.textures_delta.clear();
        let view = app.editor.as_ref().unwrap().view();
        assert_eq!(
            view,
            View::Filter {
                layer: active,
                filter: custom,
                mask: false,
            }
        );

        app.dialog = None;
        app.apply_filter(custom, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Reduce Noise"));
        app.run(Command::ReduceNoise, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::Filter { filter, .. }) if filter == custom));
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

        // Smart Sharpen on a mask also changes the mask, not the pixels.
        app.run(Command::SmartSharpen, &ctx);
        let Some(Dialog::Filter { filter, .. }) = app.dialog.take() else {
            panic!("no filter dialog");
        };
        assert!(matches!(filter, LayerFilter::SmartSharpen(_)));
        app.apply_filter(filter, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Smart Sharpen"));
        let layer = editor.doc.layer(id).unwrap();
        assert_eq!(layer.pixels.to_vec(), pixels);

        // Reduce Noise on a mask also changes the mask, not the pixels.
        app.run(Command::ReduceNoise, &ctx);
        let Some(Dialog::Filter { filter, .. }) = app.dialog.take() else {
            panic!("no filter dialog");
        };
        assert!(matches!(filter, LayerFilter::ReduceNoise(_)));
        app.apply_filter(filter, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Reduce Noise"));
        let layer = editor.doc.layer(id).unwrap();
        assert_eq!(layer.pixels.to_vec(), pixels);

        // Smart Blur on a mask also changes the mask, not the pixels.
        app.run(Command::SmartBlur, &ctx);
        let Some(Dialog::Filter { filter, .. }) = app.dialog.take() else {
            panic!("no filter dialog");
        };
        assert!(matches!(filter, LayerFilter::SmartBlur(_)));
        app.apply_filter(filter, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Smart Blur"));
        let layer = editor.doc.layer(id).unwrap();
        assert_eq!(layer.pixels.to_vec(), pixels);
    }

    #[test]
    fn hide_all_mask_is_black_or_hides_the_selection() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let mask = |app: &App, x, y| {
            let e = app.editor.as_ref().unwrap();
            e.doc.layer(e.active).unwrap().mask.as_ref().unwrap().pixels.get(x, y)
        };

        // With the selection (100..200), just the selection is hidden.
        app.run(Command::AddMaskHideAll, &ctx);
        assert_eq!(mask(&app, 150, 150), 0);
        assert_eq!(mask(&app, 50, 50), u16::MAX);
        assert_eq!(app.editor.as_ref().unwrap().target, Target::Mask);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Add Layer Mask"));
        assert!(!app.enabled(Command::AddMaskHideAll));

        app.run(Command::DeleteMask, &ctx);
        app.run(Command::Deselect, &ctx);
        app.run(Command::AddMaskHideAll, &ctx);
        assert_eq!(mask(&app, 150, 150), 0);
        assert_eq!(mask(&app, 50, 50), 0);
    }

    #[test]
    fn mask_density_applies_and_undoes() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let id = app.editor.as_ref().unwrap().active;

        // Command is disabled without a mask.
        assert!(!app.enabled(Command::MaskDensity));

        // A black mask, targeted at pixels: Mask Density targets the mask.
        app.run(Command::Deselect, &ctx);
        app.run(Command::AddMaskHideAll, &ctx);
        app.editor.as_mut().unwrap().target = Target::Pixels;
        assert!(app.enabled(Command::MaskDensity));
        let (w, h) = (600, 400);

        app.run(Command::MaskDensity, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().target, Target::Mask);
        let Some(Dialog::Filter { filter, preview }) = &mut app.dialog else {
            panic!("no filter dialog");
        };
        assert!(*preview);
        assert_eq!(*filter, LayerFilter::MaskDensity { density: 100.0 });

        *filter = LayerFilter::MaskDensity { density: 50.0 };
        let filter = *filter;
        app.dialog = None;
        app.apply_filter(filter, &ctx);

        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Mask Density"));

        // 50 % turns black mask into mid grey.
        let layer = editor.doc.layer(id).unwrap();
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!(mask.get(0, 0), 32768);
        assert_eq!(mask.get(w / 2, h / 2), 32768);
        assert_eq!(app.filters.mask_density, 50.0);

        // Undo restores black mask.
        app.run(Command::Undo, &ctx);
        let editor = app.editor.as_ref().unwrap();
        let layer = editor.doc.layer(id).unwrap();
        let mask = &layer.mask.as_ref().unwrap().pixels;
        assert_eq!(mask.get(0, 0), 0);
        assert_eq!(mask.get(w / 2, h / 2), 0);
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
    fn smart_blur_previews_then_applies_and_remembers_its_settings() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let active = app.editor.as_ref().unwrap().active;
        app.run(Command::SmartBlur, &ctx);
        let Some(Dialog::Filter { filter, preview }) = &mut app.dialog else {
            panic!("no filter dialog");
        };
        assert!(*preview);
        assert_eq!(*filter, LayerFilter::SmartBlur(FilterSettings::default().smart_blur));
        let custom = LayerFilter::SmartBlur(SmartBlurOptions {
            radius: 5.0,
            threshold: 30.0,
            quality: SmartBlurQuality::High,
            mode: SmartBlurMode::EdgeOnly,
        });
        *filter = custom;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| app.dialogs(ctx));
        output.textures_delta.clear();
        let view = app.editor.as_ref().unwrap().view();
        assert_eq!(
            view,
            View::Filter {
                layer: active,
                filter: custom,
                mask: false,
            }
        );

        app.dialog = None;
        app.apply_filter(custom, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        assert_eq!(editor.undo_label(), Some("Smart Blur"));
        app.run(Command::SmartBlur, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::Filter { filter, .. }) if filter == custom));
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
                fine: 5.0,
                coarse: None,
                band: SeparationBand::Texture,
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
                let overlay = crate::canvas::Overlay::default();
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
        app.apply_radius(command, radius, None, &ctx);
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
    fn window_menu_toggles_top_strip() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        assert_eq!(app.top_tab, None);

        // Window -> Navigator opens Navigator
        app.run(Command::ShowNavigator, &ctx);
        assert_eq!(app.top_tab, Some(TopTab::Navigator));

        // Window -> Navigator toggles Navigator off
        app.run(Command::ShowNavigator, &ctx);
        assert_eq!(app.top_tab, None);

        // Window -> Histogram opens Histogram
        app.run(Command::ShowHistogram, &ctx);
        assert_eq!(app.top_tab, Some(TopTab::Histogram));

        // Window -> Navigator switches active tab in strip
        app.run(Command::ShowNavigator, &ctx);
        assert_eq!(app.top_tab, Some(TopTab::Navigator));

        // Window -> Histogram switches to Histogram
        app.run(Command::ShowHistogram, &ctx);
        assert_eq!(app.top_tab, Some(TopTab::Histogram));

        // Window -> Histogram toggles Histogram off
        app.run(Command::ShowHistogram, &ctx);
        assert_eq!(app.top_tab, None);
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
        assert!(!app.opening.is_empty());

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
                let overlay = crate::canvas::Overlay::default();
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

    /// A test app with nothing selected and a red 20 px square at (300, 200).
    fn red_square_app() -> App {
        let mut app = test_app();
        app.editor.as_mut().unwrap().edit("Deselect", |doc, _| {
            doc.selection = None;
            for y in 190..210 {
                for x in 290..310 {
                    doc.layers[0].pixels.tile_mut(x / 256, y / 256)[((y % 256) * 256 + x % 256) as usize] = [65535, 0, 0, 65535];
                }
            }
        });
        app
    }

    #[test]
    fn liquify_warps_applies_carries_on_and_cancels() {
        use omapix_engine::warp::Brush;
        let ctx = egui::Context::default();
        let mut app = red_square_app();
        let red = [65535, 0, 0, 65535];
        let key = |app: &mut App, key| {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let mut out = ctx.run_ui(input, |ui| app.check_liquify_keys(ui.ctx()));
            out.textures_delta.clear();
        };
        let none = egui::Modifiers::NONE;
        let pixel = |app: &App, x, y| app.editor.as_ref().unwrap().doc.layers[0].pixels.get(x, y);
        let original = app.editor.as_ref().unwrap().doc.layers[0].pixels.clone();
        assert_ne!(original.get(320, 200), red);

        // Forward Warp drags the square's right edge 15 px right, as one step.
        app.run(Command::Liquify, &ctx);
        assert!(app.editor.as_ref().unwrap().liquifying() && !app.enabled(Command::Liquify));
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(300.0, 200.0)), none);
        app.tool_input(ToolInput::StrokeMove(egui::pos2(315.0, 200.0)), none);
        app.tool_input(ToolInput::StrokeEnd, none);
        assert_eq!(pixel(&app, 320, 200), red);
        key(&mut app, egui::Key::Enter);
        let editor = app.editor.as_ref().unwrap();
        assert!(!editor.liquifying());
        assert_eq!(editor.undo_label(), Some("Liquify"));

        // Liquify again carries on from the same mesh, so Reconstruct has a
        // warp to take out (a new mesh would have none), and the square's
        // edge goes back towards where it was.
        app.run(Command::Liquify, &ctx);
        key(&mut app, egui::Key::R);
        assert_eq!(app.tools.liquify.brush, Brush::Reconstruct);
        app.editor.as_mut().unwrap().liquify(Brush::Reconstruct, &[([300.0, 200.0], [0.0; 2])], 200.0, 1.0);
        assert!(pixel(&app, 320, 200) == original.get(320, 200) && pixel(&app, 305, 200) == red);

        // Esc puts it back as it was when Liquify opened.
        key(&mut app, egui::Key::Escape);
        assert_eq!(pixel(&app, 320, 200), red);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Liquify"));
        key(&mut app, egui::Key::W);
        key(&mut app, egui::Key::A);
        assert_eq!(app.tools.liquify.brush, Brush::Reconstruct, "keys are Liquify's only while it's open");
        assert!(!app.tools.liquify.face_aware);
        // A opens and closes Face-Aware Liquify's panel.
        app.run(Command::Liquify, &ctx);
        key(&mut app, egui::Key::A);
        assert!(app.tools.liquify.face_aware);
        key(&mut app, egui::Key::A);
        assert!(!app.tools.liquify.face_aware);
        // And Y Body Reshape's.
        key(&mut app, egui::Key::Y);
        assert!(app.tools.liquify.body);
        key(&mut app, egui::Key::Y);
        assert!(!app.tools.liquify.body);
    }

    #[test]
    fn liquify_undoes_its_strokes_restores_live_and_applies_as_one_step() {
        use omapix_engine::warp::Brush;
        let ctx = egui::Context::default();
        let mut app = red_square_app();
        let none = egui::Modifiers::NONE;
        let pixels = |app: &App| app.editor.as_ref().unwrap().doc.layers[0].pixels.to_vec();
        let stroke = |app: &mut App, from: Pos2, to: Pos2| {
            app.tool_input(ToolInput::StrokeBegin(from), none);
            app.tool_input(ToolInput::StrokeMove(to), none);
            app.tool_input(ToolInput::StrokeEnd, none);
        };
        let before_label = app.editor.as_ref().unwrap().undo_label().map(str::to_owned);
        let original = pixels(&app);

        // Two strokes, each undone and redone on its own while Liquify is open.
        app.run(Command::Liquify, &ctx);
        stroke(&mut app, egui::pos2(300.0, 200.0), egui::pos2(315.0, 200.0));
        let one = pixels(&app);
        stroke(&mut app, egui::pos2(300.0, 190.0), egui::pos2(300.0, 175.0));
        let two = pixels(&app);
        assert!(one != original && two != one);
        assert!(app.enabled(Command::Undo) && !app.enabled(Command::Redo));
        app.run(Command::Undo, &ctx);
        assert_eq!(pixels(&app), one);
        app.run(Command::Undo, &ctx);
        assert_eq!(pixels(&app), original);
        assert!(!app.enabled(Command::Undo) && app.enabled(Command::Redo));
        app.run(Command::Redo, &ctx);
        app.run(Command::Redo, &ctx);
        assert_eq!(pixels(&app), two);
        assert!(app.editor.as_ref().unwrap().liquifying());

        // Restore All shows live, and is a step of its own once kept.
        let editor = app.editor.as_mut().unwrap();
        editor.set_liquify_restore(1.0);
        assert_eq!(pixels(&app), original);
        let editor = app.editor.as_mut().unwrap();
        editor.set_liquify_restore(0.5);
        let half = pixels(&app);
        assert!(half != original && half != two);
        app.run(Command::Undo, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().liquify_restore(), 0.0);
        assert_eq!(pixels(&app), two);

        // Alt+right-drag resizes the brush.
        app.tool_input(ToolInput::BrushDrag { size: 20.0, hardness: 0.5 }, none);
        assert_eq!(app.tools.liquify.size, 120.0);

        // Applied, it's one step.
        app.editor.as_mut().unwrap().commit_liquify();
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Liquify"));
        app.run(Command::Undo, &ctx);
        assert_eq!(pixels(&app), original);
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), before_label.as_deref());

        // With every stroke undone, applying leaves no step.
        app.run(Command::Liquify, &ctx);
        app.editor.as_mut().unwrap().liquify(Brush::Bloat, &[([300.0, 200.0], [0.0; 2])], 50.0, 1.0);
        app.run(Command::Undo, &ctx);
        app.editor.as_mut().unwrap().commit_liquify();
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), before_label.as_deref());
        assert_eq!(pixels(&app), original);
    }

    #[test]
    fn crop_tool_box_drags_and_crops_on_enter() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let editor = app.editor.as_mut().unwrap();
        editor.canvas.lay_out_for_test(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)), 1.0);
        let key = |app: &mut App, key| {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let mut out = ctx.run_ui(input, |ui| app.check_crop_keys(ui.ctx()));
            out.textures_delta.clear();
        };
        let drag = |app: &mut App, from: (f32, f32), to: (f32, f32), modifiers| {
            app.tool_input(ToolInput::StrokeBegin(egui::pos2(from.0, from.1)), modifiers);
            app.tool_input(ToolInput::StrokeMove(egui::pos2(to.0, to.1)), modifiers);
            app.tool_input(ToolInput::StrokeEnd, modifiers);
        };
        let none = egui::Modifiers::NONE;
        app.tools.select(crate::tools::Tool::Crop);

        // With a selection, the box starts on its bounds, as in Photoshop.
        assert_eq!(app.crop_box(), [100.0, 100.0, 200.0, 200.0]);
        app.run(Command::Deselect, &ctx);
        // Esc drops the box and goes back to the tool before.
        drag(&mut app, (580.0, 380.0), (400.0, 300.0), none);
        key(&mut app, egui::Key::Escape);
        assert_eq!(app.tools.tool, crate::tools::Tool::Brush);
        app.tools.select(crate::tools::Tool::Marquee);
        app.tools.select(crate::tools::Tool::Crop);
        key(&mut app, egui::Key::Escape);
        assert_eq!(app.tools.tool, crate::tools::Tool::Marquee);
        app.tools.select(crate::tools::Tool::Crop);

        // Without, it starts round the whole 600 × 400 image. Its bottom right
        // corner goes freely to (500, 250), then Shift keeps 2:1 from the top left.
        assert_eq!(app.crop_box(), [0.0, 0.0, 600.0, 400.0]);
        drag(&mut app, (600.0, 400.0), (500.0, 250.0), none);
        assert_eq!(app.crop_box(), [0.0, 0.0, 500.0, 250.0]);
        drag(&mut app, (500.0, 250.0), (300.0, 150.0), egui::Modifiers::SHIFT);
        assert_eq!(app.crop_box(), [0.0, 0.0, 300.0, 150.0]);
        // Inside moves it; outside draws a new one.
        drag(&mut app, (100.0, 100.0), (150.0, 120.0), none);
        assert_eq!(app.crop_box(), [50.0, 20.0, 350.0, 170.0]);
        drag(&mut app, (580.0, 380.0), (400.0, 300.0), none);
        assert_eq!(app.crop_box(), [400.0, 300.0, 580.0, 380.0]);
        // Esc and back again: round the whole image.
        key(&mut app, egui::Key::Escape);
        app.tools.select(crate::tools::Tool::Crop);
        assert_eq!(app.crop_box(), [0.0, 0.0, 600.0, 400.0]);

        // Enter crops to it, and the box goes back round the cropped image.
        drag(&mut app, (0.0, 0.0), (100.0, 50.0), none);
        key(&mut app, egui::Key::Enter);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!((editor.doc.width, editor.doc.height, editor.undo_label()), (500, 350, Some("Crop")));
        assert!(editor.doc.selection.is_none());
        assert_eq!(app.crop_box(), [0.0, 0.0, 500.0, 350.0]);
    }

    /// A scene of grey rectangles for photos to be lined up by.
    fn scene(w: usize, h: usize) -> Tiled<omapix_engine::Pixel> {
        let mut px = vec![[30000u16, 30000, 30000, 65535]; w * h];
        let mut seed = 7u64;
        let mut random = |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };
        for _ in 0..w * h / 600 {
            let (x, y, v) = (random(w), random(h), 5000 + random(55000) as u16);
            let (rw, rh) = (8 + random(50), 8 + random(50));
            for j in y..(y + rh).min(h) {
                px[j * w + x..j * w + (x + rw).min(w)].fill([v, v, v, 65535]);
            }
        }
        Tiled::from_slice(w as u32, h as u32, [0; 4], &px)
    }

    #[test]
    fn photomerge_opens_the_photos_chosen_as_one_image_in_a_new_tab() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let dir = std::env::temp_dir().join(format!("omapix-app-photomerge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Two frames of a scene 900 wide, 300 apart.
        let wide = scene(900, 400);
        let paths: Vec<PathBuf> = [("left", 0), ("right", 300)]
            .into_iter()
            .map(|(name, x)| {
                let frame = omapix_engine::Raster::new(600, 400, wide.crop(x, 0, 600, 400));
                let doc = Document::from_image("t.tif".into(), &frame, ColorProfile::srgb(), 16);
                let path = dir.join(format!("{name}.png"));
                export::png(&doc, &path).unwrap();
                path
            })
            .collect();
        let finish = |app: &mut App| {
            while app.merging.is_some() {
                std::thread::sleep(Duration::from_millis(5));
                app.poll(&ctx);
            }
        };

        app.photomerge(paths[..1].to_vec(), &ctx);
        assert_eq!(app.status.take().map(|s| s.0), Some("Photomerge needs two or more photos".into()));
        app.photomerge(paths.clone(), &ctx);
        finish(&mut app);
        assert!(app.status.is_none(), "{:?}", app.status.as_ref().map(|s| &s.0));
        assert_eq!(app.tabs().count(), 2);
        let doc = &app.editor.as_ref().unwrap().doc;
        assert_eq!((doc.width, doc.height, doc.file_name()), (900, 400, "Panorama".to_owned()));
        let names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["left", "right"]);
        assert!(doc.layers.iter().all(|l| l.mask.is_some()));
        assert_eq!(doc.composite().pixels()[200 * 900 + 450][3], 65535);

        // What doesn't line up is said, and nothing opens.
        let other = omapix_engine::Raster::new(600, 400, vec![[30000, 30000, 30000, 65535]; 600 * 400]);
        export::png(&Document::from_image("t.tif".into(), &other, ColorProfile::srgb(), 16), &paths[1]).unwrap();
        app.photomerge(paths, &ctx);
        finish(&mut app);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(app.status.as_ref().map(|s| s.0.as_str()), Some("Photomerge: the photos don't line up with each other"));
        assert_eq!(app.tabs().count(), 2);
    }

    #[test]
    fn auto_align_layers_lines_up_the_selected_layers_and_says_which_it_couldnt() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        // A scene, and the same one from a little to the side, as a second
        // frame of it would be.
        let (w, h) = (600usize, 400usize);
        let scene = scene(w, h);
        let editor = app.editor.as_mut().unwrap();
        let bottom = editor.active;
        let mut top = 0;
        editor.edit("Scene", |doc, _| {
            doc.selection = None;
            doc.layers[0].pixels = scene.clone();
            top = doc.next_layer_id();
            doc.layers.push(Layer::from_pixels(top, "Second frame", scene.translated(20, 10, [0; 4])));
        });
        let finish = |app: &mut App| {
            while app.aligning.is_some() {
                std::thread::sleep(Duration::from_millis(5));
                app.editor.as_mut().unwrap().update(&ctx);
                app.poll(&ctx);
            }
        };

        // One layer has nothing to line up with.
        assert!(!app.enabled(Command::AutoAlignLayers));
        app.editor.as_mut().unwrap().select_layers(top, vec![bottom]);
        assert!(app.enabled(Command::AutoAlignLayers));
        app.run(Command::AutoAlignLayers, &ctx);
        assert!(!app.enabled(Command::AutoAlignLayers), "while it's at it");
        finish(&mut app);
        let editor = app.editor.as_mut().unwrap();
        assert_eq!(editor.undo_label(), Some("Auto-Align Layers"));
        assert!(app.status.is_none());
        let layer = |id| &editor.doc.layer(id).unwrap().pixels;
        assert!(layer(bottom).same_tiles(&scene), "the bottom one stays");
        let (was, now) = (scene.get(300, 200), layer(top).get(300, 200));
        assert!(was[0].abs_diff(now[0]) < 700, "{was:?} {now:?}");
        assert_eq!(layer(top).get(595, 395)[3], 0, "it moved up and left");

        // A layer of something else is left where it is, and named.
        let mut flat = 0;
        editor.edit("New Layer", |doc, _| {
            flat = doc.next_layer_id();
            doc.layers.push(Layer::empty(flat, "Notes", w as u32, h as u32));
        });
        editor.select_layers(top, vec![bottom, flat]);
        app.run(Command::AutoAlignLayers, &ctx);
        finish(&mut app);
        assert_eq!(app.status.as_ref().map(|s| (s.0.as_str(), s.1)), Some(("Nothing lines up with Notes", true)));
    }

    #[test]
    fn free_transform_drags_a_corner_and_applies_on_enter() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let editor = app.editor.as_mut().unwrap();
        editor.canvas.lay_out_for_test(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)), 1.0);
        let key = |app: &mut App, key| {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let mut out = ctx.run_ui(input, |ui| app.check_transform_keys(ui.ctx()));
            out.textures_delta.clear();
        };
        let none = egui::Modifiers::NONE;

        // The selection (100..200) is what's transformed. Dragging its
        // bottom right corner to (300, 300) doubles it.
        app.run(Command::FreeTransform, &ctx);
        assert!(!app.enabled(Command::FreeTransform));
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(201.0, 199.0)), none);
        app.tool_input(ToolInput::StrokeMove(egui::pos2(300.0, 300.0)), none);
        app.tool_input(ToolInput::StrokeEnd, none);
        key(&mut app, egui::Key::Enter);
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.transform().is_none());
        let sel = editor.doc.selection.clone().unwrap();
        let selected = |sel: &Selection| [(105, 105), (295, 295), (95, 150), (305, 150)].map(|(x, y)| sel.at(x, y));
        assert_eq!(selected(&sel), [1.0, 1.0, 0.0, 0.0]);
        assert_eq!(editor.undo_label(), Some("Free Transform"));

        // Ctrl+Z while transforming cancels, leaving the step before.
        app.run(Command::FreeTransform, &ctx);
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(150.0, 150.0)), none);
        app.tool_input(ToolInput::StrokeMove(egui::pos2(170.0, 150.0)), none);
        app.tool_input(ToolInput::StrokeEnd, none);
        app.run(Command::Undo, &ctx);
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.transform().is_none());
        assert!(editor.doc.selection.as_ref().unwrap().coverage.same_tiles(&sel.coverage));
        assert_eq!(editor.undo_label(), Some("Free Transform"));

        // Esc cancels too.
        app.run(Command::FreeTransform, &ctx);
        key(&mut app, egui::Key::Escape);
        assert!(app.editor.as_ref().unwrap().transform().is_none());
    }

    #[test]
    fn object_selection_asks_the_model_about_a_click() {
        let mut app = test_app();
        app.tools.tool = crate::tools::Tool::ObjectSelection;
        assert!(!app.objects.busy());
        let none = egui::Modifiers::NONE;
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(150.0, 150.0)), none);
        app.tool_input(ToolInput::StrokeEnd, none);
        // Waiting for the canvas's image, then the model.
        assert!(app.objects.busy());
        // The selection changes only when the model answers.
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Rectangular Marquee"));
    }

    #[test]
    fn dialogs_go_back_to_the_defaults_and_frequency_separation_remembers() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.defaults.blur_radius = 6.0;
        app.filters.blur_radius = 1.0;
        // A frame of the dialog, clicking at `at` if given; where "Defaults" is.
        let frame = |app: &mut App, at: Option<egui::Pos2>| {
            let events = at.map_or_else(Vec::new, |pos| {
                [true, false]
                    .map(|pressed| egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    })
                    .into_iter()
                    .chain([egui::Event::PointerMoved(pos)])
                    .collect()
            });
            let input = egui::RawInput { events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.dialogs(ui.ctx()));
            out.textures_delta.clear();
            fn find(shape: &egui::Shape) -> Option<egui::Rect> {
                match shape {
                    egui::Shape::Text(t) if t.galley.text() == "Defaults" => Some(t.visual_bounding_rect()),
                    egui::Shape::Vec(v) => v.iter().find_map(find),
                    _ => None,
                }
            }
            out.shapes.iter().find_map(|s| find(&s.shape)).map(|r| r.center())
        };

        app.run(Command::GaussianBlur, &ctx);
        // A modal is laid out, unseen, on its first frame.
        frame(&mut app, None);
        let button = frame(&mut app, None).expect("a Defaults button");
        frame(&mut app, Some(button));
        frame(&mut app, Some(button));
        assert!(matches!(app.dialog, Some(Dialog::Filter { filter: LayerFilter::GaussianBlur { radius }, .. }) if radius == 6.0));

        // Frequency Separation starts from the image's size, then from the
        // radius last used.
        app.dialog = None;
        app.run(Command::FrequencySeparation, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::Radius { radius, .. }) if radius == 0.9));
        app.dialog = None;
        app.apply_radius(Command::FrequencySeparation, 3.5, None, &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }
        app.run(Command::FrequencySeparation, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::Radius { radius, .. }) if radius == 3.5));

        // Modify commands restore their defaults.
        app.defaults.border_width = 25.0;
        app.filters.border_width = 12.0;
        app.dialog = None;
        app.run(Command::BorderSelection, &ctx);
        frame(&mut app, None);
        let button = frame(&mut app, None).expect("a Defaults button");
        frame(&mut app, Some(button));
        frame(&mut app, Some(button));
        assert!(matches!(app.dialog, Some(Dialog::Radius { radius, .. }) if radius == 25.0));
    }

    #[test]
    fn frequency_separation_3_dialog_preview_and_remembers() {
        let ctx = egui::Context::default();
        let mut app = test_app();

        // 1. Run the command: opens dialog with defaults computed from image size (fine=0.5, coarse=2.7).
        app.run(Command::FrequencySeparation3, &ctx);
        assert!(matches!(
            app.dialog,
            Some(Dialog::Radius {
                command: Command::FrequencySeparation3,
                radius,
                coarse: Some(coarse),
                preview: Some(SeparationBand::Texture),
            }) if radius == 0.5 && coarse == 2.7
        ));

        // 2. Check that the preview view is set when dialog runs.
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.dialogs(ui.ctx()));
        out.textures_delta.clear();
        assert_eq!(
            app.editor.as_ref().unwrap().view(),
            View::Separation {
                fine: 0.5,
                coarse: Some(2.7),
                band: SeparationBand::Texture,
            }
        );

        // 3. Apply with custom radii.
        app.dialog = None;
        app.apply_radius(Command::FrequencySeparation3, 1.5, Some(6.0), &ctx);
        let editor = app.editor.as_mut().unwrap();
        while editor.busy().is_some() {
            std::thread::sleep(Duration::from_millis(1));
            editor.update(&ctx);
        }

        // 4. Check layers made and that Mid layer is selected.
        let doc = &editor.doc;
        assert_eq!(doc.layers.len(), 5);
        let group = doc.layers.last().unwrap();
        assert!(group.is_group && group.blend == omapix_engine::blend::BlendMode::PassThrough);
        assert_eq!(group.name, "Frequency Separation (3 Bands)");

        let low = &doc.layers[1];
        let mid = &doc.layers[2];
        let high = &doc.layers[3];
        assert_eq!(low.name, "Low - color/tone");
        assert_eq!(low.blend, omapix_engine::blend::BlendMode::Normal);
        assert_eq!(mid.name, "Mid - blotches");
        assert_eq!(mid.blend, omapix_engine::blend::BlendMode::GrainMerge);
        assert_eq!(high.name, "High - texture");
        assert_eq!(high.blend, omapix_engine::blend::BlendMode::GrainMerge);

        assert_eq!(editor.active, mid.id);

        // 5. Check radii remembered.
        assert_eq!(app.filters.separation3_fine, Some(1.5));
        assert_eq!(app.filters.separation3_coarse, Some(6.0));

        // Running the command again uses remembered radii.
        app.run(Command::FrequencySeparation3, &ctx);
        assert!(matches!(
            app.dialog,
            Some(Dialog::Radius {
                command: Command::FrequencySeparation3,
                radius,
                coarse: Some(coarse),
                ..
            }) if radius == 1.5 && coarse == 6.0
        ));
    }

    #[test]
    fn select_modify_commands_run_through_dialogs() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let commands = [
            (Command::BorderSelection, 10.0, 15.0, "Border Selection"),
            (Command::SmoothSelection, 5.0, 8.0, "Smooth Selection"),
            (Command::ExpandSelection, 5.0, 12.0, "Expand Selection"),
            (Command::ContractSelection, 5.0, 6.0, "Contract Selection"),
            (Command::Feather, 10.0, 4.0, "Feather"),
        ];

        let press_enter = |app: &mut App| {
            let input = egui::RawInput::default();
            let mut out = ctx.run_ui(input, |ui| app.dialogs(ui.ctx()));
            out.textures_delta.clear();
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let mut out = ctx.run_ui(input, |ui| app.dialogs(ui.ctx()));
            out.textures_delta.clear();
        };

        for (cmd, default_radius, custom_radius, undo_label) in commands {
            assert!(app.enabled(cmd));
            app.run(cmd, &ctx);
            assert!(
                matches!(app.dialog, Some(Dialog::Radius { command, radius, .. }) if command == cmd && radius == default_radius),
                "expected dialog for {cmd:?} with radius {default_radius}"
            );

            if let Some(Dialog::Radius { radius, .. }) = &mut app.dialog {
                *radius = custom_radius;
            }

            press_enter(&mut app);
            assert!(app.dialog.is_none());

            let editor = app.editor.as_mut().unwrap();
            while editor.busy().is_some() {
                std::thread::sleep(Duration::from_millis(1));
                editor.update(&ctx);
            }

            assert_eq!(editor.undo_label(), Some(undo_label));

            match cmd {
                Command::BorderSelection => assert_eq!(app.filters.border_width, custom_radius),
                Command::SmoothSelection => assert_eq!(app.filters.smooth_radius, custom_radius),
                Command::ExpandSelection => assert_eq!(app.filters.expand_radius, custom_radius),
                Command::ContractSelection => assert_eq!(app.filters.contract_radius, custom_radius),
                Command::Feather => assert_eq!(app.filters.feather_radius, custom_radius),
                _ => {}
            }
        }
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

    #[test]
    fn ai_models_says_where_each_model_is_or_how_to_get_it() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let models = omapix_ai::models::models();
        let sam = models.iter().position(|m| m.id == omapix_ai::sam::MODEL).unwrap();
        let mut found = vec![None; models.len()];
        found[sam] = Some(HashMap::from([("encoder.onnx", PathBuf::from("/models/sam/encoder.onnx"))]));
        app.dialog = Some(Dialog::AiModels(found));
        let frame = |app: &mut App, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 1000.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| app.dialogs(ui.ctx()));
            out.textures_delta.clear();
            let mut texts = Vec::new();
            fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
                    _ => {}
                }
            }
            out.shapes.iter().for_each(|s| collect(&s.shape, &mut texts));
            texts
        };
        // A modal's laid out on its first frame and drawn on the next.
        frame(&mut app, vec![]);
        let texts = frame(&mut app, vec![]);
        for model in models {
            assert!(texts.iter().any(|t| t == model.name), "{} in {texts:?}", model.name);
        }
        assert!(texts.iter().any(|t| t == "/models/sam"), "{texts:?}");
        // The rest aren't installed: from the script, or from darktable.
        let count = |start: &str| texts.iter().filter(|t| t.starts_with(start)).count();
        let darktables = models.iter().filter(|m| m.files.iter().all(|f| f.url.is_none())).count();
        assert_eq!(count("Not installed: run scripts/fetch-models.sh"), models.len() - darktables, "{texts:?}");
        assert_eq!(count("Not installed: install it from darktable"), darktables - 1, "{texts:?}");

        frame(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(app.dialog.is_none());
    }

    #[test]
    fn ai_commands_are_greyed_out_without_their_model() {
        let mut app = test_app();
        assert!(app.editor.as_ref().unwrap().doc.selection.is_some());
        // Not checked yet: nothing's greyed out.
        assert!(app.enabled(Command::SelectSubject) && app.enabled(Command::ContentAwareFill));
        app.models = HashMap::from([(omapix_ai::subject::MODEL, false), (omapix_ai::lama::MODEL, true)]);
        assert!(!app.enabled(Command::SelectSubject));
        assert!(app.enabled(Command::ContentAwareFill));
        app.models.insert(omapix_ai::lama::MODEL, false);
        assert!(!app.enabled(Command::ContentAwareFill));
        assert!(app.enabled(Command::GenerativeFill));
        // It needs no selection: without one it fills empty canvas.
        let selection = app.editor.as_mut().unwrap().doc.selection.take();
        assert!(app.enabled(Command::GenerativeFill) && !app.enabled(Command::ContentAwareFill));
        app.editor.as_mut().unwrap().doc.selection = selection;
        app.models.insert(omapix_ai::flux::MODEL, false);
        assert!(!app.enabled(Command::GenerativeFill));
        assert_eq!(missing_models(Command::GenerativeFill, &app.models), ["FLUX.2 klein 4B (int4)"]);
        // Skin and Hair need all three face models.
        assert!(app.enabled(Command::SelectSkin) && app.enabled(Command::SelectHair));
        app.models.insert(omapix_ai::face::LANDMARKER, false);
        assert!(!app.enabled(Command::SelectSkin) && !app.enabled(Command::SelectHair));
        assert_eq!(missing_models(Command::SelectHair, &app.models), ["MediaPipe Face Landmarker"]);
    }

    #[test]
    fn escape_deselects_but_not_in_quick_mask() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let escape = |app: &mut App| {
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
                app.check_transform_keys(ui.ctx());
                app.check_crop_keys(ui.ctx());
                app.check_deselect(ui.ctx());
            });
            out.textures_delta.clear();
        };
        assert!(app.editor.as_ref().unwrap().doc.selection.is_some());
        escape(&mut app);
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.doc.selection.is_none());
        assert_eq!(editor.undo_label(), Some("Deselect"));

        // Quick Mask is the selection being painted: Esc leaves it be.
        app.run(Command::SelectAll, &ctx);
        app.run(Command::QuickMask, &ctx);
        escape(&mut app);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.view(), View::QuickMask);
        assert!(editor.doc.selection.is_some());
    }

    #[test]
    fn right_clicking_the_image_offers_selection_commands_or_layer_ones() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let mut time = 0.0;
        let mut frame = |app: &mut App, events: Vec<egui::Event>| {
            time += 0.1;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let response = ui.allocate_rect(ui.max_rect(), egui::Sense::click_and_drag());
                response.context_menu(|ui| app.canvas_menu(ui));
            });
            out.textures_delta.clear();
            let mut texts = Vec::new();
            fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
                    _ => {}
                }
            }
            out.shapes.iter().for_each(|s| collect(&s.shape, &mut texts));
            texts
        };
        let right_click = |pressed| egui::Event::PointerButton {
            pos: egui::pos2(400.0, 300.0),
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut app, vec![egui::Event::PointerMoved(egui::pos2(400.0, 300.0))]);
        frame(&mut app, vec![right_click(true)]);
        frame(&mut app, vec![right_click(false)]);
        let texts = frame(&mut app, vec![]);
        for item in ["Content-Aware Fill", "Select Inverse", "Deselect", "Free Transform"] {
            assert!(texts.iter().any(|t| t == item), "{item} in {texts:?}");
        }

        // With nothing selected: the layer's commands.
        app.editor.as_mut().unwrap().doc.selection = None;
        let texts = frame(&mut app, vec![]);
        assert!(texts.iter().any(|t| t == "Select All"), "{texts:?}");
        assert!(!texts.iter().any(|t| t == "Content-Aware Fill"), "{texts:?}");
    }

    #[test]
    fn select_and_mask_previews_in_red_then_applies_cancels_or_makes_a_mask() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        // Frames until the preview for the current settings is on screen.
        let run = |app: &mut App, events: Vec<egui::Event>| {
            let input = egui::RawInput { events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.dialogs(ui.ctx()));
            out.textures_delta.clear();
        };
        let open = |app: &mut App, output| {
            app.filters.select_and_mask.feather = 5.0;
            app.filters.select_and_mask_output = output;
            app.run(Command::SelectAndMask, &ctx);
            for _ in 0..500 {
                run(app, vec![]);
                if let Some(Dialog::SelectAndMask(d)) = &app.dialog
                    && d.ready().is_some()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("no preview");
        };
        // Cancelled, nothing changes.
        open(&mut app, crate::select_and_mask::Output::Selection);
        assert_eq!(app.editor.as_ref().unwrap().view(), View::QuickMask);
        run(&mut app, vec![key(egui::Key::Escape)]);
        let editor = app.editor.as_ref().unwrap();
        assert!(app.dialog.is_none());
        assert_eq!(editor.view(), View::Image);
        let selection = editor.doc.selection.as_ref().unwrap();
        assert_eq!((selection.at(99, 150), selection.at(150, 150)), (0.0, 1.0));
        assert_eq!(editor.undo_label(), Some("Rectangular Marquee"));

        // OK'd, the selection's feathered, as one step.
        open(&mut app, crate::select_and_mask::Output::Selection);
        run(&mut app, vec![key(egui::Key::Enter)]);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.view(), View::Image);
        assert_eq!(editor.undo_label(), Some("Select and Mask"));
        let edge = editor.doc.selection.as_ref().unwrap().at(99, 150);
        assert!(edge > 0.1 && edge < 0.9, "{edge}");

        // To a layer mask: the layer gets it and the selection goes.
        open(&mut app, crate::select_and_mask::Output::LayerMask);
        run(&mut app, vec![key(egui::Key::Enter)]);
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.doc.selection.is_none());
        let mask = editor.doc.layer(editor.active).unwrap().mask.as_ref().unwrap();
        assert!(mask.pixels.get(150, 150) > 60000 && mask.pixels.get(50, 50) == 0);
    }

    #[test]
    fn ctrl_click_with_the_move_tool_picks_the_layer_under_the_pointer() {
        let mut app = test_app();
        app.tools.tool = crate::tools::Tool::Move;
        let editor = app.editor.as_mut().unwrap();
        let background = editor.active;
        let id = editor.doc.next_layer_id();
        let mut patch = Layer::empty(id, "Patch", 600, 400);
        patch.pixels.tile_mut(0, 0)[10 * 256 + 10] = [1, 2, 3, 65535];
        editor.doc.layers.push(patch);
        let click = |app: &mut App, x: f32, modifiers| {
            app.tool_input(ToolInput::StrokeBegin(egui::pos2(x, 10.0)), modifiers);
            app.tool_input(ToolInput::StrokeEnd, modifiers);
            app.editor.as_ref().unwrap().active
        };
        // A plain click doesn't pick; Ctrl+click does, the top layer where
        // it has pixels and the one below where it hasn't.
        assert_eq!(click(&mut app, 10.0, egui::Modifiers::NONE), background);
        assert_eq!(click(&mut app, 10.0, egui::Modifiers::COMMAND), id);
        assert_eq!(click(&mut app, 50.0, egui::Modifiers::COMMAND), background);
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

        fn open_select_menu(&mut self) -> egui::FullOutput {
            let out = self.frame(vec![]);
            let pos = self
                .find_text_pos(&out, "Select")
                .expect("Select menu button not found");
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
    fn quick_mask_toggle_status_bar_select_menu_and_actions() {
        let mut h = SelectionEdgesHarness::new();
        let ctx = egui::Context::default();

        // Initially in normal image view, no Quick Mask message
        assert_eq!(h.app.editor.as_ref().unwrap().view(), View::Image);
        let out = h.frame(vec![]);
        assert!(!h.output_contains_text(&out, "Quick Mask — press Q to exit"));

        // Select menu initially does not have checkmark on Quick Mask
        let out = h.open_select_menu();
        assert!(h.output_contains_text(&out, "Edit in Quick Mask Mode"));
        assert!(!h.output_contains_text(&out, "✓ Edit in Quick Mask Mode"));
        h.close_menu();

        // Enter Quick Mask via command
        h.app.run(Command::QuickMask, &ctx);
        assert_eq!(h.app.editor.as_ref().unwrap().view(), View::QuickMask);
        assert_eq!(h.app.editor.as_ref().unwrap().target, Target::QuickMask);

        // Status bar displays Quick Mask message
        let out = h.frame(vec![]);
        assert!(h.output_contains_text(&out, "Quick Mask — press Q to exit"));

        // Select menu shows checkmark
        let out = h.open_select_menu();
        assert!(h.output_contains_text(&out, "✓ Edit in Quick Mask Mode"));
        h.close_menu();

        // Pressing Escape does NOT exit Quick Mask
        h.frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(h.app.editor.as_ref().unwrap().view(), View::QuickMask);

        // Invert in Quick Mask inverts the selection
        let orig_cov = h.app.editor.as_ref().unwrap().doc.selection.as_ref().unwrap().coverage.get(20, 20);
        h.app.run(Command::Invert, &ctx);
        let new_cov = h.app.editor.as_ref().unwrap().doc.selection.as_ref().unwrap().coverage.get(20, 20);
        assert_eq!(new_cov, u16::MAX - orig_cov);

        // Copy in Quick Mask returns a mask clip
        let clip = copy(h.app.editor.as_ref().unwrap(), false);
        assert!(clip.is_some());

        // Exit Quick Mask via command
        h.app.run(Command::QuickMask, &ctx);
        assert_eq!(h.app.editor.as_ref().unwrap().view(), View::Image);
        assert_eq!(h.app.editor.as_ref().unwrap().target, Target::Pixels);

        // Status bar no longer has Quick Mask message
        let out = h.frame(vec![]);
        assert!(!h.output_contains_text(&out, "Quick Mask — press Q to exit"));
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

    #[test]
    fn saving_a_round_trip_also_writes_the_tiff_for_darktable() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let dir = std::env::temp_dir().join(format!("omapix-app-round-trip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ora, tiff) = (dir.join("IMG.ora"), dir.join("IMG.tif"));
        let editor = app.editor.as_mut().unwrap();
        editor.doc.saved_path = Some(ora.clone());
        editor.doc.round_trip = Some(tiff.clone());

        app.run(Command::Save, &ctx);
        while app.file_job.is_some() {
            std::thread::sleep(Duration::from_millis(5));
            app.poll(&ctx);
        }
        assert!(ora.exists() && tiff.exists());
        let flat = omapix_engine::io::load(&tiff).unwrap();
        assert_eq!((flat.width, flat.height, flat.layers.len()), (600, 400, 1));
        assert_eq!(app.status.as_ref().unwrap().0, "Saved IMG.ora, and IMG.tif for darktable");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exporting_a_png_keeps_the_document_unsaved_and_says_so() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        assert!(app.enabled(Command::ExportPng));
        let dir = std::env::temp_dir().join(format!("omapix-app-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.png");

        app.write(Purpose::ExportPng, path.clone(), &ctx);
        while app.file_job.is_some() {
            std::thread::sleep(Duration::from_millis(5));
            app.poll(&ctx);
        }
        let flat = omapix_engine::io::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((flat.width, flat.height, flat.layers.len()), (600, 400, 1));
        assert_eq!(app.status.as_ref().unwrap().0, "Exported out.png");
        // An export isn't a save.
        assert!(app.modified());
        assert!(app.editor.as_ref().unwrap().doc.saved_path.is_none());
    }

    #[test]
    fn close_without_unsaved_changes_closes_editor() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.editor.as_mut().unwrap().modified = false;
        assert!(app.editor.is_some());
        assert!(app.enabled(Command::Close));
        assert!(!app.modified());

        app.run(Command::Close, &ctx);

        assert!(app.editor.is_none());
        assert!(!app.enabled(Command::Close));
        assert!(app.dialog.is_none());
    }

    #[test]
    fn saving_viewing_and_deleting_alpha_channels() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.run(Command::ViewRed, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().view(), View::Channel(Channel::Red));
        assert!(!app.enabled(Command::DeleteChannel), "only an alpha channel can be deleted");
        app.run(Command::ViewComposite, &ctx);

        app.run(Command::SaveSelection, &ctx);
        let editor = app.editor.as_mut().unwrap();
        assert_eq!(editor.undo_label(), Some("Save Selection"));
        let channel = &editor.doc.channels[0];
        assert_eq!((channel.pixels.get(150, 150), channel.pixels.get(300, 150)), (65535, 0));
        let id = channel.id;
        editor.set_view(View::Alpha(id));

        app.run(Command::DeleteChannel, &ctx);
        let editor = app.editor.as_mut().unwrap();
        assert!(editor.doc.channels.is_empty());
        assert_eq!(editor.view(), View::Image);
        editor.undo();
        assert_eq!(editor.doc.channels[0].id, id);

        app.run(Command::Deselect, &ctx);
        assert!(!app.enabled(Command::SaveSelection), "nothing to save");
    }

    #[test]
    fn close_with_unsaved_changes_shows_dialog_and_proceeds() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.editor.as_mut().unwrap().modified = true;
        assert!(app.modified());

        app.run(Command::Close, &ctx);

        assert!(matches!(app.dialog, Some(Dialog::UnsavedChanges { then: Then::Close })));
        assert!(app.editor.is_some());

        app.proceed(Then::Close, &ctx);
        assert!(app.editor.is_none());
        assert!(!app.enabled(Command::Close));
    }

    #[test]
    fn new_makes_a_blank_image_in_a_tab_of_its_own() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.editor.as_mut().unwrap().modified = true;

        app.run(Command::New, &ctx);
        // It starts at the open image's size.
        let Some(Dialog::NewImage { width, height }) = &mut app.dialog else {
            panic!("no New dialog");
        };
        assert_eq!((*width, *height), (600, 400));
        (*width, *height) = (320, 200);
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
        let mut output = ctx.run_ui(enter, |ctx| app.dialogs(ctx));
        output.textures_delta.clear();

        assert!(app.dialog.is_none());
        let editor = app.editor.as_ref().unwrap();
        assert_eq!((editor.doc.width, editor.doc.height), (320, 200));
        assert_eq!(editor.doc.file_name(), "Untitled");
        assert_eq!(editor.doc.composite().get(319, 199), [u16::MAX; 4]);
        assert_eq!(editor.history_labels(), ["New"]);
        assert!(!app.modified());
        // The image that was open is in the tab before, as it was.
        assert_eq!(tab_names(&app), ["t.tif •", "Untitled"]);
        assert_eq!(app.tab, 1);
    }

    fn tab_names(app: &App) -> Vec<String> {
        app.tabs().map(tab_name).collect()
    }

    /// An app with three images open, "a.tif", "b.tif" and "c.tif" (300,
    /// 400 and 500 px wide), the first showing.
    fn app_with_three_tabs() -> App {
        let mut app = test_app();
        app.editor = None;
        for (name, w) in [("a.tif", 300), ("b.tif", 400), ("c.tif", 500)] {
            let image = omapix_engine::Raster::new(w, 100, vec![[30000, 30000, 30000, 65535]; (w * 100) as usize]);
            let doc = Document::from_image(name.into(), &image, omapix_engine::ColorProfile::srgb(), 16);
            app.add_tab(Editor::new(doc).unwrap());
        }
        assert!(app.show_tab(0));
        app
    }

    fn width(app: &App) -> u32 {
        app.editor.as_ref().unwrap().doc.width
    }

    #[test]
    fn tabs_switch_between_the_open_images_and_keep_their_panels() {
        let ctx = egui::Context::default();
        let mut app = app_with_three_tabs();
        assert_eq!(tab_names(&app), ["a.tif", "b.tif", "c.tif"]);
        assert_eq!((app.tab, width(&app)), (0, 300));
        // Each image has its own revisions, so nothing kept for one's is
        // taken for another's.
        let revisions: Vec<u64> = app.tabs().map(Editor::revision).collect();
        assert!(!app.tabs().nth(1).unwrap().owns(revisions[0]));

        // An edit and a panel's state stay with their image.
        app.run(Command::NewLayer, &ctx);
        app.properties.eyedropper = Some(Eyedropper::White);
        app.crop = Some([0.0, 0.0, 10.0, 10.0]);
        app.run(Command::NextImage, &ctx);
        assert_eq!((app.tab, width(&app)), (1, 400));
        assert_eq!(app.editor.as_ref().unwrap().doc.layers.len(), 1);
        assert!(app.properties.eyedropper.is_none() && app.crop.is_none());
        assert_eq!(tab_names(&app), ["a.tif •", "b.tif", "c.tif"]);

        // Ctrl+Tab and Ctrl+Shift+Tab go round.
        app.run(Command::NextImage, &ctx);
        app.run(Command::NextImage, &ctx);
        assert_eq!((app.tab, width(&app)), (0, 300));
        assert_eq!(app.editor.as_ref().unwrap().doc.layers.len(), 2);
        assert_eq!(app.properties.eyedropper, Some(Eyedropper::White));
        app.run(Command::PreviousImage, &ctx);
        assert_eq!((app.tab, width(&app)), (2, 500));
        assert_eq!(tab_names(&app), ["a.tif •", "b.tif", "c.tif"]);

        // A copy from one image pastes into another, which is busy and
        // can't be left until it's in.
        app.run(Command::SelectAll, &ctx);
        app.run(Command::Copy, &ctx);
        app.run(Command::PreviousImage, &ctx);
        app.run(Command::Paste, &ctx);
        while app.editor.as_ref().unwrap().busy().is_some() {
            assert!(!app.show_tab(0));
            std::thread::sleep(Duration::from_millis(1));
            app.editor.as_mut().unwrap().update(&ctx);
        }
        let doc = &app.editor.as_ref().unwrap().doc;
        assert_eq!((app.tab, doc.width, doc.layers.len()), (1, 400, 2));
        assert_eq!(doc.layers[1].pixels.get(200, 50), [30000, 30000, 30000, 65535]);

        // With one image there's nowhere to go.
        app.parked.clear();
        app.tab = 0;
        assert!(!app.enabled(Command::NextImage) && !app.enabled(Command::PreviousImage));
    }

    #[test]
    fn a_busy_image_is_not_left_and_new_images_wait_behind_it() {
        let ctx = egui::Context::default();
        let mut app = app_with_three_tabs();
        app.dialog = Some(Dialog::NewImage { width: 10, height: 10 });
        assert!(!app.show_tab(1));
        assert_eq!((app.tab, width(&app)), (0, 300));
        assert!(app.status.as_ref().is_some_and(|(_, error, _)| *error));
        assert!(!app.enabled(Command::Close));

        app.new_image(Document::blank(20, 20));
        assert_eq!((app.tab, width(&app)), (0, 300));
        assert_eq!(tab_names(&app), ["a.tif", "b.tif", "c.tif", "Untitled"]);

        app.dialog = None;
        app.run(Command::PreviousImage, &ctx);
        assert_eq!((app.tab, width(&app)), (3, 20));
    }

    #[test]
    fn closing_a_tab_shows_the_next_and_asks_about_unsaved_changes() {
        let ctx = egui::Context::default();
        let mut app = app_with_three_tabs();
        // The image showing: the one after takes its place.
        app.run(Command::Close, &ctx);
        assert_eq!(tab_names(&app), ["b.tif", "c.tif"]);
        assert_eq!((app.tab, width(&app)), (0, 400));

        // A tab behind closes without being shown.
        app.close_tab(1, &ctx);
        assert_eq!(tab_names(&app), ["b.tif"]);
        assert_eq!((app.tab, width(&app)), (0, 400));

        // One with unsaved changes is shown and asked about.
        let mut app = app_with_three_tabs();
        app.show_tab(2);
        app.run(Command::NewLayer, &ctx);
        app.show_tab(1);
        app.close_tab(0, &ctx);
        assert_eq!(tab_names(&app), ["b.tif", "c.tif •"]);
        assert_eq!(app.tab, 0);
        app.close_tab(1, &ctx);
        assert_eq!((app.tab, width(&app)), (1, 500));
        assert!(matches!(app.dialog.take(), Some(Dialog::UnsavedChanges { then: Then::Close })));
        // Closing the last tab shows the one before.
        app.proceed(Then::Close, &ctx);
        assert_eq!(tab_names(&app), ["b.tif"]);
        assert_eq!((app.tab, width(&app)), (0, 400));
        app.run(Command::Close, &ctx);
        assert!(app.editor.is_none() && app.parked.is_empty());
    }

    #[test]
    fn quitting_asks_about_each_image_with_unsaved_changes() {
        let ctx = egui::Context::default();
        let mut app = app_with_three_tabs();
        for index in [1, 2] {
            app.show_tab(index);
            app.run(Command::NewLayer, &ctx);
        }
        app.show_tab(0);

        app.run(Command::Quit, &ctx);
        assert_eq!(app.tab, 1);
        assert!(matches!(app.dialog.take(), Some(Dialog::UnsavedChanges { then: Then::Quit })));
        assert!(!app.allow_close);
        // Don't Save: on to the next.
        app.editor.as_mut().unwrap().modified = false;
        app.guard(Then::Quit, &ctx);
        assert_eq!(app.tab, 2);
        assert!(matches!(app.dialog.take(), Some(Dialog::UnsavedChanges { then: Then::Quit })));
        assert!(!app.allow_close);
        app.editor.as_mut().unwrap().modified = false;
        app.guard(Then::Quit, &ctx);
        assert!(app.dialog.is_none() && app.allow_close);
    }

    #[test]
    fn the_tab_bar_shows_and_closes_images_and_a_save_finds_its_own() {
        let ctx = egui::Context::default();
        let mut app = app_with_three_tabs();
        let mut time = 0.0;
        // A frame of the tab bar, and where each piece of text was drawn.
        let mut frame = |app: &mut App, events: Vec<egui::Event>| {
            time += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 40.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| app.tab_bar(ui));
            output.textures_delta.clear();
            fn texts(shape: &egui::Shape, found: &mut Vec<(String, Pos2)>) {
                match shape {
                    egui::Shape::Text(t) => found.push((t.galley.text().to_owned(), t.visual_bounding_rect().center())),
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| texts(s, found)),
                    _ => {}
                }
            }
            let mut found = Vec::new();
            output.shapes.iter().for_each(|clipped| texts(&clipped.shape, &mut found));
            found
        };
        let click = |pos: Pos2, button: egui::PointerButton| {
            let press = |pressed| egui::Event::PointerButton { pos, button, pressed, modifiers: Default::default() };
            [vec![egui::Event::PointerMoved(pos)], vec![press(true)], vec![press(false)]]
        };
        let find = |drawn: &[(String, Pos2)], text: &str, nth: usize| {
            drawn.iter().filter(|(t, _)| t == text).nth(nth).unwrap_or_else(|| panic!("no {text} in {drawn:?}")).1
        };
        let drawn = frame(&mut app, vec![]);

        // A save of the first image is under way when the third is shown.
        let revision = app.editor.as_ref().unwrap().revision();
        for events in click(find(&drawn, "c.tif", 0), egui::PointerButton::Primary) {
            frame(&mut app, events);
        }
        assert_eq!((app.tab, width(&app)), (2, 500));
        let (tx, rx) = channel();
        tx.send(Ok((revision, PathBuf::from("/tmp/a.ora"), true))).unwrap();
        app.file_job = Some(FileJob { label: "Saving".into(), rx });
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.poll(ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(tab_names(&app), ["a.ora", "b.tif", "c.tif"]);

        // The second tab's ✕, then a middle click on the first.
        let drawn = frame(&mut app, vec![]);
        for events in click(find(&drawn, "✕", 1), egui::PointerButton::Primary) {
            frame(&mut app, events);
        }
        assert_eq!(tab_names(&app), ["a.ora", "c.tif"]);
        assert_eq!((app.tab, width(&app)), (1, 500));
        let drawn = frame(&mut app, vec![]);
        for events in click(find(&drawn, "a.ora", 0), egui::PointerButton::Middle) {
            frame(&mut app, events);
        }
        assert_eq!(tab_names(&app), ["c.tif"]);
        assert_eq!((app.tab, width(&app)), (0, 500));
    }

    #[test]
    fn new_from_clipboard_makes_an_image_of_the_copy() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let wait = |app: &mut App| {
            let started = Instant::now();
            while app.from_clipboard.is_some() {
                std::thread::sleep(Duration::from_millis(1));
                let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| app.poll(ctx));
                out.textures_delta.clear();
                assert!(started.elapsed() < Duration::from_secs(5), "timed out reading the clipboard");
            }
        };
        // The selection, 100 px square.
        let clip = copy(app.editor.as_ref().unwrap(), false).unwrap();
        app.clipboard.set(clip);

        // With nothing open, Paste does the same.
        for cmd in [Command::NewFromClipboard, Command::Paste] {
            app.editor = None;
            assert!(app.enabled(cmd));
            app.run(cmd, &ctx);
            wait(&mut app);
            let doc = &app.editor.as_ref().unwrap().doc;
            assert_eq!((doc.width, doc.height, doc.layers.len()), (100, 100, 1));
            assert_eq!(doc.composite().get(50, 50), [30000, 30000, 30000, 65535]);
            assert_eq!(doc.file_name(), "Untitled");
        }

        // With an image open, it goes in a new tab beside it.
        app.editor.as_mut().unwrap().modified = true;
        app.run(Command::NewFromClipboard, &ctx);
        wait(&mut app);
        assert!(app.dialog.is_none());
        assert_eq!(tab_names(&app), ["Untitled •", "Untitled"]);
        assert_eq!(app.tab, 1);
    }

    fn write_test_png(path: &Path, w: u32, h: u32) {
        let clip = Clip {
            bounds: [0, 0, w, h],
            pixels: Tiled::new(w, h, [20000, 30000, 40000, 65535]),
            profile: omapix_engine::ColorProfile::srgb(),
            in_place: false,
        };
        let png = clip.to_png().expect("encode png");
        std::fs::write(path, png).expect("write png");
    }

    #[test]
    fn dropped_image_places_as_new_layer_centred_with_undo() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let dir = std::env::temp_dir().join(format!("omapix_test_place_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dropped_layer.png");
        write_test_png(&path, 16, 16);

        let initial_layers = app.editor.as_ref().unwrap().doc.layers.len();
        let doc_w = app.editor.as_ref().unwrap().doc.width;
        let doc_h = app.editor.as_ref().unwrap().doc.height;

        let raw = egui::RawInput {
            dropped_files: vec![Arc::new(crate::drop::DroppedPath(path.clone()))],
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ctx| app.poll(ctx));
        output.textures_delta.clear();

        let started = Instant::now();
        while app.pasting.is_some() || app.editor.as_ref().is_some_and(|e| e.busy().is_some()) {
            std::thread::sleep(Duration::from_millis(10));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| app.poll(ctx));
            out.textures_delta.clear();
            if let Some(editor) = &mut app.editor {
                editor.update(&ctx);
            }
            if started.elapsed() > Duration::from_secs(5) {
                panic!("timed out waiting for place to complete");
            }
        }

        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.doc.layers.len(), initial_layers + 1);
        let new_layer = editor.doc.layers.last().unwrap();
        assert_eq!(new_layer.name, "dropped_layer");
        assert_eq!(editor.undo_label(), Some("Place"));

        // Centred: (doc_w - 16) / 2, (doc_h - 16) / 2
        let cx = (doc_w - 16) / 2;
        let cy = (doc_h - 16) / 2;
        assert_eq!(new_layer.pixels.get(cx, cy)[3], 65535);
        assert_eq!(new_layer.pixels.get(0, 0)[3], 0);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn dropped_image_with_no_document_opens() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.editor = None;
        assert!(app.editor.is_none());

        let dir = std::env::temp_dir().join(format!("omapix_test_open_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("open_test.png");
        write_test_png(&path, 32, 24);

        let raw = egui::RawInput {
            dropped_files: vec![Arc::new(crate::drop::DroppedPath(path.clone()))],
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ctx| app.poll(ctx));
        output.textures_delta.clear();

        let started = Instant::now();
        while !app.opening.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| app.poll(ctx));
            out.textures_delta.clear();
            if started.elapsed() > Duration::from_secs(5) {
                panic!("timed out waiting for open to complete");
            }
        }

        assert!(app.editor.is_some());
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.doc.width, 32);
        assert_eq!(editor.doc.height, 24);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn dropped_image_with_shift_held_opens_instead_of_placing() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.editor.as_mut().unwrap().modified = false;

        let dir = std::env::temp_dir().join(format!("omapix_test_shift_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shift_open.png");
        write_test_png(&path, 40, 30);

        let modifiers = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let raw = egui::RawInput {
            dropped_files: vec![Arc::new(crate::drop::DroppedPath(path.clone()))],
            events: vec![egui::Event::ModifiersChanged(modifiers)],
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ctx| app.poll(ctx));
        output.textures_delta.clear();

        let started = Instant::now();
        while !app.opening.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| app.poll(ctx));
            out.textures_delta.clear();
            if started.elapsed() > Duration::from_secs(5) {
                panic!("timed out waiting for open");
            }
        }

        assert!(app.editor.is_some());
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.doc.width, 40);
        assert_eq!(editor.doc.height, 30);
        // In a new tab, and dropping it again shows that tab.
        assert_eq!(tab_names(&app), ["t.tif", "shift_open.png"]);
        app.show_tab(0);
        app.open(path.clone(), &ctx);
        assert!(app.opening.is_empty());
        assert_eq!(app.tab, 1);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn several_dropped_images_place_each_layer() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let dir = std::env::temp_dir().join(format!("omapix_test_multi_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path1 = dir.join("layer_a.png");
        let path2 = dir.join("layer_b.png");
        write_test_png(&path1, 16, 16);
        write_test_png(&path2, 20, 20);

        let initial_layers = app.editor.as_ref().unwrap().doc.layers.len();

        let raw = egui::RawInput {
            dropped_files: vec![Arc::new(crate::drop::DroppedPath(path1.clone())), Arc::new(crate::drop::DroppedPath(path2.clone()))],
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ctx| app.poll(ctx));
        output.textures_delta.clear();

        let started = Instant::now();
        while app.pasting.is_some() || app.editor.as_ref().is_some_and(|e| e.busy().is_some()) {
            std::thread::sleep(Duration::from_millis(10));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| app.poll(ctx));
            out.textures_delta.clear();
            if let Some(editor) = &mut app.editor {
                editor.update(&ctx);
            }
            if started.elapsed() > Duration::from_secs(5) {
                panic!("timed out waiting for multi-place to complete");
            }
        }

        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.doc.layers.len(), initial_layers + 2);
        assert_eq!(editor.doc.layers[initial_layers].name, "layer_a");
        assert_eq!(editor.doc.layers[initial_layers + 1].name, "layer_b");

        let _ = std::fs::remove_file(&path1);
        let _ = std::fs::remove_file(&path2);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn clone_stamp_current_and_below_copies_below_not_above() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let red: omapix_engine::Pixel = [60000, 0, 0, 65535];
        let blue: omapix_engine::Pixel = [0, 0, 60000, 65535];

        // Deselect so painting isn't restricted by initial selection
        app.run(Command::Deselect, &ctx);

        // Fill background layer with red
        let bg_id = app.editor.as_ref().unwrap().active;
        app.editor.as_mut().unwrap().edit("Fill Red", |doc, _| {
            let (w, h) = (doc.width, doc.height);
            let bg = doc.layer_mut(bg_id).unwrap();
            bg.pixels = Tiled::from_raster(&omapix_engine::Raster::new(
                w,
                h,
                vec![red; (w * h) as usize],
            ));
        });

        // Add empty layer (active layer to clone onto)
        app.run(Command::NewLayer, &ctx);
        let active_id = app.editor.as_ref().unwrap().active;

        // Add top layer above active layer and fill with blue
        app.run(Command::NewLayer, &ctx);
        let top_id = app.editor.as_ref().unwrap().active;
        app.editor.as_mut().unwrap().edit("Fill Blue", |doc, _| {
            let (w, h) = (doc.width, doc.height);
            let top = doc.layer_mut(top_id).unwrap();
            top.pixels = Tiled::from_raster(&omapix_engine::Raster::new(
                w,
                h,
                vec![blue; (w * h) as usize],
            ));
        });

        // Select the middle active layer
        app.editor.as_mut().unwrap().select_layers(active_id, Vec::new());
        assert_eq!(app.editor.as_ref().unwrap().active, active_id);

        // Set tool to Clone Stamp with Current & Below
        app.tools.select(crate::tools::Tool::CloneStamp);
        app.tools.clone_sample = crate::tools::Sample::CurrentAndBelow;

        // Alt+click to set source at (100.0, 100.0)
        app.tool_input(
            ToolInput::Sample(egui::pos2(100.0, 100.0)),
            egui::Modifiers::ALT,
        );

        // Stroke at (200.0, 200.0)
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(200.0, 200.0)),
            egui::Modifiers::NONE,
        );
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);

        // The active layer should have copied from below (Red), not from above (Blue)
        let active_pixel = app
            .editor
            .as_ref()
            .unwrap()
            .doc
            .layer(active_id)
            .unwrap()
            .pixels
            .get(200, 200);
        assert_eq!(active_pixel, red);

        // With All Layers, cloning stamps Blue from the top layer
        app.tools.clone_sample = crate::tools::Sample::All;
        app.tool_input(
            ToolInput::Sample(egui::pos2(100.0, 100.0)),
            egui::Modifiers::ALT,
        );
        app.tool_input(
            ToolInput::StrokeBegin(egui::pos2(250.0, 200.0)),
            egui::Modifiers::NONE,
        );
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
        let all_pixel = app
            .editor
            .as_ref()
            .unwrap()
            .doc
            .layer(active_id)
            .unwrap()
            .pixels
            .get(250, 200);
        assert_eq!(all_pixel, blue);
    }

    #[test]
    fn gradient_drag_on_pixels_with_selection_and_on_mask() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.tools.select(crate::tools::Tool::Gradient);
        app.tools.foreground = [0, 0, 0];
        app.tools.background = [255, 255, 255];

        // test_app has a rectangular selection from (100, 100) to (200, 200).
        // Initial layer has [30000, 30000, 30000, 65535].
        let active = app.editor.as_ref().unwrap().active;

        // Drag gradient from (100, 150) to (190, 150) on pixels
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(100.0, 150.0)), egui::Modifiers::NONE);
        assert!(app.drawing.is_some());
        app.tool_input(ToolInput::StrokeMove(egui::pos2(190.0, 150.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
        assert!(app.drawing.is_none());

        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Gradient"));
        let layer = app.editor.as_ref().unwrap().doc.layer(active).unwrap();
        // Inside selection at start: foreground (black)
        assert_eq!(layer.pixels.get(100, 150), [0, 0, 0, 65535]);
        // Inside selection at end: background (white)
        assert_eq!(layer.pixels.get(190, 150), [65535, 65535, 65535, 65535]);
        // Inside selection halfway: mid grey
        let mid = layer.pixels.get(145, 150);
        assert!(mid[0].abs_diff(32768) <= 1);
        // Outside selection: untouched
        assert_eq!(layer.pixels.get(50, 50), [30000, 30000, 30000, 65535]);
        assert_eq!(layer.pixels.get(250, 250), [30000, 30000, 30000, 65535]);

        // Now test on a mask:
        app.editor.as_mut().unwrap().doc.selection = None;
        app.run(Command::AddMask, &ctx);
        app.editor.as_mut().unwrap().target = Target::Mask;

        // Drag gradient from (0, 50) to (100, 50) on the mask
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(0.0, 50.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeMove(egui::pos2(100.0, 50.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);

        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Gradient"));
        let mask = app.editor.as_ref().unwrap().doc.layer(active).unwrap().mask.as_ref().unwrap();
        assert_eq!(mask.pixels.get(0, 50), 0);
        assert_eq!(mask.pixels.get(100, 50), 65535);
        assert_eq!(mask.pixels.get(50, 50), 32768);

        // Undo step restores mask
        app.run(Command::Undo, &ctx);
        let mask_after_undo = app.editor.as_ref().unwrap().doc.layer(active).unwrap().mask.as_ref().unwrap();
        assert_eq!(mask_after_undo.pixels.get(0, 50), 65535);
    }

    #[test]
    fn paint_bucket_fill_on_pixels_selection_and_mask() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.tools.select(crate::tools::Tool::PaintBucket);
        app.tools.foreground = [255, 0, 0]; // Red
        let active = app.editor.as_ref().unwrap().active;

        // 1. Fill on pixels with a selection (test_app has rect selection 100..200).
        // Initial layer has [30000, 30000, 30000, 65535].
        // Click at (150, 150) inside selection:
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(150.0, 150.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);

        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Paint Bucket"));
        let layer = app.editor.as_ref().unwrap().doc.layer(active).unwrap();
        // Inside selection: filled with foreground (red)
        assert_eq!(layer.pixels.get(150, 150), [65535, 0, 0, 65535]);
        // Outside selection: untouched
        assert_eq!(layer.pixels.get(50, 50), [30000, 30000, 30000, 65535]);

        // 2. Clear selection and test contiguous vs non-contiguous fill.
        app.editor.as_mut().unwrap().doc.selection = None;
        // Paint a dividing green barrier separating the canvas at x=300
        let green = [0, 65535, 0, 65535];
        let (w, h) = (app.editor.as_ref().unwrap().doc.width, app.editor.as_ref().unwrap().doc.height);
        app.editor.as_mut().unwrap().edit("Divide", |doc, _| {
            let l = doc.layer_mut(active).unwrap();
            let barrier = Selection::rectangle(w, h, (300.0, 0.0), (301.0, h as f32));
            l.pixels = ops::fill_pixels(&l.pixels, Some(green), Some(&barrier));
        });

        // Click on the left side (x=50, y=50) with contiguous = true.
        app.tools.foreground = [0, 0, 255]; // Blue
        app.tools.bucket_contiguous = true;
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);

        let layer = app.editor.as_ref().unwrap().doc.layer(active).unwrap();
        // Left side: blue
        assert_eq!(layer.pixels.get(50, 50), [0, 0, 65535, 65535]);
        // Right side (x=400): untouched because contiguous stopped at the green barrier
        assert_eq!(layer.pixels.get(400, 50), [30000, 30000, 30000, 65535]);

        // Undo, and fill with non-contiguous
        app.run(Command::Undo, &ctx);
        app.tools.bucket_contiguous = false;
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);

        let layer = app.editor.as_ref().unwrap().doc.layer(active).unwrap();
        // Left side: blue
        assert_eq!(layer.pixels.get(50, 50), [0, 0, 65535, 65535]);
        // Right side: ALSO blue because non-contiguous filled all matching pixels
        assert_eq!(layer.pixels.get(400, 50), [0, 0, 65535, 65535]);

        // 3. Fill on a mask
        app.run(Command::AddMask, &ctx);
        app.editor.as_mut().unwrap().target = Target::Mask;
        app.tools.foreground = [0, 0, 0]; // Black -> mask value 0
        app.tools.bucket_contiguous = true;
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);

        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Paint Bucket"));
        let mask = app.editor.as_ref().unwrap().doc.layer(active).unwrap().mask.as_ref().unwrap();
        assert_eq!(mask.pixels.get(50, 50), 0);

        // 4. Click outside canvas does nothing
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(-10.0, 50.0)), egui::Modifiers::NONE);
        app.tool_input(ToolInput::StrokeEnd, egui::Modifiers::NONE);
        // Undo label unchanged
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Paint Bucket"));

        // 5. Locked layer check
        app.editor.as_mut().unwrap().target = Target::Pixels;
        app.editor.as_mut().unwrap().doc.layer_mut(active).unwrap().locks.pixels = true;
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)), egui::Modifiers::NONE);
        assert_eq!(
            app.status.as_ref().map(|s| s.0.as_str()),
            Some("Could not use the paint bucket tool because the layer is locked")
        );

        // 6. Opacity: half red over the blue.
        app.editor.as_mut().unwrap().doc.layer_mut(active).unwrap().locks.pixels = false;
        app.tools.bucket_opacity = 0.5;
        app.tools.foreground = [255, 0, 0];
        app.tool_input(ToolInput::StrokeBegin(egui::pos2(50.0, 50.0)), egui::Modifiers::NONE);
        let [r, g, b, a] = app.editor.as_ref().unwrap().doc.layer(active).unwrap().pixels.get(50, 50);
        assert!(r.abs_diff(32768) <= 1 && g == 0 && b.abs_diff(32768) <= 1 && a == 65535, "{r} {g} {b} {a}");
    }

    #[test]
    fn changing_brush_size_in_tools_saves_once() {
        let dir = std::env::temp_dir().join(format!("omapix-tool-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tools.toml");

        let ctx = egui::Context::default();
        let mut app = test_app();
        app.tools_path = Some(path.clone());
        app.saved_tools = toml::to_string(&app.tools).unwrap();

        // While a pointer button is held, save check does not write.
        app.tools.set_size(150.0);
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::PointerButton {
            pos: Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        let mut out = ctx.run_ui(raw, |_| {});
        out.textures_delta.clear();
        app.save_tools(&ctx);
        assert!(!path.exists(), "must not write while pointer button is down");

        // When pointer is released, save check writes the file once.
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::PointerButton {
            pos: Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut out = ctx.run_ui(raw, |_| {});
        out.textures_delta.clear();
        app.save_tools(&ctx);
        assert!(path.exists(), "must write once pointer is released");

        let loaded: Tools = crate::settings::load_from(&path, &Tools::default());
        assert_eq!(toml::to_string(&loaded).unwrap(), toml::to_string(&app.tools).unwrap());

        // Remove the file and verify subsequent save check does not write it again.
        std::fs::remove_file(&path).unwrap();
        app.save_tools(&ctx);
        assert!(!path.exists(), "must not write again when settings haven't changed");

        // A file with only some settings keeps the defaults for the rest.
        std::fs::write(
            &path,
            "wand_tolerance = 12\nbucket_opacity = 0.75\nbucket_tolerance = 48\nbucket_anti_alias = false\nbucket_contiguous = false\nbucket_all_layers = true\n[eraser]\nsize = 42.0\n",
        ).unwrap();
        let loaded: Tools = crate::settings::load_from(&path, &Tools::default());
        assert_eq!((loaded.wand_tolerance, loaded.gradient_opacity), (12, 1.0));
        assert_eq!(
            (
                loaded.bucket_opacity,
                loaded.bucket_tolerance,
                loaded.bucket_anti_alias,
                loaded.bucket_contiguous,
                loaded.bucket_all_layers,
            ),
            (0.75, 48, false, false, true)
        );
        let mut eraser = loaded.clone();
        eraser.tool = crate::tools::Tool::Eraser;
        assert_eq!((eraser.settings().size, eraser.settings().hardness), (42.0, 0.5));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_rotation_90_cw_and_undo_redo_updates_canvas_and_fits_view() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let (w, h) = (600, 400);

        let canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        app.editor.as_mut().unwrap().canvas.lay_out_for_test(canvas_rect, 1.0);

        assert_eq!(app.editor.as_ref().unwrap().doc.width, w);
        assert_eq!(app.editor.as_ref().unwrap().doc.height, h);
        assert_eq!(app.editor.as_ref().unwrap().canvas.width(), w);
        assert_eq!(app.editor.as_ref().unwrap().canvas.height(), h);

        app.run(Command::Rotate90Cw, &ctx);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.undo_label(), Some("Rotate 90° Clockwise"));
        assert_eq!((editor.doc.width, editor.doc.height), (h, w));
        assert_eq!((editor.canvas.width(), editor.canvas.height()), (h, w));
        assert_eq!(editor.canvas.levels()[0], (h, w));

        app.run(Command::Undo, &ctx);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!((editor.doc.width, editor.doc.height), (w, h));
        assert_eq!((editor.canvas.width(), editor.canvas.height()), (w, h));
        assert_eq!(editor.canvas.levels()[0], (w, h));

        app.run(Command::Redo, &ctx);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!((editor.doc.width, editor.doc.height), (h, w));
        assert_eq!((editor.canvas.width(), editor.canvas.height()), (h, w));
        assert_eq!(editor.canvas.levels()[0], (h, w));
    }

    #[test]
    fn rotation_commits_free_transform_in_progress() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let background = crate::tools::grey(app.tools.background);
        app.editor.as_mut().unwrap().begin_transform(background).unwrap();
        assert!(app.editor.as_ref().unwrap().transform().is_some());

        app.run(Command::Rotate180, &ctx);
        assert!(app.editor.as_ref().unwrap().transform().is_none());
        assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some("Rotate 180°"));
    }

    #[test]
    fn image_size_only_enlarges_with_ai_when_larger_and_the_model_is_there() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        // A frame of the dialog; whether Enlarge with AI's note was shown.
        let frame = |app: &mut App, events: Vec<egui::Event>| {
            let input = egui::RawInput { events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.dialogs(ui.ctx()));
            out.textures_delta.clear();
            fn find(shape: &egui::Shape) -> bool {
                match shape {
                    egui::Shape::Text(t) => t.galley.text().contains("Upscale layer"),
                    egui::Shape::Vec(v) => v.iter().any(find),
                    _ => false,
                }
            }
            out.shapes.iter().any(|s| find(&s.shape))
        };
        // A modal is laid out, unseen, on its first frame; Enter on the
        // second.
        let enter = |app: &mut App| {
            let key = egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            frame(app, vec![]) | frame(app, vec![key])
        };
        let size = |app: &App| {
            let doc = &app.editor.as_ref().unwrap().doc;
            (doc.width, doc.height, doc.layers.len())
        };
        let layers = size(&app).2;

        // Smaller: the ordinary way, though AI was ticked.
        app.dialog = Some(Dialog::ImageSize { width: 300, height: 200, constrain: true, ai: true });
        assert!(!enter(&mut app));
        assert_eq!((size(&app), app.upscaling.busy()), ((300, 200, layers), None));
        app.run(Command::Undo, &ctx);

        // Larger, without the model: the ordinary way too.
        app.models.insert(omapix_ai::upscale::MODEL, false);
        app.dialog = Some(Dialog::ImageSize { width: 1200, height: 800, constrain: true, ai: true });
        assert!(!enter(&mut app));
        assert_eq!((size(&app), app.upscaling.busy()), ((1200, 800, layers), None));
        app.run(Command::Undo, &ctx);

        // Larger, with it: the note says where the model's work goes.
        app.models.insert(omapix_ai::upscale::MODEL, true);
        app.dialog = Some(Dialog::ImageSize { width: 1200, height: 800, constrain: true, ai: true });
        frame(&mut app, vec![]);
        assert!(frame(&mut app, vec![]));
        assert_eq!(size(&app), (600, 400, layers));
    }

    #[test]
    fn image_size_canvas_size_and_crop_run_and_undo() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let size = |app: &App| {
            let doc = &app.editor.as_ref().unwrap().doc;
            (doc.width, doc.height)
        };

        // The dialogs start at the current size; OK applies what's typed.
        app.run(Command::ImageSize, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::ImageSize { width: 600, height: 400, constrain: true, ai: false })));
        app.dialog = None;
        app.resize_image(300, 200);
        assert_eq!((size(&app), app.editor.as_ref().unwrap().undo_label()), ((300, 200), Some("Image Size")));
        app.run(Command::Undo, &ctx);

        // Canvas Size anchored top left, extended in black.
        app.run(Command::CanvasSize, &ctx);
        assert!(matches!(app.dialog, Some(Dialog::CanvasSize { anchor: (1, 1), .. })));
        app.dialog = None;
        app.resize_canvas(700, 450, (0, 0), Extension::Black);
        let doc = &app.editor.as_ref().unwrap().doc;
        assert_eq!((doc.width, doc.height), (700, 450));
        assert_eq!(doc.layers[0].pixels.get(0, 0), [30000, 30000, 30000, 65535]);
        assert_eq!(doc.layers[0].pixels.get(650, 420), [0, 0, 0, 65535]);
        // Centred, shrinking crops half the difference off each side.
        app.run(Command::Undo, &ctx);
        app.resize_canvas(500, 400, (1, 1), Extension::Transparent);
        let sel = &app.editor.as_ref().unwrap().doc.selection.as_ref().unwrap();
        assert_eq!(sel.bounds(), Some([50, 100, 100, 100]));
        app.run(Command::Undo, &ctx);

        // Crop goes to the selection (100, 100, 100 × 100) and deselects.
        assert!(app.enabled(Command::Crop));
        app.run(Command::Crop, &ctx);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!((size(&app), editor.undo_label()), ((100, 100), Some("Crop")));
        assert!(editor.doc.selection.is_none() && !app.enabled(Command::Crop));
        app.run(Command::Undo, &ctx);
        assert_eq!(size(&app), (600, 400));
    }

    #[test]
    fn all_rotation_and_flip_commands_run_and_undo() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let commands = [
            (Command::Rotate180, "Rotate 180°", 600, 400),
            (Command::Rotate90Cw, "Rotate 90° Clockwise", 400, 600),
            (Command::Rotate90Ccw, "Rotate 90° Counter Clockwise", 400, 600),
            (Command::FlipCanvasHorizontal, "Flip Canvas Horizontal", 600, 400),
            (Command::FlipCanvasVertical, "Flip Canvas Vertical", 600, 400),
        ];
        for (cmd, label, exp_w, exp_h) in commands {
            app.run(cmd, &ctx);
            assert_eq!(app.editor.as_ref().unwrap().undo_label(), Some(label));
            assert_eq!(
                (app.editor.as_ref().unwrap().doc.width, app.editor.as_ref().unwrap().doc.height),
                (exp_w, exp_h)
            );
            app.run(Command::Undo, &ctx);
        }
    }

    #[test]
    fn finish_creates_sharpen_and_grain_layers_in_one_undo_step() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let initial_layer_count = app.editor.as_ref().unwrap().doc.layers.len();

        app.run(Command::Finish, &ctx);

        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.undo_label(), Some("Finish"));
        assert_eq!(editor.doc.layers.len(), initial_layer_count + 2);

        // Top layer is Grain in Overlay blend mode
        let grain_layer = editor.doc.layers.last().unwrap();
        assert_eq!(grain_layer.name, "Grain");
        assert_eq!(grain_layer.blend, omapix_engine::BlendMode::Overlay);

        // Below it is Sharpen layer
        let sharpen_layer = &editor.doc.layers[editor.doc.layers.len() - 2];
        assert_eq!(sharpen_layer.name, "Sharpen");

        // Undoing undoes the whole Finish action in one step
        app.run(Command::Undo, &ctx);
        assert_eq!(app.editor.as_ref().unwrap().doc.layers.len(), initial_layer_count);
    }

    #[test]
    fn batch_export_runs_in_the_background_and_says_what_failed() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let dir = std::env::temp_dir().join(format!("omapix-batch-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.tif");
        omapix_engine::export::tiff(&app.editor.as_ref().unwrap().doc, &good).unwrap();
        let bad = dir.join("bad.tif");
        std::fs::write(&bad, b"not a tiff").unwrap();
        let settings = crate::settings::BatchExport { folder: Some(dir.join("out")), ..Default::default() };
        app.batch_export(vec![good, bad], settings, &ctx);
        for _ in 0..1000 {
            app.poll(&ctx);
            if app.batch_export.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (text, error, _) = app.status.clone().expect("a message");
        assert!(error && text.starts_with("Exported 1 file; 1 failed: bad.tif"), "{text}");
        assert!(dir.join("out/good.jpg").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn script_steps_run_auto_retouch_with_a_preset_or_as_last_used() {
        use crate::auto_retouch::Preset;
        assert!(matches!(ScriptStep::parse("AutoRetouch Natural"), Some(ScriptStep::AutoRetouch(Some(Preset::Natural)))));
        assert!(matches!(ScriptStep::parse("AutoRetouch"), Some(ScriptStep::AutoRetouch(None))));
        assert!(ScriptStep::parse("AutoRetouch Gentle").is_none());
    }

    /// End to end: `AutoRetouch Natural` as an `OMAPIX_SCRIPT` step on
    /// `OMAPIX_FACE_PHOTO`, checking the layers it builds. Needs the models
    /// (scripts/fetch-models.sh).
    #[test]
    #[ignore]
    fn the_auto_retouch_script_step_builds_a_group_for_each_face() {
        let Ok(photo) = std::env::var("OMAPIX_FACE_PHOTO") else { return };
        let ctx = egui::Context::default();
        let mut app = test_app();
        let doc = omapix_engine::io::load(std::path::Path::new(&photo)).unwrap();
        let (render, _) = render_view(&doc, View::Image, [0; 4], None);
        let mut editor = Editor::new(doc).unwrap();
        editor.canvas.set_render(Arc::new(render));
        editor.canvas.lay_out_for_test(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)), 0.1);
        app.editor = Some(editor);
        app.script.push_back(ScriptStep::parse("AutoRetouch Natural").unwrap());
        let done = |app: &App| app.script.is_empty() && app.dialog.is_none() && app.editor.as_ref().unwrap().busy().is_none();
        for _ in 0..12000 {
            app.run_script(&ctx);
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.dialogs(ui.ctx()));
            out.textures_delta.clear();
            app.editor.as_mut().unwrap().update(&ctx);
            if done(&app) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done(&app), "{:?}", app.status);
        let editor = app.editor.as_ref().unwrap();
        let names: Vec<_> = editor.doc.layers.iter().map(|l| l.name.as_str()).collect();
        eprintln!("{names:?}");
        assert_eq!(names.last(), Some(&"Retouch"));
        assert_eq!(names[names.len() - 2], "Face 1");
        assert!(names.contains(&"Smooth Skin") && names.contains(&"Dodge & Burn"));
        assert_eq!(editor.undo_label(), Some("Auto Retouch"));
        assert_eq!(app.filters.auto_retouch, crate::auto_retouch::Preset::Natural.settings());
    }

    #[test]
    fn a_script_step_runs_generative_fill_with_its_prompt() {
        let prompt = |step| match ScriptStep::parse(step) {
            Some(ScriptStep::GenerativeFill(prompt)) => Some(prompt),
            _ => None,
        };
        assert_eq!(prompt("GenerativeFill remove the 2 chairs").as_deref(), Some("remove the 2 chairs"));
        assert_eq!(prompt("GenerativeFill").as_deref(), Some(""));
    }

    /// End to end: `GenerativeFill` as an `OMAPIX_SCRIPT` step on
    /// `OMAPIX_FILL_PHOTO` with the box `OMAPIX_FILL_BOX` (`x0 y0 x1 y1`)
    /// selected, or with `OMAPIX_FILL_EXTEND=1`, with the canvas grown to
    /// that box and nothing selected, checking the layer it leaves. Needs
    /// the model (scripts/fetch-models.sh fill-flux2-klein-4b) and the GPU.
    #[test]
    #[ignore]
    fn the_generative_fill_script_step_leaves_a_masked_layer() {
        let (Ok(photo), Ok(corners)) = (std::env::var("OMAPIX_FILL_PHOTO"), std::env::var("OMAPIX_FILL_BOX")) else { return };
        let corners: Vec<f32> = corners.split_whitespace().map(|c| c.parse().unwrap()).collect();
        let ctx = egui::Context::default();
        let mut app = test_app();
        let mut doc = omapix_engine::io::load(std::path::Path::new(&photo)).unwrap();
        let extend = std::env::var_os("OMAPIX_FILL_EXTEND").is_some();
        if extend {
            let [x0, y0, x1, y1] = [0, 1, 2, 3].map(|i| corners[i] as i32);
            doc.resize_canvas((x1 - x0) as u32, (y1 - y0) as u32, -x0, -y0, None);
        } else {
            doc.selection = Some(Selection::rectangle(doc.width, doc.height, (corners[0], corners[1]), (corners[2], corners[3])));
        }
        let (render, _) = render_view(&doc, View::Image, [0; 4], None);
        let mut editor = Editor::new(doc).unwrap();
        editor.canvas.set_render(Arc::new(render));
        editor.canvas.lay_out_for_test(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)), 0.1);
        app.editor = Some(editor);
        // Content-Aware Fill first, so its model's on the card and has to
        // give way.
        if !extend {
            app.run(Command::ContentAwareFill, &ctx);
            while app.content_fill.busy() {
                std::thread::sleep(Duration::from_millis(5));
                app.content_fill.poll(app.editor.as_mut().unwrap());
            }
            let editor = app.editor.as_mut().unwrap();
            assert_eq!(editor.undo_label(), Some("Content-Aware Fill"));
            editor.undo();
        }
        let before = app.editor.as_ref().unwrap().doc.layers.len();
        app.script.push_back(ScriptStep::parse("GenerativeFill").unwrap());
        let done = |app: &App| app.script.is_empty() && app.dialog.is_none();
        for _ in 0..24000 {
            app.run_script(&ctx);
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.dialogs(ui.ctx()));
            out.textures_delta.clear();
            app.editor.as_mut().unwrap().update(&ctx);
            if done(&app) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done(&app), "{:?}", app.status);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.doc.layers.len(), before + 1, "{:?}", app.status);
        let layer = editor.doc.layer(editor.active).unwrap();
        assert_eq!(layer.name, "Generative Fill");
        // The middle of the box, or the new canvas's first corner.
        let (x, y) = if extend { (0, 0) } else { (((corners[0] + corners[2]) / 2.0) as u32, ((corners[1] + corners[3]) / 2.0) as u32) };
        assert_eq!(layer.mask.as_ref().unwrap().pixels.get(x, y), 65535);
        assert_eq!(layer.pixels.get(x, y)[3], 65535);
        assert_eq!(editor.undo_label(), Some("Generative Fill"));
    }

    #[test]
    fn batch_export_dialog_opens_with_saved_settings() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.filters.batch_export.long_edge = 1920;
        app.filters.batch_export.jpeg = false;
        app.run(Command::BatchExport, &ctx);
        let Some(Dialog::BatchExport { settings, .. }) = &app.dialog else {
            panic!("no dialog");
        };
        assert_eq!(settings, &app.filters.batch_export);
    }
}

