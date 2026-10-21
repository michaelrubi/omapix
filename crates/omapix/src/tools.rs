//! Painting tools: the toolbar on the left, the options bar under the menus,
//! and Photoshop's single-key tool shortcuts.

use egui::{Button, ComboBox, Key, Modifiers, Pos2, RichText, Slider, Ui, Vec2};
use omapix_engine::brush::{BrushSettings, Paint};
use omapix_engine::dynamics::{ColorDynamics, DualBrush, Dynamics, MAX_COUNT, PenAngle, Pose};
use omapix_engine::selection::Combine;
use omapix_engine::texture::{Mode, Texture};
use omapix_engine::toning::ToneRange;
use omapix_engine::warp::Brush as LiquifyBrush;
use omapix_engine::{ColorProfile, DisplayTransform, Pixel};

use crate::commands::Command;
use crate::editor::Target;
use crate::theme::Theme;

/// A tool key held longer than this (seconds) is spring-loaded.
const SPRING: f64 = 0.4;
const MOVE_ICON: &str = "\u{f047}";
const BRUSH_ICON: &str = "\u{f1fc}";
const ERASER_ICON: &str = "\u{f12d}";
const CLONE_ICON: &str = "\u{f24d}";
const HEAL_ICON: &str = "\u{f0fa}";
const SPOT_ICON: &str = "\u{f462}";
const WAND_ICON: &str = "\u{f0d0}";
const OBJECT_ICON: &str = "\u{f05b}";
const QUICK_ICON: &str = "\u{f1fc}";
const EYEDROPPER_ICON: &str = "\u{f1fb}";
const MARQUEE_ICON: &str = "\u{f096}";
const ELLIPSE_ICON: &str = "\u{f10c}";
const LASSO_ICON: &str = "\u{f0c4}";
const GRADIENT_ICON: &str = "\u{f069b}";
const CROP_ICON: &str = "\u{f125}";
const BUCKET_ICON: &str = "\u{f0266}";
const DODGE_ICON: &str = "\u{f185}";
const BURN_ICON: &str = "\u{f06d}";
const SPONGE_ICON: &str = "\u{f043}";

const MIN_SIZE: f32 = 1.0;
const MAX_SIZE: f32 = 5000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolGroup {
    Move,
    Marquee,
    Lasso,
    Wand,
    Crop,
    Eyedropper,
    Healing,
    Brush,
    CloneStamp,
    Eraser,
    Gradient,
    Dodge,
}

impl ToolGroup {
    pub const ALL: &[ToolGroup] = &[
        ToolGroup::Move,
        ToolGroup::Marquee,
        ToolGroup::Lasso,
        ToolGroup::Wand,
        ToolGroup::Crop,
        ToolGroup::Eyedropper,
        ToolGroup::Healing,
        ToolGroup::Brush,
        ToolGroup::CloneStamp,
        ToolGroup::Eraser,
        ToolGroup::Gradient,
        ToolGroup::Dodge,
    ];

    pub fn tools(self) -> &'static [Tool] {
        match self {
            ToolGroup::Move => &[Tool::Move],
            ToolGroup::Marquee => &[Tool::Marquee, Tool::EllipticalMarquee],
            ToolGroup::Lasso => &[Tool::Lasso],
            ToolGroup::Wand => &[Tool::ObjectSelection, Tool::QuickSelection, Tool::MagicWand],
            ToolGroup::Crop => &[Tool::Crop],
            ToolGroup::Eyedropper => &[Tool::Eyedropper],
            ToolGroup::Healing => &[Tool::SpotHealing, Tool::Healing],
            ToolGroup::Brush => &[Tool::Brush],
            ToolGroup::CloneStamp => &[Tool::CloneStamp],
            ToolGroup::Eraser => &[Tool::Eraser],
            ToolGroup::Gradient => &[Tool::Gradient, Tool::PaintBucket],
            ToolGroup::Dodge => &[Tool::Dodge, Tool::Burn, Tool::Sponge],
        }
    }

    pub fn default_key(self) -> Option<Key> {
        let k = match self {
            ToolGroup::Move => Key::V,
            ToolGroup::Marquee => Key::M,
            ToolGroup::Lasso => Key::L,
            ToolGroup::Wand => Key::W,
            ToolGroup::Crop => Key::C,
            ToolGroup::Eyedropper => Key::I,
            ToolGroup::Healing => Key::J,
            ToolGroup::Brush => Key::B,
            ToolGroup::CloneStamp => Key::S,
            ToolGroup::Eraser => Key::E,
            ToolGroup::Gradient => Key::G,
            ToolGroup::Dodge => Key::O,
        };
        Some(k)
    }

    pub fn key(self) -> Option<Key> {
        crate::hotkeys::current().tool_group(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Move,
    Brush,
    Eraser,
    CloneStamp,
    SpotHealing,
    Healing,
    Marquee,
    EllipticalMarquee,
    Lasso,
    MagicWand,
    ObjectSelection,
    QuickSelection,
    Eyedropper,
    Gradient,
    PaintBucket,
    Crop,
    Dodge,
    Burn,
    Sponge,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Move => "Move",
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::CloneStamp => "Clone Stamp",
            Tool::SpotHealing => "Spot Healing Brush",
            Tool::Healing => "Healing Brush",
            Tool::Marquee => "Rectangular Marquee",
            Tool::EllipticalMarquee => "Elliptical Marquee",
            Tool::Lasso => "Lasso",
            Tool::MagicWand => "Magic Wand",
            Tool::ObjectSelection => "Object Selection",
            Tool::QuickSelection => "Quick Selection",
            Tool::Eyedropper => "Eyedropper",
            Tool::Gradient => "Gradient",
            Tool::PaintBucket => "Paint Bucket",
            Tool::Crop => "Crop",
            Tool::Dodge => "Dodge",
            Tool::Burn => "Burn",
            Tool::Sponge => "Sponge",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Tool::Move => MOVE_ICON,
            Tool::Brush => BRUSH_ICON,
            Tool::Eraser => ERASER_ICON,
            Tool::CloneStamp => CLONE_ICON,
            Tool::SpotHealing => SPOT_ICON,
            Tool::Healing => HEAL_ICON,
            Tool::Marquee => MARQUEE_ICON,
            Tool::EllipticalMarquee => ELLIPSE_ICON,
            Tool::Lasso => LASSO_ICON,
            Tool::MagicWand => WAND_ICON,
            Tool::ObjectSelection => OBJECT_ICON,
            Tool::QuickSelection => QUICK_ICON,
            Tool::Eyedropper => EYEDROPPER_ICON,
            Tool::Gradient => GRADIENT_ICON,
            Tool::PaintBucket => BUCKET_ICON,
            Tool::Crop => CROP_ICON,
            Tool::Dodge => DODGE_ICON,
            Tool::Burn => BURN_ICON,
            Tool::Sponge => SPONGE_ICON,
        }
    }

    pub fn shortcut_letter(self) -> Option<&'static str> {
        self.group().key().map(|k| k.symbol_or_name())
    }

    pub fn group(self) -> ToolGroup {
        match self {
            Tool::Move => ToolGroup::Move,
            Tool::Marquee | Tool::EllipticalMarquee => ToolGroup::Marquee,
            Tool::Lasso => ToolGroup::Lasso,
            Tool::MagicWand | Tool::ObjectSelection | Tool::QuickSelection => ToolGroup::Wand,
            Tool::Eyedropper => ToolGroup::Eyedropper,
            Tool::SpotHealing | Tool::Healing => ToolGroup::Healing,
            Tool::Brush => ToolGroup::Brush,
            Tool::CloneStamp => ToolGroup::CloneStamp,
            Tool::Eraser => ToolGroup::Eraser,
            Tool::Gradient | Tool::PaintBucket => ToolGroup::Gradient,
            Tool::Dodge | Tool::Burn | Tool::Sponge => ToolGroup::Dodge,
            Tool::Crop => ToolGroup::Crop,
        }
    }

    /// Tools that make selections rather than paint.
    pub fn selects(self) -> bool {
        matches!(
            self,
            Tool::Marquee | Tool::EllipticalMarquee | Tool::Lasso | Tool::MagicWand | Tool::ObjectSelection | Tool::QuickSelection
        )
    }

    /// Tools with a brush, and so its outline: those that paint, and Quick
    /// Selection.
    pub fn has_brush(self) -> bool {
        self.paints() || self == Tool::QuickSelection
    }

    /// Tools that paint with a brush, and so show its outline.
    pub fn paints(self) -> bool {
        !self.selects() && !matches!(self, Tool::Move | Tool::Eyedropper | Tool::Gradient | Tool::PaintBucket | Tool::Crop)
    }

    /// Tools that copy pixels from a source point set with Alt+click.
    pub fn copies(self) -> bool {
        matches!(self, Tool::CloneStamp | Tool::Healing)
    }

    /// Tools where holding Alt picks up a colour, as the Eyedropper does.
    pub fn alt_picks_colour(self) -> bool {
        self.paints() && !self.copies() && !matches!(self, Tool::Dodge | Tool::Burn | Tool::Sponge)
    }
}

/// Eyedropper sample size presets matching Photoshop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SampleSize {
    #[default]
    Point,
    ThreeByThree,
    FiveByFive,
    ElevenByEleven,
}

impl SampleSize {
    pub const ALL: [SampleSize; 4] = [SampleSize::Point, SampleSize::ThreeByThree, SampleSize::FiveByFive, SampleSize::ElevenByEleven];

    pub fn radius(self) -> u32 {
        match self {
            SampleSize::Point => 0,
            SampleSize::ThreeByThree => 1,
            SampleSize::FiveByFive => 2,
            SampleSize::ElevenByEleven => 5,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SampleSize::Point => "Point",
            SampleSize::ThreeByThree => "3×3 average",
            SampleSize::FiveByFive => "5×5 average",
            SampleSize::ElevenByEleven => "11×11 average",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Sample {
    Current,
    CurrentAndBelow,
    #[default]
    All,
}

impl Sample {
    pub const ALL: [Sample; 3] = [Sample::Current, Sample::CurrentAndBelow, Sample::All];

    pub fn label(self) -> &'static str {
        match self {
            Sample::Current => "Current Layer",
            Sample::CurrentAndBelow => "Current & Below",
            Sample::All => "All Layers",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorBadge {
    Add,
    Subtract,
    Intersect,
    Copy,
    /// Holding Alt picks up a colour.
    Eyedropper,
}

/// Badge shown at the cursor when modifier keys change what a click or drag does.
pub fn cursor_badge(tool: Tool, modifiers: Modifiers) -> Option<CursorBadge> {
    if tool.selects() {
        match Combine::from_modifiers(modifiers.shift, modifiers.alt) {
            Combine::Add => Some(CursorBadge::Add),
            Combine::Subtract => Some(CursorBadge::Subtract),
            Combine::Intersect => Some(CursorBadge::Intersect),
            Combine::Replace => None,
        }
    } else if tool == Tool::Move && modifiers.alt {
        Some(CursorBadge::Copy)
    } else if tool.alt_picks_colour() && modifiers.alt {
        Some(CursorBadge::Eyedropper)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum GradientColors {
    #[default]
    ForegroundToBackground,
    ForegroundToTransparent,
}

impl GradientColors {
    pub const ALL: [GradientColors; 2] = [GradientColors::ForegroundToBackground, GradientColors::ForegroundToTransparent];

    pub fn label(self) -> &'static str {
        match self {
            GradientColors::ForegroundToBackground => "Foreground to Background",
            GradientColors::ForegroundToTransparent => "Foreground to Transparent",
        }
    }
}

/// Mode for the Sponge tool (desaturate or saturate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SpongeMode {
    #[default]
    Desaturate,
    Saturate,
}

impl SpongeMode {
    pub const ALL: [SpongeMode; 2] = [SpongeMode::Desaturate, SpongeMode::Saturate];

    pub fn label(self) -> &'static str {
        match self {
            SpongeMode::Desaturate => "Desaturate",
            SpongeMode::Saturate => "Saturate",
        }
    }
}

/// A 0–1 setting as a percent slider in the options bar.
fn percent(ui: &mut Ui, label: &str, value: &mut f32) {
    ui.label(label);
    let mut p = *value * 100.0;
    if ui.add(Slider::new(&mut p, 0.0..=100.0).suffix("%").fixed_decimals(0)).changed() {
        *value = p / 100.0;
    }
}

/// Liquify's brush and its settings.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Liquify {
    pub brush: LiquifyBrush,
    /// Diameter in image pixels.
    pub size: f32,
    /// How strongly it works, 0–1.
    pub pressure: f32,
    /// A pen's pressure scales `pressure`.
    pub pen_pressure: bool,
    /// Face-Aware Liquify's panel is open, and Body Reshape's.
    pub face_aware: bool,
    pub body: bool,
}

impl Default for Liquify {
    fn default() -> Self {
        Self {
            brush: LiquifyBrush::ForwardWarp,
            size: 100.0,
            pressure: 1.0,
            pen_pressure: true,
            face_aware: false,
            body: false,
        }
    }
}

impl Liquify {
    /// Photoshop's key for each brush.
    fn key(brush: LiquifyBrush) -> Key {
        match brush {
            LiquifyBrush::ForwardWarp => Key::W,
            LiquifyBrush::Reconstruct => Key::R,
            LiquifyBrush::Pucker => Key::S,
            LiquifyBrush::Bloat => Key::B,
            LiquifyBrush::PushLeft => Key::O,
        }
    }

    /// While Liquify is open, its keys pick a brush, [ and ] change the
    /// size, A (Photoshop's Face tool) opens and closes Face-Aware Liquify,
    /// and Y Body Reshape.
    pub fn keys(&mut self, ctx: &egui::Context) {
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::A) {
                self.face_aware = !self.face_aware;
            }
            if i.consume_key(Modifiers::NONE, Key::Y) {
                self.body = !self.body;
            }
            for brush in LiquifyBrush::ALL {
                if i.consume_key(Modifiers::NONE, Self::key(brush)) {
                    self.brush = brush;
                }
            }
            for (key, bigger) in [(Key::OpenBracket, false), (Key::CloseBracket, true)] {
                if i.consume_key(Modifiers::NONE, key) {
                    self.size = step_size(self.size, bigger).clamp(MIN_SIZE, MAX_SIZE);
                }
            }
        });
    }

    /// Alt+right-drag: change the size by `by` pixels.
    pub fn resize(&mut self, by: f32) {
        self.size = (self.size + by).clamp(MIN_SIZE, MAX_SIZE);
    }

    /// `restore` is how much of the brushes' warp Restore All takes out,
    /// 0–1, and `missing` the models Face-Aware Liquify and Body Reshape
    /// need that aren't installed.
    pub fn options_bar(&mut self, ui: &mut Ui, theme: &Theme, restore: &mut f32, missing: [&[&str]; 2]) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Liquify").strong());
            ui.separator();
            for brush in LiquifyBrush::ALL {
                let name = format!("{} ({})", brush.name(), Self::key(brush).name());
                ui.selectable_value(&mut self.brush, brush, name);
            }
            ui.separator();
            ui.label("Size");
            let size = egui::DragValue::new(&mut self.size).range(MIN_SIZE..=MAX_SIZE).suffix(" px").fixed_decimals(0);
            ui.add(size);
            percent(ui, "Pressure", &mut self.pressure);
            ui.toggle_value(&mut self.pen_pressure, "✒").on_hover_text("A pen's pressure sets the pressure");
            ui.separator();
            percent(ui, "Restore All", restore);
            ui.separator();
            let panels = [
                (&mut self.face_aware, "Face-Aware (A)", "Sliders for each face's shape and symmetry"),
                (&mut self.body, "Body (Y)", "Sliders for each person's shape"),
            ];
            for ((open, name, hint), missing) in panels.into_iter().zip(missing) {
                ui.add_enabled_ui(missing.is_empty(), |ui| {
                    let plural = if missing.len() > 1 { "s" } else { "" };
                    let needs = format!("Needs the {} model{plural}: see Help › AI Models", missing.join(", "));
                    ui.toggle_value(open, name).on_hover_text(hint).on_disabled_hover_text(needs);
                });
            }
            ui.separator();
            let hint = "Alt swaps Pucker and Bloat, and pushes right · Enter applies · Esc cancels";
            ui.label(RichText::new(hint).color(theme.dark_foreground));
        });
    }
}

/// The tools and their options. The options (not the tool, the colours
/// or the clone source) are kept between runs in `~/.config/omapix/tools.toml`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Tools {
    #[serde(skip)]
    pub tool: Tool,
    #[serde(skip)]
    last_marquee: Tool,
    #[serde(skip)]
    last_healing: Tool,
    #[serde(skip)]
    last_wand: Tool,
    #[serde(skip)]
    last_gradient: Tool,
    #[serde(skip)]
    last_dodge: Tool,
    /// The tool before the Crop tool, which Esc goes back to.
    #[serde(skip)]
    before_crop: Tool,
    /// The tool key being held, the tool before it, and when it went down.
    #[serde(skip)]
    held: Option<(Key, Tool, f64)>,
    brush: BrushSettings,
    eraser: BrushSettings,
    clone: BrushSettings,
    spot: BrushSettings,
    heal: BrushSettings,
    quick: BrushSettings,
    dodge: BrushSettings,
    burn: BrushSettings,
    sponge: BrushSettings,
    pub dodge_range: ToneRange,
    pub dodge_protect: bool,
    pub burn_range: ToneRange,
    pub burn_protect: bool,
    pub sponge_mode: SpongeMode,
    pub sponge_vibrance: bool,
    /// Object and Quick Selection: where the model's confidence is cut off
    /// (0 is the model's own; lower selects more, higher less).
    pub ai_threshold: f32,
    /// The threshold slider moved this frame (`true`: let go).
    #[serde(skip)]
    pub threshold_moved: Option<bool>,
    /// Where the clone/heal source was set with Alt+click, in image pixels.
    #[serde(skip)]
    source: Option<Pos2>,
    /// Source minus destination, fixed by the first stroke after setting a
    /// source and kept for later strokes (Photoshop's "Aligned").
    #[serde(skip)]
    offset: Option<Vec2>,
    /// Where each tool samples pixels from (Current Layer, Current & Below,
    /// All Layers): each keeps its own, as in Photoshop. Other tools'
    /// Alt+click uses the Eyedropper's.
    pub eyedropper_sample: Sample,
    pub clone_sample: Sample,
    pub heal_sample: Sample,
    pub spot_sample: Sample,
    pub wand_sample: Sample,
    /// Eyedropper sample size (Point, 3×3, 5×5, 11×11 average).
    pub sample_size: SampleSize,
    /// Magic Wand settings matching Photoshop.
    pub wand_tolerance: u8,
    pub wand_contiguous: bool,
    pub wand_anti_alias: bool,
    /// Gradient tool settings matching Photoshop.
    pub gradient_type: omapix_engine::ops::GradientType,
    pub gradient_colors: GradientColors,
    pub gradient_reverse: bool,
    pub gradient_opacity: f32,
    /// Paint Bucket tool settings matching Photoshop.
    pub bucket_opacity: f32,
    pub bucket_tolerance: u8,
    pub bucket_anti_alias: bool,
    pub bucket_contiguous: bool,
    pub bucket_all_layers: bool,
    pub liquify: Liquify,
    /// Foreground and background colours, in sRGB as shown in the pickers.
    #[serde(skip)]
    pub foreground: [u8; 3],
    #[serde(skip)]
    pub background: [u8; 3],
    /// While a mask is targeted: the colours to go back to afterwards.
    #[serde(skip)]
    kept_colours: Option<([u8; 3], [u8; 3])>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            tool: Tool::Brush,
            last_marquee: Tool::Marquee,
            last_healing: Tool::SpotHealing,
            last_wand: Tool::MagicWand,
            last_gradient: Tool::Gradient,
            last_dodge: Tool::Dodge,
            before_crop: Tool::Marquee,
            held: None,
            kept_colours: None,
            brush: BrushSettings::default(),
            eraser: BrushSettings {
                hardness: 0.5,
                ..BrushSettings::default()
            },
            clone: BrushSettings {
                size: 60.0,
                hardness: 0.5,
                ..BrushSettings::default()
            },
            spot: BrushSettings {
                size: 30.0,
                hardness: 0.6,
                ..BrushSettings::default()
            },
            heal: BrushSettings {
                size: 40.0,
                hardness: 0.7,
                ..BrushSettings::default()
            },
            quick: BrushSettings {
                size: 60.0,
                ..BrushSettings::default()
            },
            dodge: BrushSettings {
                opacity: 0.5,
                ..BrushSettings::default()
            },
            burn: BrushSettings {
                opacity: 0.5,
                ..BrushSettings::default()
            },
            sponge: BrushSettings {
                opacity: 0.5,
                ..BrushSettings::default()
            },
            dodge_range: ToneRange::Midtones,
            dodge_protect: true,
            burn_range: ToneRange::Midtones,
            burn_protect: true,
            sponge_mode: SpongeMode::Desaturate,
            sponge_vibrance: true,
            ai_threshold: 0.0,
            threshold_moved: None,
            source: None,
            offset: None,
            eyedropper_sample: Sample::All,
            clone_sample: Sample::All,
            heal_sample: Sample::All,
            spot_sample: Sample::All,
            wand_sample: Sample::All,
            sample_size: SampleSize::default(),
            wand_tolerance: 32,
            wand_contiguous: true,
            wand_anti_alias: true,
            gradient_type: omapix_engine::ops::GradientType::default(),
            gradient_colors: GradientColors::default(),
            gradient_reverse: false,
            gradient_opacity: 1.0,
            bucket_opacity: 1.0,
            bucket_tolerance: 32,
            bucket_anti_alias: true,
            bucket_contiguous: true,
            bucket_all_layers: false,
            liquify: Liquify::default(),
            foreground: [0, 0, 0],
            background: [255, 255, 255],
        }
    }
}

impl Tools {
    pub fn settings(&self) -> BrushSettings {
        match self.tool {
            Tool::Move
            | Tool::Brush
            | Tool::Marquee
            | Tool::EllipticalMarquee
            | Tool::Lasso
            | Tool::MagicWand
            | Tool::ObjectSelection
            | Tool::Eyedropper
            | Tool::Gradient
            | Tool::PaintBucket
            | Tool::Crop => self.brush,
            Tool::QuickSelection => self.quick,
            Tool::Eraser => self.eraser,
            Tool::CloneStamp => self.clone,
            Tool::SpotHealing => self.spot,
            Tool::Healing => self.heal,
            Tool::Dodge => self.dodge,
            Tool::Burn => self.burn,
            Tool::Sponge => self.sponge,
        }
    }

    /// Make the current tool's brush `brush`, from the brushes loaded.
    pub fn use_brush(&mut self, brush: &omapix_engine::brushes::Preset) {
        let settings = self.settings_mut();
        brush.apply(settings);
        settings.size = settings.size.clamp(MIN_SIZE, MAX_SIZE);
    }

    pub fn set_size(&mut self, size: f32) {
        self.settings_mut().size = size.clamp(MIN_SIZE, MAX_SIZE);
    }

    pub fn set_opacity(&mut self, opacity: f32) {
        if self.tool == Tool::Gradient {
            self.gradient_opacity = opacity.clamp(0.0, 1.0);
        } else if self.tool == Tool::PaintBucket {
            self.bucket_opacity = opacity.clamp(0.0, 1.0);
        } else {
            self.settings_mut().opacity = opacity.clamp(0.0, 1.0);
        }
    }

    fn settings_mut(&mut self) -> &mut BrushSettings {
        match self.tool {
            Tool::Move
            | Tool::Brush
            | Tool::Marquee
            | Tool::EllipticalMarquee
            | Tool::Lasso
            | Tool::MagicWand
            | Tool::ObjectSelection
            | Tool::Eyedropper
            | Tool::Gradient
            | Tool::PaintBucket
            | Tool::Crop => &mut self.brush,
            Tool::QuickSelection => &mut self.quick,
            Tool::Eraser => &mut self.eraser,
            Tool::CloneStamp => &mut self.clone,
            Tool::SpotHealing => &mut self.spot,
            Tool::Healing => &mut self.heal,
            Tool::Dodge => &mut self.dodge,
            Tool::Burn => &mut self.burn,
            Tool::Sponge => &mut self.sponge,
        }
    }

    /// Alt+click with a clone or heal tool: copy from here.
    pub fn set_source(&mut self, at: Pos2) {
        self.source = Some(at);
        self.offset = None;
    }

    /// Where to mark the clone source on the canvas: a fixed point until
    /// the first stroke, then an offset that follows the pointer.
    pub fn source_marker(&self) -> Option<crate::canvas::SourceMarker> {
        if !self.tool.copies() {
            return None;
        }
        match (self.offset, self.source) {
            (Some(offset), _) => Some(crate::canvas::SourceMarker::Offset(offset)),
            (None, Some(at)) => Some(crate::canvas::SourceMarker::Fixed(at)),
            (None, None) => None,
        }
    }

    /// What a stroke starting at `start` should do, given what it paints
    /// on. `None` for a clone or heal stroke with no source set yet, or on a
    /// mask (they only work on pixels).
    pub fn paint(&mut self, target: Target, profile: &ColorProfile, start: Pos2) -> Option<Paint> {
        if !self.tool.paints() {
            return None;
        }
        if self.tool == Tool::SpotHealing {
            return (target == Target::Pixels).then_some(Paint::SpotHeal);
        }
        if self.tool.copies() {
            if target == Target::Mask || target == Target::QuickMask {
                return None;
            }
            let offset = match self.offset {
                Some(offset) => offset,
                None => {
                    let offset = self.source? - start;
                    self.offset = Some(offset);
                    offset
                }
            };
            let (dx, dy) = (offset.x.round() as i32, offset.y.round() as i32);
            return Some(match self.tool {
                Tool::Healing => Paint::Heal { dx, dy },
                _ => Paint::Clone { dx, dy },
            });
        }
        if self.tool == Tool::Dodge {
            return Some(Paint::Tone {
                range: self.dodge_range,
                burn: false,
                protect: self.dodge_protect,
            });
        }
        if self.tool == Tool::Burn {
            return Some(Paint::Tone {
                range: self.burn_range,
                burn: true,
                protect: self.burn_protect,
            });
        }
        if self.tool == Tool::Sponge {
            if target == Target::Mask || target == Target::QuickMask {
                return None;
            }
            return Some(Paint::Sponge {
                saturate: self.sponge_mode == SpongeMode::Saturate,
                vibrance: self.sponge_vibrance,
            });
        }
        Some(match (self.tool, target) {
            // On a mask, the eraser reveals and the brush paints the
            // foreground colour's grey level, as in Photoshop.
            (Tool::Eraser, Target::Mask | Target::QuickMask) => Paint::Mask(u16::MAX),
            (Tool::Brush, Target::Mask | Target::QuickMask) => Paint::Mask(grey(self.foreground)),
            (Tool::Eraser, Target::Pixels) => Paint::Erase,
            (_, Target::Pixels) => Paint::Color(profile.from_srgb8(self.foreground).unwrap_or([0, 0, 0, u16::MAX])),
            (_, Target::Mask | Target::QuickMask) => Paint::Mask(grey(self.foreground)),
        })
    }

    /// Set the foreground or background colour from a document pixel (the eyedropper).
    pub fn sample(&mut self, pixel: Pixel, profile: &ColorProfile, background: bool) {
        if let Ok(transform) = DisplayTransform::to_srgb(profile) {
            let mut out = [[0u8; 4]];
            transform.convert(&[[pixel[0], pixel[1], pixel[2], u16::MAX]], &mut out);
            let srgb = [out[0][0], out[0][1], out[0][2]];
            if background {
                self.background = srgb;
            } else {
                self.foreground = srgb;
            }
        }
    }

    /// Change the active tool's brush size (diameter) and hardness by these
    /// amounts, within their limits (Alt+right-drag).
    pub fn drag_brush(&mut self, size: f32, hardness: f32) {
        let s = self.settings_mut();
        s.size = (s.size + size).clamp(MIN_SIZE, MAX_SIZE);
        s.hardness = (s.hardness + hardness).clamp(0.0, 1.0);
    }

    /// Select a tool, remembering it as the last-used tool in its group.
    pub fn select(&mut self, tool: Tool) {
        if self.tool != Tool::Crop {
            self.before_crop = self.tool;
        }
        self.tool = tool;
        self.sync_last_used();
    }

    /// Keep last-used tools in sync with the current tool.
    pub fn sync_last_used(&mut self) {
        match self.tool {
            Tool::Marquee | Tool::EllipticalMarquee => self.last_marquee = self.tool,
            Tool::SpotHealing | Tool::Healing => self.last_healing = self.tool,
            Tool::MagicWand | Tool::ObjectSelection | Tool::QuickSelection => self.last_wand = self.tool,
            Tool::Gradient | Tool::PaintBucket => self.last_gradient = self.tool,
            Tool::Dodge | Tool::Burn | Tool::Sponge => self.last_dodge = self.tool,
            _ => {}
        }
        if self.tool != Tool::Crop {
            self.before_crop = self.tool;
        }
    }

    /// Esc with the Crop tool: back to the tool before it.
    pub fn leave_crop(&mut self) {
        self.select(self.before_crop);
    }

    /// The tool to show or pick for a group (the tool last used from it).
    pub fn group_tool(&self, group: ToolGroup) -> Tool {
        if self.tool.group() == group {
            return self.tool;
        }
        match group {
            ToolGroup::Marquee => self.last_marquee,
            ToolGroup::Healing => self.last_healing,
            ToolGroup::Wand => self.last_wand,
            ToolGroup::Gradient => self.last_gradient,
            ToolGroup::Dodge => self.last_dodge,
            ToolGroup::Move => Tool::Move,
            ToolGroup::Lasso => Tool::Lasso,
            ToolGroup::Crop => Tool::Crop,
            ToolGroup::Eyedropper => Tool::Eyedropper,
            ToolGroup::Brush => Tool::Brush,
            ToolGroup::CloneStamp => Tool::CloneStamp,
            ToolGroup::Eraser => Tool::Eraser,
        }
    }

    /// Shift+letter cycles forward through a tool group.
    pub fn cycle_group(&mut self, group: ToolGroup) {
        let tools = group.tools();
        if tools.is_empty() {
            return;
        }
        let current = self.group_tool(group);
        let index = tools.iter().position(|&t| t == current).unwrap_or(0);
        let next = tools[(index + 1) % tools.len()];
        self.select(next);
    }

    /// Photoshop's single-key shortcuts. Call only when no text field has
    /// keyboard focus. Returns a layer opacity typed with the Move tool,
    /// where Photoshop's number keys set the layer's opacity instead of the
    /// brush's.
    pub fn keys(&mut self, ctx: &egui::Context) -> Option<f32> {
        self.sync_last_used();
        ctx.input_mut(|i| {
            let shift = Modifiers::SHIFT;
            let mut layer_opacity = None;
            for &group in ToolGroup::ALL {
                if let Some(key) = group.key() {
                    if i.consume_key(Modifiers::SHIFT, key) {
                        self.cycle_group(group);
                    }
                    if i.consume_key(Modifiers::NONE, key) {
                        let before = self.tool;
                        self.select(self.group_tool(group));
                        // Key repeats while held don't count as new presses.
                        if self.held.is_none_or(|(held, ..)| held != key) {
                            self.held = Some((key, before, i.time));
                        }
                    }
                }
            }
            // Photoshop's spring-loaded tools: a tool key held down (rather
            // than tapped) switches back to the tool before when let go.
            if let Some((key, before, since)) = self.held
                && !i.key_down(key)
            {
                self.held = None;
                if i.time - since > SPRING {
                    self.select(before);
                }
            }
            if i.consume_key(Modifiers::NONE, Key::X) {
                std::mem::swap(&mut self.foreground, &mut self.background);
            }
            if i.consume_key(Modifiers::NONE, Key::D) {
                // On a mask, white (reveal) over black, as in Photoshop.
                (self.foreground, self.background) = if self.kept_colours.is_some() {
                    ([255; 3], [0; 3])
                } else {
                    ([0; 3], [255; 3])
                };
            }
            // Shift+[ / Shift+] change hardness in 25 % steps; [ / ] change size.
            // Plain [ / ] and Shift+[ / ] only: Alt (layer navigation) and
            // Ctrl+Shift (bring to front / send to back) must not touch the brush.
            if !i.modifiers.alt && !i.modifiers.command && !i.modifiers.ctrl && !i.modifiers.mac_cmd {
                if i.consume_key(shift, Key::OpenBracket) {
                    let s = self.settings_mut();
                    s.hardness = ((s.hardness - 0.25) * 4.0).round().max(0.0) / 4.0;
                }
                if i.consume_key(shift, Key::CloseBracket) {
                    let s = self.settings_mut();
                    s.hardness = ((s.hardness + 0.25) * 4.0).round().min(4.0) / 4.0;
                }
                if i.consume_key(Modifiers::NONE, Key::OpenBracket) {
                    let s = self.settings_mut();
                    s.size = step_size(s.size, false);
                }
                if i.consume_key(Modifiers::NONE, Key::CloseBracket) {
                    let s = self.settings_mut();
                    s.size = step_size(s.size, true);
                }
            }
            // 1–9 set opacity to 10–90 %, 0 to 100 %.
            let digits = [
                Key::Num0,
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
                Key::Num7,
                Key::Num8,
                Key::Num9,
            ];
            for (n, key) in digits.into_iter().enumerate() {
                if i.consume_key(Modifiers::NONE, key) {
                    let opacity = if n == 0 { 1.0 } else { n as f32 / 10.0 };
                    match self.tool {
                        Tool::Move => layer_opacity = Some(opacity),
                        Tool::Gradient => self.gradient_opacity = opacity,
                        Tool::PaintBucket => self.bucket_opacity = opacity,
                        _ => self.settings_mut().opacity = opacity,
                    }
                }
            }
            layer_opacity
        })
    }

    /// Arrow keys with the Move tool nudge by a pixel, or 10 with Shift, as
    /// in Photoshop. Returns this frame's nudge, consuming the keys. Call
    /// only when no text field has keyboard focus.
    pub fn nudge(&self, ctx: &egui::Context) -> Option<(i32, i32)> {
        if self.tool != Tool::Move {
            return None;
        }
        ctx.input_mut(|i| {
            let mut total = (0, 0);
            for (key, (dx, dy)) in [
                (Key::ArrowLeft, (-1, 0)),
                (Key::ArrowRight, (1, 0)),
                (Key::ArrowUp, (0, -1)),
                (Key::ArrowDown, (0, 1)),
            ] {
                for (modifiers, step) in [(Modifiers::SHIFT, 10), (Modifiers::NONE, 1)] {
                    let n = i.count_and_consume_key(modifiers, key) as i32 * step;
                    total = (total.0 + dx * n, total.1 + dy * n);
                }
            }
            (total != (0, 0)).then_some(total)
        })
    }

    /// Where the current tool samples pixels from.
    pub fn sample_from(&self) -> Sample {
        match self.tool {
            Tool::CloneStamp => self.clone_sample,
            Tool::Healing => self.heal_sample,
            Tool::SpotHealing => self.spot_sample,
            Tool::MagicWand => self.wand_sample,
            _ => self.eyedropper_sample,
        }
    }

    fn sample_from_mut(&mut self) -> &mut Sample {
        match self.tool {
            Tool::CloneStamp => &mut self.clone_sample,
            Tool::Healing => &mut self.heal_sample,
            Tool::SpotHealing => &mut self.spot_sample,
            Tool::MagicWand => &mut self.wand_sample,
            _ => &mut self.eyedropper_sample,
        }
    }

    fn sample_options(&mut self, ui: &mut Ui) {
        ui.label("Sample");
        let current = self.sample_from_mut();
        for sample in Sample::ALL {
            ui.selectable_value(current, sample, sample.label());
        }
    }

    /// The Brush Settings panel: the brushes loaded, to choose from, and
    /// the tip of the current tool's brush and how its dabs vary along a
    /// stroke, as in Photoshop's panel (F5). Gives back a command to run.
    pub fn brush_settings(&mut self, ui: &mut Ui, theme: &Theme, brushes: &mut crate::brushes::Library) -> Option<Command> {
        // Tools without a brush show the Brush tool's.
        let name = if self.tool.has_brush() { self.tool.name() } else { Tool::Brush.name() };
        let mut command = None;
        let settings = self.settings_mut();
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).color(theme.dark_foreground));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let plain = settings.dynamics == Dynamics::default() && settings.tip.is_none();
                if ui.add_enabled(!plain, Button::new("Reset")).on_hover_text("A plain round brush").clicked() {
                    (settings.dynamics, settings.tip) = (Dynamics::default(), None);
                }
                if ui.button("Load…").on_hover_text("Brushes from a Photoshop brush file (.abr)").clicked() {
                    command = Some(Command::ImportBrushes);
                }
            });
        });
        let current = *settings;
        if let Some(brush) = brushes.show(ui, theme, &current) {
            self.use_brush(&brush);
        }
        brushes.preview(ui, theme, &self.settings_mut().clone());
        let (patterns, tips) = (brushes.patterns(), brushes.tips());
        // Color Dynamics is the Brush's: the other tools don't paint a colour.
        let paints_colour = !self.tool.has_brush() || self.tool == Tool::Brush;
        let size = self.settings_mut().size;
        let d = &mut self.settings_mut().dynamics;
        let heading = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).color(theme.foreground).strong());
            ui.end_row();
        };
        // A slider over `range`, showing `value` times `scale`.
        let row = |ui: &mut Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, scale: f32, suffix: &str, hint: &str| {
            ui.label(label).on_hover_text(hint);
            let mut shown = *value * scale;
            let slider = Slider::new(&mut shown, range).suffix(suffix).fixed_decimals(0);
            if ui.add(slider).on_hover_text(hint).changed() {
                *value = shown / scale;
            }
            ui.end_row();
        };
        let percent = |ui: &mut Ui, label: &str, value: &mut f32, hint: &str| row(ui, label, value, 0.0..=100.0, 100.0, "%", hint);
        // Two tick boxes, one under the names and one under the sliders.
        let pair = |ui: &mut Ui, a: (&mut bool, &str, &str), b: (&mut bool, &str, &str)| {
            ui.checkbox(a.0, a.1).on_hover_text(a.2);
            ui.checkbox(b.0, b.1).on_hover_text(b.2);
            ui.end_row();
        };
        // All of it if there's room, leaving the layers some.
        let height = (ui.available_height() - 220.0).max(120.0);
        egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
            // Narrow enough for the panel at the width it starts with.
            ui.spacing_mut().slider_width = 70.0;
            egui::Grid::new("brush-settings").num_columns(2).show(ui, |ui| {
                heading(ui, "Brush Tip Shape");
                row(
                    ui,
                    "Spacing",
                    &mut d.spacing,
                    1.0..=300.0,
                    100.0,
                    "%",
                    "The distance between dabs, as a share of the brush's size",
                );
                row(
                    ui,
                    "Angle",
                    &mut d.angle,
                    -180.0..=180.0,
                    1.0,
                    "°",
                    "Turns a tip that isn't round, anticlockwise",
                );
                row(
                    ui,
                    "Roundness",
                    &mut d.roundness,
                    1.0..=100.0,
                    100.0,
                    "%",
                    "The tip's height as a share of its width",
                );
                let (x, y) = (
                    "A tip made from an image, mirrored left to right",
                    "A tip made from an image, mirrored top to bottom",
                );
                pair(ui, (&mut d.flip_x, "Flip X", x), (&mut d.flip_y, "Flip Y", y));

                heading(ui, "Shape Dynamics");
                percent(ui, "Size Jitter", &mut d.size_jitter, "How much smaller a dab can be at random");
                percent(
                    ui,
                    "Minimum Diameter",
                    &mut d.minimum_diameter,
                    "The smallest a dab gets, from jitter or a pen's pressure",
                );
                percent(ui, "Angle Jitter", &mut d.angle_jitter, "How far a dab can turn at random");
                ui.label("");
                ui.checkbox(&mut d.angle_follows, "Follow the stroke")
                    .on_hover_text("The tip turns the way the stroke goes (Photoshop's Control: Direction)");
                ui.end_row();
                ui.label("Follow the pen")
                    .on_hover_text("The tip turns the way a pen leans, or as its barrel turns (Photoshop's Control: Pen Tilt, Rotation)");
                let names = |a: PenAngle| match a {
                    PenAngle::Neither => "Neither",
                    PenAngle::Tilt => "Pen Tilt",
                    PenAngle::Rotation => "Rotation",
                };
                ComboBox::from_id_salt("angle-pen").selected_text(names(d.angle_pen)).show_ui(ui, |ui| {
                    for a in [PenAngle::Neither, PenAngle::Tilt, PenAngle::Rotation] {
                        ui.selectable_value(&mut d.angle_pen, a, names(a));
                    }
                });
                ui.end_row();
                percent(ui, "Roundness Jitter", &mut d.roundness_jitter, "How much flatter a dab can be at random");
                percent(ui, "Minimum Roundness", &mut d.minimum_roundness, "The flattest that makes it");
                let (x, y) = (
                    "Each dab mirrored left to right or not, at random",
                    "Each dab mirrored top to bottom or not, at random",
                );
                pair(ui, (&mut d.flip_x_jitter, "Flip X Jitter", x), (&mut d.flip_y_jitter, "Flip Y Jitter", y));

                heading(ui, "Scattering");
                row(
                    ui,
                    "Scatter",
                    &mut d.scatter,
                    0.0..=1000.0,
                    100.0,
                    "%",
                    "How far dabs stray from the stroke, as a share of the brush's radius",
                );
                ui.label("");
                ui.checkbox(&mut d.both_axes, "Both Axes")
                    .on_hover_text("Stray along the stroke as well as across it");
                ui.end_row();
                ui.label("Count").on_hover_text("Dabs at each step");
                ui.add(Slider::new(&mut d.count, 1..=MAX_COUNT)).on_hover_text("Dabs at each step");
                ui.end_row();
                percent(ui, "Count Jitter", &mut d.count_jitter, "How many fewer there can be at random");

                heading(ui, "Transfer");
                percent(ui, "Opacity Jitter", &mut d.opacity_jitter, "How much lower a dab's opacity can be at random");
                percent(ui, "Flow Jitter", &mut d.flow_jitter, "How much lower a dab's flow can be at random");

                heading(ui, "Texture");
                let mut textured = d.texture.is_some();
                ui.label("");
                let toggle = ui
                    .add_enabled(textured || !patterns.is_empty(), egui::Checkbox::new(&mut textured, "Use a pattern"))
                    .on_hover_text("A pattern worked into the paint")
                    .on_disabled_hover_text("Load a brush file with patterns to have some to choose from");
                if toggle.changed() {
                    d.texture = textured.then(|| Texture {
                        pattern: patterns[0].id(),
                        ..Texture::default()
                    });
                }
                ui.end_row();
                if let Some(t) = &mut d.texture {
                    ui.label("Pattern").on_hover_text("From the brush files loaded");
                    let name = patterns.iter().find(|p| p.id() == t.pattern).map_or("(not loaded)", |p| p.name.as_str());
                    ComboBox::from_id_salt("texture-pattern").selected_text(name).show_ui(ui, |ui| {
                        for p in &patterns {
                            ui.selectable_value(&mut t.pattern, p.id(), &p.name);
                        }
                    });
                    ui.end_row();
                    row(ui, "Scale", &mut t.scale, 1.0..=1000.0, 100.0, "%", "The pattern's size");
                    row(
                        ui,
                        "Brightness",
                        &mut t.brightness,
                        -150.0..=150.0,
                        150.0,
                        "",
                        "Lightens or darkens the pattern",
                    );
                    // Photoshop's -50 to 0 and 0 to 100, scaled apart.
                    let hint = "Steepens the pattern's greys towards black and white, or flattens them";
                    ui.label("Contrast").on_hover_text(hint);
                    let mut shown = if t.contrast < 0.0 { t.contrast * 50.0 } else { t.contrast * 100.0 };
                    if ui.add(Slider::new(&mut shown, -50.0..=100.0).fixed_decimals(0)).on_hover_text(hint).changed() {
                        t.contrast = if shown < 0.0 { shown / 50.0 } else { shown / 100.0 };
                    }
                    ui.end_row();
                    let (invert, each) = (
                        "Swaps the pattern's light and dark",
                        "Into each dab, so Depth can vary from dab to dab, rather than into the stroke as a whole",
                    );
                    pair(ui, (&mut t.invert, "Invert", invert), (&mut t.each_tip, "Texture Each Tip", each));
                    ui.label("Mode").on_hover_text("How the pattern meets the paint");
                    ComboBox::from_id_salt("texture-mode").selected_text(t.mode.label()).show_ui(ui, |ui| {
                        for mode in Mode::ALL {
                            ui.selectable_value(&mut t.mode, mode, mode.label());
                        }
                    });
                    ui.end_row();
                    percent(ui, "Depth", &mut t.depth, "How strongly the pattern shows");
                    if t.each_tip {
                        percent(ui, "Minimum Depth", &mut t.minimum_depth, "The least Depth Jitter takes it to");
                        percent(ui, "Depth Jitter", &mut t.depth_jitter, "How much lower each dab's depth can be at random");
                    }
                }

                heading(ui, "Dual Brush");
                let mut doubled = d.dual.is_some();
                ui.label("");
                ui.checkbox(&mut doubled, "Use a second tip")
                    .on_hover_text("A second tip laid along the stroke, and the paint only where the two meet");
                ui.end_row();
                if doubled != d.dual.is_some() {
                    d.dual = doubled.then(DualBrush::default);
                }
                if let Some(dual) = &mut d.dual {
                    ui.label("Mode").on_hover_text("How the second tip meets the paint");
                    ComboBox::from_id_salt("dual-mode").selected_text(dual.mode.label()).show_ui(ui, |ui| {
                        for mode in Mode::ALL {
                            ui.selectable_value(&mut dual.mode, mode, mode.label());
                        }
                    });
                    ui.end_row();
                    ui.label("Tip").on_hover_text("Round, or a tip from the brush files loaded");
                    let name = dual.tip.map_or("Round", |id| {
                        tips.iter().find(|(_, t)| t.id() == id).map_or("(not loaded)", |(name, _)| name.as_str())
                    });
                    ComboBox::from_id_salt("dual-tip").selected_text(name).show_ui(ui, |ui| {
                        ui.selectable_value(&mut dual.tip, None, "Round");
                        for (name, tip) in &tips {
                            ui.selectable_value(&mut dual.tip, Some(tip.id()), name);
                        }
                    });
                    ui.end_row();
                    // Kept as a share of the brush's size, shown in pixels.
                    let hint = "The second tip's size; it keeps its proportion as the brush is made bigger or smaller";
                    ui.label("Size").on_hover_text(hint);
                    let mut px = dual.diameter(size);
                    if ui
                        .add(Slider::new(&mut px, 1.0..=5000.0).logarithmic(true).suffix(" px").fixed_decimals(0))
                        .on_hover_text(hint)
                        .changed()
                    {
                        dual.size = px / size.max(1.0);
                    }
                    ui.end_row();
                    if dual.tip.is_none() {
                        percent(ui, "Hardness", &mut dual.hardness, "How hard the round tip's edge is");
                    }
                    row(
                        ui,
                        "Spacing",
                        &mut dual.spacing,
                        1.0..=1000.0,
                        100.0,
                        "%",
                        "The distance between its dabs, as a share of its size",
                    );
                    row(
                        ui,
                        "Scatter",
                        &mut dual.scatter,
                        0.0..=1000.0,
                        100.0,
                        "%",
                        "How far its dabs stray, as a share of its radius",
                    );
                    let (both, flip) = (
                        "Stray along the stroke as well as across it",
                        "Each of its dabs mirrored left to right or not, at random",
                    );
                    pair(ui, (&mut dual.both_axes, "Both Axes", both), (&mut dual.flip, "Flip", flip));
                    ui.label("Count").on_hover_text("Its dabs at each step");
                    ui.add(Slider::new(&mut dual.count, 1..=MAX_COUNT)).on_hover_text("Its dabs at each step");
                    ui.end_row();
                }

                if paints_colour {
                    heading(ui, "Color Dynamics");
                    let mut varied = d.colour.is_some();
                    ui.label("");
                    ui.checkbox(&mut varied, "Vary the colour")
                        .on_hover_text("Each dab a colour between the foreground and background, its hue, saturation and brightness varied");
                    ui.end_row();
                    if varied != d.colour.is_some() {
                        d.colour = varied.then(ColorDynamics::default);
                    }
                    if let Some(c) = &mut d.colour {
                        ui.label("");
                        ui.checkbox(&mut c.per_tip, "Apply Per Tip")
                            .on_hover_text("Each dab its own colour; unticked, each stroke");
                        ui.end_row();
                        percent(
                            ui,
                            "Foreground/Background Jitter",
                            &mut c.foreground_background,
                            "How far towards the background colour it can go",
                        );
                        percent(ui, "Hue Jitter", &mut c.hue_jitter, "How far round the colour wheel it can turn");
                        percent(ui, "Saturation Jitter", &mut c.saturation_jitter, "How much more or less saturated it can be");
                        percent(ui, "Brightness Jitter", &mut c.brightness_jitter, "How much lighter or darker it can be");
                        row(
                            ui,
                            "Purity",
                            &mut c.purity,
                            -100.0..=100.0,
                            100.0,
                            "%",
                            "Makes every dab more saturated, or greyer",
                        );
                    }
                }

                heading(ui, "Brush Pose");
                let pose = d.pose.get_or_insert_default();
                // Each ticked stands in for the pen's, with a mouse too.
                let mut tilted = pose.tilt.is_some();
                ui.label("");
                ui.checkbox(&mut tilted, "Override Tilt").on_hover_text("Paint as if the pen leant this way");
                ui.end_row();
                if tilted != pose.tilt.is_some() {
                    pose.tilt = tilted.then_some([0.0; 2]);
                }
                if let Some(tilt) = &mut pose.tilt {
                    row(ui, "Tilt X", &mut tilt[0], -90.0..=90.0, 1.0, "°", "How far it leans to the right");
                    row(ui, "Tilt Y", &mut tilt[1], -90.0..=90.0, 1.0, "°", "How far it leans towards you");
                }
                let mut turned = pose.rotation.is_some();
                ui.label("");
                ui.checkbox(&mut turned, "Override Rotation")
                    .on_hover_text("Paint as if the pen's barrel were turned this far");
                ui.end_row();
                if turned != pose.rotation.is_some() {
                    pose.rotation = turned.then_some(0.0);
                }
                if let Some(rotation) = &mut pose.rotation {
                    row(ui, "Rotation", rotation, 0.0..=359.0, 1.0, "°", "How far the barrel is turned, clockwise");
                }
                let mut pressed = pose.pressure.is_some();
                ui.label("");
                ui.checkbox(&mut pressed, "Override Pressure")
                    .on_hover_text("Paint as if the pen pressed this hard, or as a mouse would");
                ui.end_row();
                if pressed != pose.pressure.is_some() {
                    pose.pressure = pressed.then_some(1.0);
                }
                if let Some(pressure) = &mut pose.pressure {
                    percent(ui, "Pressure", pressure, "How hard it presses");
                }
                if d.pose == Some(Pose::default()) {
                    d.pose = None;
                }

                heading(ui, "Other");
                let (noise, wet) = (
                    "Breaks up the soft parts of each dab into grain",
                    "Paint gathers along the edge of the stroke, as watercolour's does",
                );
                pair(ui, (&mut d.noise, "Noise", noise), (&mut d.wet_edges, "Wet Edges", wet));
            });
        });
        command
    }

    /// The options bar: settings for the current tool.
    pub fn options_bar(&mut self, ui: &mut Ui, target: Target, theme: &Theme) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.tool.name()).strong());
            ui.separator();
            if self.tool == Tool::Gradient {
                ui.label("Type");
                ui.selectable_value(&mut self.gradient_type, omapix_engine::ops::GradientType::Linear, "Linear");
                ui.selectable_value(&mut self.gradient_type, omapix_engine::ops::GradientType::Radial, "Radial");
                ui.separator();
                ui.label("Colours");
                ComboBox::from_id_salt("gradient-colors")
                    .selected_text(self.gradient_colors.label())
                    .show_ui(ui, |ui| {
                        for colors in GradientColors::ALL {
                            ui.selectable_value(&mut self.gradient_colors, colors, colors.label());
                        }
                    });
                ui.separator();
                ui.checkbox(&mut self.gradient_reverse, "Reverse");
                ui.separator();
                let percent = |ui: &mut Ui, label: &str, value: &mut f32| {
                    ui.label(label);
                    let mut p = *value * 100.0;
                    if ui.add(Slider::new(&mut p, 0.0..=100.0).suffix("%").fixed_decimals(0)).changed() {
                        *value = p / 100.0;
                    }
                };
                percent(ui, "Opacity", &mut self.gradient_opacity);
                ui.separator();
                let (text, colour) = match target {
                    Target::QuickMask => ("Gradient in Quick Mask", theme.accent),
                    Target::Mask => ("Gradient on layer mask", theme.accent),
                    Target::Pixels => ("Gradient on layer", theme.dark_foreground),
                };
                ui.label(RichText::new(text).color(colour));
                return;
            }
            if self.tool == Tool::PaintBucket {
                percent(ui, "Opacity", &mut self.bucket_opacity);
                ui.separator();
                ui.label("Tolerance");
                ui.add(egui::DragValue::new(&mut self.bucket_tolerance).range(0..=255).speed(1.0));
                ui.checkbox(&mut self.bucket_anti_alias, "Anti-alias");
                ui.checkbox(&mut self.bucket_contiguous, "Contiguous");
                ui.checkbox(&mut self.bucket_all_layers, "All Layers");
                ui.separator();
                let hint = "Click to fill similar colours with the foreground colour, within the selection";
                ui.label(RichText::new(hint).color(theme.dark_foreground));
                return;
            }
            if self.tool == Tool::Eyedropper {
                ui.label("Sample Size");
                ComboBox::from_id_salt("eyedropper-sample-size")
                    .selected_text(self.sample_size.label())
                    .show_ui(ui, |ui| {
                        for size in SampleSize::ALL {
                            ui.selectable_value(&mut self.sample_size, size, size.label());
                        }
                    });
                ui.separator();
                self.sample_options(ui);
                ui.separator();
                let hint = "Click or drag to sample foreground colour · Alt sets background";
                ui.label(RichText::new(hint).color(theme.dark_foreground));
                return;
            }
            if matches!(self.tool, Tool::ObjectSelection | Tool::QuickSelection) {
                if self.tool == Tool::QuickSelection {
                    ui.label("Size");
                    ui.add(egui::DragValue::new(&mut self.quick.size).range(MIN_SIZE..=MAX_SIZE).speed(1.0).suffix(" px"));
                    ui.separator();
                }
                ui.label("Threshold");
                let slider = ui
                    .add(Slider::new(&mut self.ai_threshold, -8.0..=8.0).fixed_decimals(1))
                    .on_hover_text("Lower selects more of what the model's unsure about, higher less");
                if slider.changed() {
                    self.threshold_moved = Some(!slider.dragged());
                } else if slider.drag_stopped() {
                    self.threshold_moved = Some(true);
                }
                ui.separator();
                let hint = if self.tool == Tool::QuickSelection {
                    "Paint over an object · paint again to add to it, Alt to take away · Shift adds another"
                } else {
                    "Click an object, or drag a box round it · Shift adds · Alt subtracts"
                };
                ui.label(RichText::new(hint).color(theme.dark_foreground));
                return;
            }
            if self.tool == Tool::MagicWand {
                ui.label("Tolerance");
                ui.add(egui::DragValue::new(&mut self.wand_tolerance).range(0..=255).speed(1.0));
                ui.checkbox(&mut self.wand_anti_alias, "Anti-alias");
                ui.checkbox(&mut self.wand_contiguous, "Contiguous");
                let mut sample_all = self.wand_sample == Sample::All;
                if ui.checkbox(&mut sample_all, "Sample All Layers").changed() {
                    self.wand_sample = if sample_all { Sample::All } else { Sample::Current };
                }
                ui.separator();
                let hint = "Click to select similar colours · Shift adds · Alt subtracts · Shift+Alt intersects";
                ui.label(RichText::new(hint).color(theme.dark_foreground));
                return;
            }
            if self.tool.selects() {
                let hint = "Drag to select · Shift adds · Alt subtracts · Shift+Alt intersects · \
                            click outside to deselect · Shift+F6 feathers";
                ui.label(RichText::new(hint).color(theme.dark_foreground));
                return;
            }
            if self.tool == Tool::Move {
                let hint = "Drag to move the layer, or the selected pixels · Alt+drag moves a \
                            copy · Shift constrains to 45° · arrow keys nudge (Shift: 10 px)";
                ui.label(RichText::new(hint).color(theme.dark_foreground));
                return;
            }
            if self.tool == Tool::Crop {
                let hint = "Drag the handles to resize the box (Shift keeps the proportions, Alt \
                            from the centre) · drag inside to move it · drag outside for a new \
                            one · Enter crops · Esc resets";
                ui.label(RichText::new(hint).color(theme.dark_foreground));
                return;
            }
            if matches!(self.tool, Tool::Dodge | Tool::Burn) {
                ui.label("Range");
                let is_dodge = self.tool == Tool::Dodge;
                let range = if is_dodge { &mut self.dodge_range } else { &mut self.burn_range };
                ComboBox::from_id_salt("tone-range").selected_text(range.label()).show_ui(ui, |ui| {
                    for r in ToneRange::ALL {
                        ui.selectable_value(range, r, r.label());
                    }
                });
                ui.separator();
                let s = self.settings_mut();
                percent(ui, "Exposure", &mut s.opacity);
                ui.toggle_value(&mut s.opacity_pressure, "✒")
                    .on_hover_text("A pen's pressure sets the exposure");
                ui.separator();
                let protect = if is_dodge { &mut self.dodge_protect } else { &mut self.burn_protect };
                ui.checkbox(protect, "Protect Tones");
                ui.separator();
                let s = self.settings_mut();
                ui.label("Size");
                ui.add(
                    egui::DragValue::new(&mut s.size)
                        .range(MIN_SIZE..=MAX_SIZE)
                        .speed(1.0)
                        .suffix(" px")
                        .fixed_decimals(0),
                );
                percent(ui, "Hardness", &mut s.hardness);
                ui.toggle_value(&mut s.size_pressure, "⊙").on_hover_text("A pen's pressure sets the size");
                ui.separator();
                let hint = if is_dodge {
                    "Drag to lighten · Alt burns · 1–9 set exposure"
                } else {
                    "Drag to darken · Alt dodges · 1–9 set exposure"
                };
                let (text, colour) = match target {
                    Target::QuickMask => (if is_dodge { "Dodging Quick Mask" } else { "Burning Quick Mask" }, theme.accent),
                    Target::Mask => (if is_dodge { "Dodging layer mask" } else { "Burning layer mask" }, theme.accent),
                    Target::Pixels => (hint, theme.dark_foreground),
                };
                ui.label(RichText::new(text).color(colour));
                return;
            }
            if self.tool == Tool::Sponge {
                ui.label("Mode");
                ComboBox::from_id_salt("sponge-mode").selected_text(self.sponge_mode.label()).show_ui(ui, |ui| {
                    for m in SpongeMode::ALL {
                        ui.selectable_value(&mut self.sponge_mode, m, m.label());
                    }
                });
                ui.separator();
                let s = self.settings_mut();
                percent(ui, "Flow", &mut s.opacity);
                ui.toggle_value(&mut s.opacity_pressure, "✒").on_hover_text("A pen's pressure sets the flow");
                ui.separator();
                ui.checkbox(&mut self.sponge_vibrance, "Vibrance");
                ui.separator();
                let s = self.settings_mut();
                ui.label("Size");
                ui.add(
                    egui::DragValue::new(&mut s.size)
                        .range(MIN_SIZE..=MAX_SIZE)
                        .speed(1.0)
                        .suffix(" px")
                        .fixed_decimals(0),
                );
                percent(ui, "Hardness", &mut s.hardness);
                ui.toggle_value(&mut s.size_pressure, "⊙").on_hover_text("A pen's pressure sets the size");
                ui.separator();
                let (text, colour) = match target {
                    Target::QuickMask | Target::Mask => ("Sponge works on layer colours, not masks", theme.red),
                    Target::Pixels => ("Drag to saturate or desaturate · Alt swaps mode · 1–9 set flow", theme.dark_foreground),
                };
                ui.label(RichText::new(text).color(colour));
                return;
            }
            let s = self.settings_mut();
            ui.label("Size");
            ui.add(
                egui::DragValue::new(&mut s.size)
                    .range(MIN_SIZE..=MAX_SIZE)
                    .speed(1.0)
                    .suffix(" px")
                    .fixed_decimals(0),
            );
            percent(ui, "Hardness", &mut s.hardness);
            percent(ui, "Opacity", &mut s.opacity);
            ui.toggle_value(&mut s.opacity_pressure, "✒").on_hover_text("A pen's pressure sets the opacity");
            percent(ui, "Flow", &mut s.flow);
            ui.toggle_value(&mut s.flow_pressure, "✒").on_hover_text("A pen's pressure sets the flow");
            ui.toggle_value(&mut s.size_pressure, "⊙").on_hover_text("A pen's pressure sets the size");
            percent(ui, "Smoothing", &mut s.smoothing);
            ui.toggle_value(&mut s.pulled_string, "〰")
                .on_hover_text("Pulled String Mode: the brush moves only when the pointer pulls the string taut (its length is the Smoothing)");
            ui.separator();
            if self.tool == Tool::SpotHealing {
                self.sample_options(ui);
                ui.separator();
                let (text, colour) = match target {
                    Target::QuickMask => ("Quick Mask paints selections, not blemishes", theme.red),
                    Target::Mask => ("Select the layer, not its mask, to heal", theme.red),
                    Target::Pixels => ("Paint over a blemish; it heals when you let go", theme.dark_foreground),
                };
                ui.label(RichText::new(text).color(colour));
            } else if self.tool.copies() {
                self.sample_options(ui);
                ui.separator();
                let (text, colour) = match (target, self.source) {
                    (Target::QuickMask, _) => ("Quick Mask paints selections, not cloned pixels", theme.red),
                    (Target::Mask, _) => ("Select the layer, not its mask, to clone or heal", theme.red),
                    (_, None) => ("Alt+click to set the source", theme.accent),
                    (_, Some(_)) => ("Alt+click to set a new source", theme.dark_foreground),
                };
                ui.label(RichText::new(text).color(colour));
            } else {
                let (text, colour) = match target {
                    Target::QuickMask => ("Painting in Quick Mask", theme.accent),
                    Target::Mask => ("Painting on layer mask", theme.accent),
                    Target::Pixels => ("Painting on layer", theme.dark_foreground),
                };
                ui.label(RichText::new(text).color(colour));
            }
        });
    }

    /// The toolbar: tools and the foreground/background colours.
    pub fn toolbar(&mut self, ui: &mut Ui, theme: &Theme) {
        self.sync_last_used();
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            for &group in ToolGroup::ALL {
                let current_tool = self.group_tool(group);
                let active = self.tool.group() == group;
                let colour = if active { theme.foreground } else { theme.dark_foreground };
                let icon = current_tool.icon();
                let tip = match current_tool.shortcut_letter() {
                    Some(letter) => format!("{} ({letter})", current_tool.name()),
                    None => current_tool.name().to_string(),
                };

                ui.push_id(group as usize, |ui| {
                    let button = Button::new(RichText::new(icon).size(18.0).color(colour))
                        .frame(false)
                        .min_size(Vec2::splat(28.0));
                    let response = ui.add(button).on_hover_text(tip);

                    if group.tools().len() > 1 {
                        let r = response.rect;
                        let p1 = Pos2::new(r.max.x - 3.0, r.max.y - 7.0);
                        let p2 = Pos2::new(r.max.x - 7.0, r.max.y - 3.0);
                        let p3 = Pos2::new(r.max.x - 3.0, r.max.y - 3.0);
                        ui.painter().add(egui::Shape::convex_polygon(vec![p1, p2, p3], colour, egui::Stroke::NONE));

                        let mut repaint_after = None;
                        let is_held = response.contains_pointer()
                            && ui.input(|i| {
                                if i.pointer.primary_down()
                                    && let Some(start) = i.pointer.press_start_time()
                                {
                                    let elapsed = i.time - start;
                                    if elapsed >= 0.35 {
                                        return true;
                                    }
                                    let remaining = (0.35 - elapsed).max(0.0);
                                    repaint_after = Some(std::time::Duration::from_secs_f64(remaining));
                                }
                                false
                            });
                        if let Some(d) = repaint_after {
                            ui.ctx().request_repaint_after(d);
                        }

                        let mut popup = egui::Popup::context_menu(&response);
                        if is_held || response.secondary_clicked() {
                            popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
                        }
                        let is_open = popup.is_open();
                        popup.show(|ui| {
                            for &tool in group.tools() {
                                let item_active = self.tool == tool;
                                let item_colour = if item_active { theme.foreground } else { theme.dark_foreground };
                                let item_text = RichText::new(format!("{}  {}", tool.icon(), tool.name())).color(item_colour);
                                let mut item_button = Button::new(item_text);
                                if let Some(letter) = tool.shortcut_letter() {
                                    item_button = item_button.shortcut_text(letter);
                                }
                                if ui.add(item_button).clicked() {
                                    self.select(tool);
                                    ui.close();
                                }
                            }
                        });

                        if response.clicked() && !is_open && !is_held {
                            self.select(current_tool);
                        }
                    } else if response.clicked() {
                        self.select(current_tool);
                    }
                });
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut self.foreground).on_hover_text("Foreground colour — X swaps, D resets");
            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut self.background).on_hover_text("Background colour");
        });
    }
}

impl Tools {
    /// Masks take greys, as in Photoshop: while one (or Quick Mask) is
    /// targeted, the colours show and paint as their grey levels, and any
    /// colour picked becomes grey. Going back to pixels brings the colours
    /// back.
    pub fn follow_target(&mut self, target: Target) {
        let masking = target != Target::Pixels;
        match self.kept_colours {
            None if masking => self.kept_colours = Some((self.foreground, self.background)),
            Some((foreground, background)) if !masking => {
                (self.foreground, self.background) = (foreground, background);
                self.kept_colours = None;
                return;
            }
            _ => {}
        }
        if masking {
            let level = |c| [(f32::from(grey(c)) / 257.0).round() as u8; 3];
            (self.foreground, self.background) = (level(self.foreground), level(self.background));
        }
    }
}

/// Grey level of an sRGB colour, as a mask value.
pub fn grey([r, g, b]: [u8; 3]) -> u16 {
    let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    (luma / 255.0 * 65535.0).round() as u16
}

/// Photoshop-style bracket-key size steps: finer for small brushes.
pub(crate) fn step_size(size: f32, bigger: bool) -> f32 {
    // Shrinking from a band's lower edge uses the band below (100 → 90).
    let basis = if bigger { size } else { size - 0.01 };
    let step = if basis < 10.0 {
        1.0
    } else if basis < 100.0 {
        10.0
    } else if basis < 200.0 {
        25.0
    } else if basis < 500.0 {
        50.0
    } else {
        100.0
    };
    let next = if bigger { size + step } else { size - step };
    // Snap to the step grid so repeated presses land on round numbers.
    ((next / step).round() * step).clamp(MIN_SIZE, MAX_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tool_keeps_its_own_sample() {
        let mut tools = Tools::default();
        tools.select(Tool::SpotHealing);
        *tools.sample_from_mut() = Sample::Current;
        tools.select(Tool::Eyedropper);
        *tools.sample_from_mut() = Sample::CurrentAndBelow;
        tools.select(Tool::SpotHealing);
        assert_eq!(tools.sample_from(), Sample::Current);
        // The Brush's Alt+click samples as the Eyedropper does.
        tools.select(Tool::Brush);
        assert_eq!(tools.sample_from(), Sample::CurrentAndBelow);
        tools.select(Tool::CloneStamp);
        assert_eq!(tools.sample_from(), Sample::All);
    }

    #[test]
    fn clone_offset_is_fixed_by_the_first_stroke_and_kept() {
        let mut tools = Tools {
            tool: Tool::CloneStamp,
            ..Tools::default()
        };
        let srgb = ColorProfile::srgb();
        assert_eq!(tools.paint(Target::Pixels, &srgb, Pos2::new(50.0, 50.0)), None);
        tools.set_source(Pos2::new(10.0, 20.0));
        let first = tools.paint(Target::Pixels, &srgb, Pos2::new(50.0, 50.0));
        assert_eq!(first, Some(Paint::Clone { dx: -40, dy: -30 }));
        // A later stroke elsewhere keeps the same offset (aligned).
        let second = tools.paint(Target::Pixels, &srgb, Pos2::new(300.0, 10.0));
        assert_eq!(second, first);
        assert_eq!(tools.paint(Target::Mask, &srgb, Pos2::ZERO), None);
    }

    #[test]
    fn bracket_steps_round_and_clamp() {
        assert_eq!(step_size(100.0, true), 125.0);
        assert_eq!(step_size(100.0, false), 90.0);
        assert_eq!(step_size(3.0, false), 2.0);
        assert_eq!(step_size(1.0, false), 1.0);
        assert_eq!(step_size(5000.0, true), 5000.0);
    }

    #[test]
    fn brackets_with_alt_or_ctrl_do_not_change_brush() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();
        let initial_size = tools.settings().size;
        let initial_hardness = tools.settings().hardness;

        // Alt+[ and Alt+] (layer navigation) do not change brush size or hardness.
        press(&ctx, &[(Key::OpenBracket, Modifiers::ALT)]);
        tools.keys(&ctx);
        press(&ctx, &[(Key::CloseBracket, Modifiers::ALT)]);
        tools.keys(&ctx);
        assert_eq!(tools.settings().size, initial_size);
        assert_eq!(tools.settings().hardness, initial_hardness);

        // Ctrl+Shift+[ and Ctrl+Shift+] (bring to front / send to back) do not change brush size or hardness.
        let cmd_shift = Modifiers {
            shift: true,
            ..Modifiers::COMMAND
        };
        press(&ctx, &[(Key::OpenBracket, cmd_shift)]);
        tools.keys(&ctx);
        press(&ctx, &[(Key::CloseBracket, cmd_shift)]);
        tools.keys(&ctx);
        assert_eq!(tools.settings().size, initial_size);
        assert_eq!(tools.settings().hardness, initial_hardness);

        // Plain [ and ] DO change size.
        press(&ctx, &[(Key::CloseBracket, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_ne!(tools.settings().size, initial_size);

        // Shift+[ and Shift+] DO change hardness (CloseBracket increases from 0.0).
        press(&ctx, &[(Key::CloseBracket, Modifiers::SHIFT)]);
        tools.keys(&ctx);
        assert_ne!(tools.settings().hardness, initial_hardness);
    }

    #[test]
    fn dragging_changes_the_active_tools_brush_within_limits() {
        let mut tools = Tools::default();
        tools.select(Tool::Eraser);
        let (size, brush) = (tools.settings().size, tools.brush);
        tools.drag_brush(40.0, -2.0);
        tools.drag_brush(0.0, 0.25);
        assert_eq!(tools.settings().size, size + 40.0);
        assert_eq!(tools.settings().hardness, 0.25);
        assert_eq!(tools.brush, brush, "only the eraser's brush");
        tools.drag_brush(-1e6, 5.0);
        assert_eq!(tools.settings().size, MIN_SIZE);
        assert_eq!(tools.settings().hardness, 1.0);
    }

    #[test]
    fn mask_paint_follows_foreground_grey() {
        let mut tools = Tools::default();
        let srgb = ColorProfile::srgb();
        let at = Pos2::ZERO;
        assert_eq!(tools.paint(Target::Mask, &srgb, at), Some(Paint::Mask(0)));
        let mut eraser = Tools {
            tool: Tool::Eraser,
            ..Tools::default()
        };
        assert_eq!(eraser.paint(Target::Mask, &srgb, at), Some(Paint::Mask(u16::MAX)));
        assert_eq!(eraser.paint(Target::Pixels, &srgb, at), Some(Paint::Erase));
    }

    #[test]
    fn toolbar_renders() {
        let mut tools = Tools::default();
        let theme = Theme::default();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            tools.toolbar(ui, &theme);
        });
        output.textures_delta.clear();
    }

    /// Run one frame with these key presses.
    fn press(ctx: &egui::Context, keys: &[(Key, Modifiers)]) {
        let mut raw = egui::RawInput::default();
        for &(key, modifiers) in keys {
            raw.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            });
        }
        let mut out = ctx.run_ui(raw, |_| {});
        out.textures_delta.clear();
    }

    #[test]
    fn v_selects_move_and_arrows_nudge() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();
        // Arrows do nothing with other tools.
        press(&ctx, &[(Key::ArrowLeft, Modifiers::NONE)]);
        assert_eq!(tools.nudge(&ctx), None);

        press(&ctx, &[(Key::V, Modifiers::NONE)]);
        assert_eq!(tools.keys(&ctx), None);
        assert_eq!(tools.tool, Tool::Move);
        // Number keys set the layer's opacity, not the brush's.
        press(&ctx, &[(Key::Num5, Modifiers::NONE)]);
        assert_eq!(tools.keys(&ctx), Some(0.5));
        assert_eq!(tools.settings().opacity, 1.0);
        assert_eq!(tools.paint(Target::Pixels, &ColorProfile::srgb(), Pos2::ZERO), None);

        press(
            &ctx,
            &[
                (Key::ArrowLeft, Modifiers::NONE),
                (Key::ArrowDown, Modifiers::SHIFT),
                (Key::ArrowLeft, Modifiers::NONE),
            ],
        );
        assert_eq!(tools.nudge(&ctx), Some((-2, 10)));
        // The keys were consumed.
        assert_eq!(tools.nudge(&ctx), None);
    }

    #[test]
    fn m_and_shift_m_switches_marquee_tools() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();

        // M selects Marquee.
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut out = ctx.run_ui(raw, |_| {});
        out.textures_delta.clear();
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Marquee);

        // Shift+M toggles to EllipticalMarquee.
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::SHIFT,
        });
        let mut out = ctx.run_ui(raw, |_| {});
        out.textures_delta.clear();
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::EllipticalMarquee);

        // Shift+M toggles back to Marquee.
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::SHIFT,
        });
        let mut out = ctx.run_ui(raw, |_| {});
        out.textures_delta.clear();
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Marquee);
    }

    #[test]
    fn w_selects_magic_wand() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();
        press(&ctx, &[(Key::W, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::MagicWand);
    }

    #[test]
    fn masks_take_the_colours_as_greys_and_give_them_back_after() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();
        (tools.foreground, tools.background) = ([255, 0, 0], [0, 0, 255]);
        tools.follow_target(Target::Mask);
        assert_eq!((tools.foreground, tools.background), ([76; 3], [29; 3]));
        // A colour picked on the mask turns grey too.
        tools.foreground = [0, 255, 0];
        tools.follow_target(Target::Mask);
        assert_eq!(tools.foreground, [150; 3]);
        // D: white over black on a mask.
        press(&ctx, &[(Key::D, Modifiers::NONE)]);
        tools.keys(&ctx);
        tools.follow_target(Target::Mask);
        assert_eq!((tools.foreground, tools.background), ([255; 3], [0; 3]));
        // Back on the pixels, the colours from before.
        tools.follow_target(Target::Pixels);
        assert_eq!((tools.foreground, tools.background), ([255, 0, 0], [0, 0, 255]));
        tools.follow_target(Target::QuickMask);
        assert_eq!(tools.foreground, [76; 3]);
    }

    #[test]
    fn a_tool_key_held_down_switches_back_when_let_go() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();
        let key = |pressed: bool, key: Key, time: f64, tools: &mut Tools| {
            let raw = egui::RawInput {
                time: Some(time),
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |_| {});
            out.textures_delta.clear();
            tools.keys(&ctx);
        };
        assert_eq!(tools.tool, Tool::Brush);
        // Held: the Eyedropper while I is down, then the Brush again.
        key(true, Key::I, 1.0, &mut tools);
        assert_eq!(tools.tool, Tool::Eyedropper);
        key(false, Key::I, 2.0, &mut tools);
        assert_eq!(tools.tool, Tool::Brush);
        // Tapped: it stays.
        key(true, Key::I, 3.0, &mut tools);
        key(false, Key::I, 3.1, &mut tools);
        assert_eq!(tools.tool, Tool::Eyedropper);
    }

    #[test]
    fn g_selects_gradient_and_number_keys_set_opacity() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();
        press(&ctx, &[(Key::G, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Gradient);

        // Number keys set gradient opacity
        press(&ctx, &[(Key::Num5, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.gradient_opacity, 0.5);

        press(&ctx, &[(Key::Num0, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.gradient_opacity, 1.0);
    }

    #[test]
    fn g_and_shift_g_cycle_gradient_and_paint_bucket_and_number_keys_set_opacity() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();
        press(&ctx, &[(Key::G, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Gradient);

        // Shift+G cycles to Paint Bucket
        press(&ctx, &[(Key::G, Modifiers::SHIFT)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::PaintBucket);

        // Number keys set bucket opacity
        press(&ctx, &[(Key::Num4, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.bucket_opacity, 0.4);

        press(&ctx, &[(Key::Num0, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.bucket_opacity, 1.0);

        // Shift+G cycles back to Gradient
        press(&ctx, &[(Key::G, Modifiers::SHIFT)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Gradient);

        // Switching away and back with G remembers the last used tool in the group
        press(&ctx, &[(Key::G, Modifiers::SHIFT)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::PaintBucket);

        press(&ctx, &[(Key::B, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Brush);

        press(&ctx, &[(Key::G, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::PaintBucket);
    }

    #[test]
    fn groups_remember_last_used_tool_when_switching() {
        let mut tools = Tools::default();
        let ctx = egui::Context::default();

        // M selects Marquee.
        press(&ctx, &[(Key::M, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Marquee);

        // Shift+M switches to EllipticalMarquee.
        press(&ctx, &[(Key::M, Modifiers::SHIFT)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::EllipticalMarquee);

        // Switch away to Brush.
        press(&ctx, &[(Key::B, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Brush);

        // Plain M switches back to the last-used Marquee tool (EllipticalMarquee).
        press(&ctx, &[(Key::M, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::EllipticalMarquee);

        // Plain J switches to SpotHealing.
        press(&ctx, &[(Key::J, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::SpotHealing);

        // Shift+J switches to Healing.
        press(&ctx, &[(Key::J, Modifiers::SHIFT)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Healing);

        // Switch to Move.
        press(&ctx, &[(Key::V, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Move);

        // Plain J restores Healing.
        press(&ctx, &[(Key::J, Modifiers::NONE)]);
        tools.keys(&ctx);
        assert_eq!(tools.tool, Tool::Healing);
    }

    #[test]
    fn toolbar_groups_render_and_can_be_selected() {
        let mut tools = Tools::default();
        let theme = Theme::default();
        let ctx = egui::Context::default();

        // Default tool is Brush.
        assert_eq!(tools.tool, Tool::Brush);
        assert_eq!(tools.group_tool(ToolGroup::Marquee), Tool::Marquee);
        assert_eq!(tools.group_tool(ToolGroup::Healing), Tool::SpotHealing);

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            tools.toolbar(ui, &theme);
        });
        output.textures_delta.clear();

        // When a tool changes, the group's last-used tool updates.
        tools.select(Tool::EllipticalMarquee);
        assert_eq!(tools.group_tool(ToolGroup::Marquee), Tool::EllipticalMarquee);

        tools.select(Tool::Healing);
        assert_eq!(tools.group_tool(ToolGroup::Healing), Tool::Healing);

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            tools.toolbar(ui, &theme);
        });
        output.textures_delta.clear();
    }

    #[test]
    fn toolbar_right_click_and_hold_open_group_menu() {
        use egui::{Event, PointerButton};

        let mut tools = Tools::default();
        let theme = Theme::default();
        let ctx = egui::Context::default();

        // Render first frame to lay out the toolbar.
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 500.0))),
                ..Default::default()
            },
            |ui| {
                tools.toolbar(ui, &theme);
            },
        );
        out.textures_delta.clear();

        let _ = egui::Popup::is_any_open(&ctx);
        let center = Pos2::new(50.0, 51.0);

        // Secondary click opens the menu.
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 500.0))),
            events: vec![
                Event::PointerMoved(center),
                Event::PointerButton {
                    pos: center,
                    button: PointerButton::Secondary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
                Event::PointerButton {
                    pos: center,
                    button: PointerButton::Secondary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            tools.toolbar(ui, &theme);
        });
        out.textures_delta.clear();

        // Context menu popup for the marquee button should now be open.
        assert!(egui::Popup::is_any_open(&ctx));

        // Close the popup by clicking outside.
        let mut out = ctx.run_ui(
            egui::RawInput {
                events: vec![
                    Event::PointerMoved(Pos2::new(90.0, 490.0)),
                    Event::PointerButton {
                        pos: Pos2::new(90.0, 490.0),
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                    Event::PointerButton {
                        pos: Pos2::new(90.0, 490.0),
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| {
                tools.toolbar(ui, &theme);
            },
        );
        out.textures_delta.clear();
        assert!(!egui::Popup::is_any_open(&ctx));

        // Now test hold-to-open: press down at time 10.0, then advance time past 0.35s while holding.
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 500.0))),
                time: Some(10.0),
                events: vec![
                    Event::PointerMoved(center),
                    Event::PointerButton {
                        pos: center,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| {
                tools.toolbar(ui, &theme);
            },
        );
        out.textures_delta.clear();

        // Advance time to 10.4s (held for 0.4s) with pointer still at center.
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 500.0))),
                time: Some(10.4),
                events: vec![Event::PointerMoved(center)],
                ..Default::default()
            },
            |ui| {
                tools.toolbar(ui, &theme);
            },
        );
        out.textures_delta.clear();

        assert!(egui::Popup::is_any_open(&ctx));
    }

    #[test]
    fn toolbar_click_slot_selects_tool() {
        use egui::{Event, PointerButton};

        let mut tools = Tools::default();
        let theme = Theme::default();
        let ctx = egui::Context::default();
        let center = Pos2::new(50.0, 51.0); // Marquee button center

        // Layout frame
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 500.0))),
                time: Some(1.0),
                ..Default::default()
            },
            |ui| tools.toolbar(ui, &theme),
        );
        out.textures_delta.clear();

        // Press down
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 500.0))),
                time: Some(2.0),
                events: vec![
                    Event::PointerMoved(center),
                    Event::PointerButton {
                        pos: center,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| tools.toolbar(ui, &theme),
        );
        out.textures_delta.clear();

        // Release in next frame
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 500.0))),
                time: Some(2.05),
                events: vec![
                    Event::PointerMoved(center),
                    Event::PointerButton {
                        pos: center,
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| tools.toolbar(ui, &theme),
        );
        out.textures_delta.clear();

        assert_eq!(tools.tool, Tool::Marquee);
    }

    #[test]
    fn cursor_modifier_badges_choice() {
        let none = Modifiers::NONE;
        let shift = Modifiers::SHIFT;
        let alt = Modifiers::ALT;
        let shift_alt = Modifiers {
            shift: true,
            alt: true,
            ..Modifiers::NONE
        };
        let ctrl = Modifiers::COMMAND;

        // Selection tools: +, -, ×, or None
        let selection_tools = [Tool::Marquee, Tool::EllipticalMarquee, Tool::Lasso, Tool::MagicWand];
        for tool in selection_tools {
            assert_eq!(cursor_badge(tool, none), None);
            assert_eq!(cursor_badge(tool, ctrl), None);
            assert_eq!(cursor_badge(tool, shift), Some(CursorBadge::Add));
            assert_eq!(cursor_badge(tool, alt), Some(CursorBadge::Subtract));
            assert_eq!(cursor_badge(tool, shift_alt), Some(CursorBadge::Intersect));
        }

        // Move tool: Copy badge when Alt is held, None otherwise
        assert_eq!(cursor_badge(Tool::Move, none), None);
        assert_eq!(cursor_badge(Tool::Move, shift), None);
        assert_eq!(cursor_badge(Tool::Move, ctrl), None);
        assert_eq!(cursor_badge(Tool::Move, alt), Some(CursorBadge::Copy));
        assert_eq!(cursor_badge(Tool::Move, shift_alt), Some(CursorBadge::Copy));

        // Painting/editing tools never show selection/move badges
        let other_tools = [
            Tool::Brush,
            Tool::Eraser,
            Tool::CloneStamp,
            Tool::SpotHealing,
            Tool::Healing,
            Tool::Dodge,
            Tool::Burn,
            Tool::Sponge,
        ];
        for tool in other_tools {
            assert_eq!(cursor_badge(tool, none), None);
            assert_eq!(cursor_badge(tool, shift), None);
            assert_eq!(cursor_badge(tool, ctrl), None);
            // Alt picks up a colour, except where it sets a clone source or toggles tone modes.
            let alt_badge = tool.alt_picks_colour().then_some(CursorBadge::Eyedropper);
            assert_eq!(cursor_badge(tool, alt), alt_badge);
            assert_eq!(cursor_badge(tool, shift_alt), alt_badge);
        }
    }

    #[test]
    fn test_toning_tools_cycle_and_paint() {
        let mut tools = Tools::default();
        let srgb = ColorProfile::srgb();
        assert_eq!(ToolGroup::Dodge.default_key(), Some(Key::O));
        assert_eq!(ToolGroup::Dodge.tools(), &[Tool::Dodge, Tool::Burn, Tool::Sponge]);

        // Select Dodge
        tools.select(Tool::Dodge);
        assert_eq!(tools.tool, Tool::Dodge);
        assert_eq!(tools.group_tool(ToolGroup::Dodge), Tool::Dodge);
        assert_eq!(
            tools.paint(Target::Pixels, &srgb, Pos2::ZERO),
            Some(Paint::Tone {
                range: ToneRange::Midtones,
                burn: false,
                protect: true,
            })
        );
        // On a mask, Dodge still works
        assert_eq!(
            tools.paint(Target::Mask, &srgb, Pos2::ZERO),
            Some(Paint::Tone {
                range: ToneRange::Midtones,
                burn: false,
                protect: true,
            })
        );

        // Cycle to Burn
        tools.cycle_group(ToolGroup::Dodge);
        assert_eq!(tools.tool, Tool::Burn);
        assert_eq!(
            tools.paint(Target::Pixels, &srgb, Pos2::ZERO),
            Some(Paint::Tone {
                range: ToneRange::Midtones,
                burn: true,
                protect: true,
            })
        );

        // Cycle to Sponge
        tools.cycle_group(ToolGroup::Dodge);
        assert_eq!(tools.tool, Tool::Sponge);
        assert_eq!(
            tools.paint(Target::Pixels, &srgb, Pos2::ZERO),
            Some(Paint::Sponge {
                saturate: false,
                vibrance: true,
            })
        );
        // Sponge is disabled on masks
        assert_eq!(tools.paint(Target::Mask, &srgb, Pos2::ZERO), None);
        assert_eq!(tools.paint(Target::QuickMask, &srgb, Pos2::ZERO), None);

        // Cycle wraps back to Dodge
        tools.cycle_group(ToolGroup::Dodge);
        assert_eq!(tools.tool, Tool::Dodge);

        // Number keys set exposure/opacity
        tools.set_opacity(0.7);
        assert_eq!(tools.settings().opacity, 0.7);
    }

    #[test]
    fn the_brush_settings_panel_changes_the_current_tools_brush_and_resets_it() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut tools = Tools {
            tool: Tool::Eraser,
            ..Tools::default()
        };
        let mut brushes = crate::brushes::Library::default();
        let mut time = 0.0;
        // Draw the panel, and give back where its texts are.
        let mut frame = |tools: &mut Tools, events: Vec<egui::Event>| -> Vec<(String, Pos2)> {
            time += 0.5;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(320.0, 2400.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                tools.brush_settings(ui, &theme, &mut brushes);
            });
            out.textures_delta.clear();
            fn texts(shape: &egui::Shape, found: &mut Vec<(String, Pos2)>) {
                match shape {
                    egui::Shape::Text(t) => found.push((t.galley.text().to_owned(), t.visual_bounding_rect().center())),
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| texts(s, found)),
                    _ => {}
                }
            }
            let mut found = Vec::new();
            out.shapes.iter().for_each(|s| texts(&s.shape, &mut found));
            found
        };
        let click = |pos: Pos2, pressed: bool| {
            let button = egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            vec![egui::Event::PointerMoved(pos), button]
        };
        let at = |drawn: &[(String, Pos2)], text: &str| drawn.iter().find(|(t, _)| t == text).unwrap_or_else(|| panic!("no {text}")).1;

        let drawn = frame(&mut tools, vec![]);
        // It says whose brush it is, and has every section.
        for text in [
            "Eraser",
            "Brush Tip Shape",
            "Shape Dynamics",
            "Scattering",
            "Transfer",
            "Spacing",
            "Flow Jitter",
        ] {
            at(&drawn, text);
        }
        // The Eraser doesn't paint a colour, so it has no Color Dynamics.
        assert!(!drawn.iter().any(|(t, _)| t == "Color Dynamics"));
        let follow = at(&drawn, "Follow the stroke");
        frame(&mut tools, click(follow, true));
        frame(&mut tools, click(follow, false));
        assert!(tools.eraser.dynamics.angle_follows);
        // The tick boxes in pairs: one of each pair.
        for text in ["Flip Y", "Flip X Jitter", "Wet Edges"] {
            let tick = at(&drawn, text);
            frame(&mut tools, click(tick, true));
            frame(&mut tools, click(tick, false));
        }
        let d = tools.eraser.dynamics;
        assert_eq!(
            (d.flip_x, d.flip_y, d.flip_x_jitter, d.flip_y_jitter, d.noise, d.wet_edges),
            (false, true, true, false, false, true)
        );
        assert_eq!(tools.brush.dynamics, Dynamics::default(), "the Brush tool's is its own");

        let drawn = frame(&mut tools, vec![]);
        let reset = at(&drawn, "Reset");
        frame(&mut tools, click(reset, true));
        frame(&mut tools, click(reset, false));
        assert_eq!(tools.eraser.dynamics, Dynamics::default());

        // A tool with no brush shows the Brush tool's.
        tools.tool = Tool::Move;
        at(&frame(&mut tools, vec![]), "Brush");
    }

    #[test]
    fn the_options_bar_has_smoothing_and_pulled_string_mode() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut tools = Tools::default();
        let frame = |tools: &mut Tools, events: Vec<egui::Event>| -> Vec<(String, Pos2)> {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 40.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| tools.options_bar(ui, Target::Pixels, &theme));
            out.textures_delta.clear();
            fn texts(shape: &egui::Shape, found: &mut Vec<(String, Pos2)>) {
                match shape {
                    egui::Shape::Text(t) => found.push((t.galley.text().to_owned(), t.visual_bounding_rect().center())),
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| texts(s, found)),
                    _ => {}
                }
            }
            let mut found = Vec::new();
            out.shapes.iter().for_each(|s| texts(&s.shape, &mut found));
            found
        };
        let drawn = frame(&mut tools, vec![]);
        assert!(drawn.iter().any(|(t, _)| t == "Smoothing"), "{drawn:?}");
        let string = drawn.iter().find(|(t, _)| t == "〰").unwrap().1;
        let button = |pressed| egui::Event::PointerButton {
            pos: string,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        frame(&mut tools, vec![egui::Event::PointerMoved(string), button(true)]);
        frame(&mut tools, vec![button(false)]);
        assert!(tools.brush.pulled_string);
    }

    #[test]
    fn texture_and_dual_brush_use_the_loaded_patterns_and_tips() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut brushes = crate::brushes::Library::default();
        let mut tools = Tools::default();
        let mut time = 0.0;
        let mut frame = |tools: &mut Tools, brushes: &mut crate::brushes::Library, events: Vec<egui::Event>| -> Vec<(String, Pos2)> {
            time += 0.5;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(320.0, 2400.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                tools.brush_settings(ui, &theme, brushes);
            });
            out.textures_delta.clear();
            fn texts(shape: &egui::Shape, found: &mut Vec<(String, Pos2)>) {
                match shape {
                    egui::Shape::Text(t) => found.push((t.galley.text().to_owned(), t.visual_bounding_rect().center())),
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| texts(s, found)),
                    _ => {}
                }
            }
            let mut found = Vec::new();
            out.shapes.iter().for_each(|s| texts(&s.shape, &mut found));
            found
        };
        let click = |pos: Pos2| {
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            [vec![egui::Event::PointerMoved(pos), button(true)], vec![button(false)]]
        };
        let at = |drawn: &[(String, Pos2)], text: &str| drawn.iter().find(|(t, _)| t == text).map(|(_, p)| *p);

        // With no patterns loaded there's nothing to use.
        let drawn = frame(&mut tools, &mut brushes, vec![]);
        for events in click(at(&drawn, "Use a pattern").unwrap()) {
            frame(&mut tools, &mut brushes, events);
        }
        assert_eq!(tools.brush.dynamics.texture, None);

        let dir = std::env::temp_dir().join(format!("omapix-brush-texture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Nature.abr"), crate::brushes::tests::brush_file()).unwrap();
        brushes.load(dir.clone(), None, &ctx);
        crate::brushes::tests::loaded(&mut brushes);
        std::fs::remove_dir_all(&dir).unwrap();
        let canvas = brushes.patterns()[0].id();

        let drawn = frame(&mut tools, &mut brushes, vec![]);
        assert!(at(&drawn, "Pattern").is_none());
        for events in click(at(&drawn, "Use a pattern").unwrap()) {
            frame(&mut tools, &mut brushes, events);
        }
        let texture = tools.brush.dynamics.texture.expect("a Texture");
        assert_eq!((texture.pattern, texture.each_tip), (canvas, false));
        let drawn = frame(&mut tools, &mut brushes, vec![]);
        assert!(at(&drawn, "Canvas").is_some() && at(&drawn, "Depth").is_some() && at(&drawn, "Depth Jitter").is_none());
        for events in click(at(&drawn, "Texture Each Tip").unwrap()) {
            frame(&mut tools, &mut brushes, events);
        }
        assert!(tools.brush.dynamics.texture.unwrap().each_tip);
        assert!(at(&frame(&mut tools, &mut brushes, vec![]), "Depth Jitter").is_some());
        assert_eq!(brushes.pattern(&tools.brush).map(|p| p.id()), Some(canvas));

        // Dual Brush: a round second tip to start with, then one of the
        // brushes' sampled tips, which a stroke is given.
        let drawn = frame(&mut tools, &mut brushes, vec![]);
        for events in click(at(&drawn, "Use a second tip").unwrap()) {
            frame(&mut tools, &mut brushes, events);
        }
        assert_eq!(tools.brush.dynamics.dual, Some(omapix_engine::dynamics::DualBrush::default()));
        let drawn = frame(&mut tools, &mut brushes, vec![]);
        assert!(at(&drawn, "Round").is_some() && at(&drawn, "Hardness").is_some());
        assert!(brushes.parts(&tools.brush).dual_tip.is_none());
        let (name, leaf) = brushes.tips()[0].clone();
        assert_eq!(name, "Leaf");
        tools.brush.dynamics.dual.as_mut().unwrap().tip = Some(leaf.id());
        let drawn = frame(&mut tools, &mut brushes, vec![]);
        assert!(at(&drawn, "Leaf").is_some() && at(&drawn, "Hardness").is_none());
        assert_eq!(brushes.parts(&tools.brush).dual_tip.map(|t| t.id()), Some(leaf.id()));

        // Color Dynamics, for the Brush.
        let drawn = frame(&mut tools, &mut brushes, vec![]);
        for events in click(at(&drawn, "Vary the colour").unwrap()) {
            frame(&mut tools, &mut brushes, events);
        }
        assert_eq!(tools.brush.dynamics.colour, Some(omapix_engine::dynamics::ColorDynamics::default()));
        let drawn = frame(&mut tools, &mut brushes, vec![]);
        assert!(at(&drawn, "Foreground/Background Jitter").is_some() && at(&drawn, "Purity").is_some());

        // Brush Pose: nothing until something's ticked, then that alone.
        assert_eq!(tools.brush.dynamics.pose, None);
        for events in click(at(&drawn, "Override Pressure").unwrap()) {
            frame(&mut tools, &mut brushes, events);
        }
        let pose = tools.brush.dynamics.pose.expect("a pose");
        assert_eq!((pose.pressure, pose.tilt, pose.rotation), (Some(1.0), None, None));
        let drawn = frame(&mut tools, &mut brushes, vec![]);
        assert!(at(&drawn, "Pressure").is_some() && at(&drawn, "Tilt X").is_none() && at(&drawn, "Neither").is_some());
    }

    #[test]
    fn clicking_a_loaded_brush_makes_it_the_current_tools() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let dir = std::env::temp_dir().join(format!("omapix-brush-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Nature.abr"), crate::brushes::tests::brush_file()).unwrap();
        let mut brushes = crate::brushes::Library::default();
        brushes.load(dir.clone(), None, &ctx);
        crate::brushes::tests::loaded(&mut brushes);
        std::fs::remove_dir_all(&dir).unwrap();

        let mut tools = Tools {
            tool: Tool::CloneStamp,
            ..Tools::default()
        };
        tools.clone.opacity = 0.5;
        let mut time = 0.0;
        // Draw the panel: where its pictures are, and the command it asks for.
        let mut frame = |tools: &mut Tools, brushes: &mut crate::brushes::Library, events: Vec<egui::Event>| -> (Vec<Pos2>, Option<Command>) {
            time += 0.5;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(320.0, 900.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut command = None;
            let mut out = ctx.run_ui(input, |ui| command = tools.brush_settings(ui, &theme, brushes));
            out.textures_delta.clear();
            fn pictures(shape: &egui::Shape, found: &mut Vec<Pos2>) {
                match shape {
                    egui::Shape::Rect(r) if r.fill_texture_id() != egui::TextureId::default() => found.push(r.rect.center()),
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| pictures(s, found)),
                    _ => {}
                }
            }
            let mut found = Vec::new();
            out.shapes.iter().for_each(|s| pictures(&s.shape, &mut found));
            (found, command)
        };
        let click = |pos: Pos2, pressed: bool| {
            let button = egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            vec![egui::Event::PointerMoved(pos), button]
        };
        // The file's two brushes, the second the one with a sampled tip,
        // and then the stroke's preview.
        let (pictures, command) = frame(&mut tools, &mut brushes, vec![]);
        assert_eq!((pictures.len(), command), (3, None));
        frame(&mut tools, &mut brushes, click(pictures[1], true));
        frame(&mut tools, &mut brushes, click(pictures[1], false));
        let leaf = tools.clone;
        assert!(leaf.tip.is_some() && brushes.tip(leaf.tip).is_some());
        assert_eq!((leaf.size, leaf.dynamics.spacing, leaf.size_pressure), (60.0, 0.25, false));
        assert_eq!(
            (leaf.opacity, tools.brush.tip),
            (0.5, None),
            "the tool's opacity, and the other tools' brushes, are left alone"
        );
        // The round one takes the tip away again, and brings its hardness.
        frame(&mut tools, &mut brushes, click(pictures[0], true));
        frame(&mut tools, &mut brushes, click(pictures[0], false));
        assert_eq!((tools.clone.tip, tools.clone.size, tools.clone.hardness), (None, 20.0, 0.5));
    }
}
