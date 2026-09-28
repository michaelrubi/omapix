//! Painting tools: the toolbar on the left, the options bar under the menus,
//! and Photoshop's single-key tool shortcuts.

use egui::{Button, ComboBox, Key, Modifiers, Pos2, RichText, Slider, Ui, Vec2};
use omapix_engine::brush::{BrushSettings, Paint};
use omapix_engine::selection::Combine;
use omapix_engine::{ColorProfile, DisplayTransform, Pixel};

use crate::editor::Target;
use crate::theme::Theme;

const MOVE_ICON: &str = "\u{f047}";
const BRUSH_ICON: &str = "\u{f1fc}";
const ERASER_ICON: &str = "\u{f12d}";
const CLONE_ICON: &str = "\u{f24d}";
const HEAL_ICON: &str = "\u{f0fa}";
const SPOT_ICON: &str = "\u{f462}";
const WAND_ICON: &str = "\u{f0d0}";
const OBJECT_ICON: &str = "\u{f05b}";
const EYEDROPPER_ICON: &str = "\u{f1fb}";
const MARQUEE_ICON: &str = "\u{f096}";
const ELLIPSE_ICON: &str = "\u{f10c}";
const LASSO_ICON: &str = "\u{f0c4}";

const MIN_SIZE: f32 = 1.0;
const MAX_SIZE: f32 = 5000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolGroup {
    Move,
    Marquee,
    Lasso,
    Wand,
    Eyedropper,
    Healing,
    Brush,
    CloneStamp,
    Eraser,
}

impl ToolGroup {
    pub const ALL: &[ToolGroup] = &[
        ToolGroup::Move,
        ToolGroup::Marquee,
        ToolGroup::Lasso,
        ToolGroup::Wand,
        ToolGroup::Eyedropper,
        ToolGroup::Healing,
        ToolGroup::Brush,
        ToolGroup::CloneStamp,
        ToolGroup::Eraser,
    ];

    pub fn tools(self) -> &'static [Tool] {
        match self {
            ToolGroup::Move => &[Tool::Move],
            ToolGroup::Marquee => &[Tool::Marquee, Tool::EllipticalMarquee],
            ToolGroup::Lasso => &[Tool::Lasso],
            ToolGroup::Wand => &[Tool::ObjectSelection, Tool::MagicWand],
            ToolGroup::Eyedropper => &[Tool::Eyedropper],
            ToolGroup::Healing => &[Tool::SpotHealing, Tool::Healing],
            ToolGroup::Brush => &[Tool::Brush],
            ToolGroup::CloneStamp => &[Tool::CloneStamp],
            ToolGroup::Eraser => &[Tool::Eraser],
        }
    }

    pub fn default_key(self) -> Option<Key> {
        let k = match self {
            ToolGroup::Move => Key::V,
            ToolGroup::Marquee => Key::M,
            ToolGroup::Lasso => Key::L,
            ToolGroup::Wand => Key::W,
            ToolGroup::Eyedropper => Key::I,
            ToolGroup::Healing => Key::J,
            ToolGroup::Brush => Key::B,
            ToolGroup::CloneStamp => Key::S,
            ToolGroup::Eraser => Key::E,
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
    Eyedropper,
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
            Tool::Eyedropper => "Eyedropper",
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
            Tool::Eyedropper => EYEDROPPER_ICON,
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
            Tool::MagicWand | Tool::ObjectSelection => ToolGroup::Wand,
            Tool::Eyedropper => ToolGroup::Eyedropper,
            Tool::SpotHealing | Tool::Healing => ToolGroup::Healing,
            Tool::Brush => ToolGroup::Brush,
            Tool::CloneStamp => ToolGroup::CloneStamp,
            Tool::Eraser => ToolGroup::Eraser,
        }
    }

    /// Tools that make selections rather than paint.
    pub fn selects(self) -> bool {
        matches!(
            self,
            Tool::Marquee | Tool::EllipticalMarquee | Tool::Lasso | Tool::MagicWand | Tool::ObjectSelection
        )
    }

    /// Tools that paint with a brush, and so show its outline.
    pub fn paints(self) -> bool {
        !self.selects() && !matches!(self, Tool::Move | Tool::Eyedropper)
    }

    /// Tools that copy pixels from a source point set with Alt+click.
    pub fn copies(self) -> bool {
        matches!(self, Tool::CloneStamp | Tool::Healing)
    }
}

/// Eyedropper sample size presets matching Photoshop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SampleSize {
    #[default]
    Point,
    ThreeByThree,
    FiveByFive,
    ElevenByEleven,
}

impl SampleSize {
    pub const ALL: [SampleSize; 4] = [
        SampleSize::Point,
        SampleSize::ThreeByThree,
        SampleSize::FiveByFive,
        SampleSize::ElevenByEleven,
    ];

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Sample {
    Current,
    CurrentAndBelow,
    #[default]
    All,
}

impl Sample {
    pub const ALL: [Sample; 3] = [
        Sample::Current,
        Sample::CurrentAndBelow,
        Sample::All,
    ];

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
    } else {
        None
    }
}

pub struct Tools {
    pub tool: Tool,
    last_marquee: Tool,
    last_healing: Tool,
    last_wand: Tool,
    brush: BrushSettings,
    eraser: BrushSettings,
    clone: BrushSettings,
    spot: BrushSettings,
    heal: BrushSettings,
    /// Where the clone/heal source was set with Alt+click, in image pixels.
    source: Option<Pos2>,
    /// Source minus destination, fixed by the first stroke after setting a
    /// source and kept for later strokes (Photoshop's "Aligned").
    offset: Option<Vec2>,
    /// Where to sample pixels from (Current Layer, Current & Below, All Layers).
    pub sample: Sample,
    /// Eyedropper sample size (Point, 3×3, 5×5, 11×11 average).
    pub sample_size: SampleSize,
    /// Magic Wand settings matching Photoshop.
    pub wand_tolerance: u8,
    pub wand_contiguous: bool,
    pub wand_anti_alias: bool,
    /// Foreground and background colours, in sRGB as shown in the pickers.
    pub foreground: [u8; 3],
    pub background: [u8; 3],
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            tool: Tool::Brush,
            last_marquee: Tool::Marquee,
            last_healing: Tool::SpotHealing,
            last_wand: Tool::MagicWand,
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
            source: None,
            offset: None,
            sample: Sample::All,
            sample_size: SampleSize::default(),
            wand_tolerance: 32,
            wand_contiguous: true,
            wand_anti_alias: true,
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
            | Tool::Eyedropper => self.brush,
            Tool::Eraser => self.eraser,
            Tool::CloneStamp => self.clone,
            Tool::SpotHealing => self.spot,
            Tool::Healing => self.heal,
        }
    }

    pub fn set_size(&mut self, size: f32) {
        self.settings_mut().size = size.clamp(MIN_SIZE, MAX_SIZE);
    }

    pub fn set_opacity(&mut self, opacity: f32) {
        self.settings_mut().opacity = opacity.clamp(0.0, 1.0);
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
            | Tool::Eyedropper => &mut self.brush,
            Tool::Eraser => &mut self.eraser,
            Tool::CloneStamp => &mut self.clone,
            Tool::SpotHealing => &mut self.spot,
            Tool::Healing => &mut self.heal,
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
        Some(match (self.tool, target) {
            // On a mask, the eraser reveals and the brush paints the
            // foreground colour's grey level, as in Photoshop.
            (Tool::Eraser, Target::Mask | Target::QuickMask) => Paint::Mask(u16::MAX),
            (Tool::Brush, Target::Mask | Target::QuickMask) => Paint::Mask(grey(self.foreground)),
            (Tool::Eraser, Target::Pixels) => Paint::Erase,
            (_, Target::Pixels) => {
                Paint::Color(
                    profile
                        .from_srgb8(self.foreground)
                        .unwrap_or([0, 0, 0, u16::MAX]),
                )
            }
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
        self.tool = tool;
        self.sync_last_used();
    }

    /// Keep last-used tools in sync with the current tool.
    pub fn sync_last_used(&mut self) {
        match self.tool {
            Tool::Marquee | Tool::EllipticalMarquee => self.last_marquee = self.tool,
            Tool::SpotHealing | Tool::Healing => self.last_healing = self.tool,
            Tool::MagicWand | Tool::ObjectSelection => self.last_wand = self.tool,
            _ => {}
        }
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
            ToolGroup::Move => Tool::Move,
            ToolGroup::Lasso => Tool::Lasso,
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
                        self.select(self.group_tool(group));
                    }
                }
            }
            if i.consume_key(Modifiers::NONE, Key::X) {
                std::mem::swap(&mut self.foreground, &mut self.background);
            }
            if i.consume_key(Modifiers::NONE, Key::D) {
                self.foreground = [0, 0, 0];
                self.background = [255, 255, 255];
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

    fn sample_options(&mut self, ui: &mut Ui) {
        ui.label("Sample");
        for sample in Sample::ALL {
            ui.selectable_value(&mut self.sample, sample, sample.label());
        }
    }

    /// The options bar: settings for the current tool.
    pub fn options_bar(&mut self, ui: &mut Ui, target: Target, theme: &Theme) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.tool.name()).strong());
            ui.separator();
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
            if self.tool == Tool::ObjectSelection {
                let hint = "Click an object, or drag a box round it · Shift adds · Alt subtracts";
                ui.label(RichText::new(hint).color(theme.dark_foreground));
                return;
            }
            if self.tool == Tool::MagicWand {
                ui.label("Tolerance");
                ui.add(
                    egui::DragValue::new(&mut self.wand_tolerance)
                        .range(0..=255)
                        .speed(1.0),
                );
                ui.checkbox(&mut self.wand_anti_alias, "Anti-alias");
                ui.checkbox(&mut self.wand_contiguous, "Contiguous");
                let mut sample_all = self.sample == Sample::All;
                if ui.checkbox(&mut sample_all, "Sample All Layers").changed() {
                    self.sample = if sample_all {
                        Sample::All
                    } else {
                        Sample::Current
                    };
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
            let s = self.settings_mut();
            ui.label("Size");
            ui.add(
                egui::DragValue::new(&mut s.size)
                    .range(MIN_SIZE..=MAX_SIZE)
                    .speed(1.0)
                    .suffix(" px")
                    .fixed_decimals(0),
            );
            let percent = |ui: &mut Ui, label: &str, value: &mut f32| {
                ui.label(label);
                let mut p = *value * 100.0;
                if ui
                    .add(
                        Slider::new(&mut p, 0.0..=100.0)
                            .suffix("%")
                            .fixed_decimals(0),
                    )
                    .changed()
                {
                    *value = p / 100.0;
                }
            };
            percent(ui, "Hardness", &mut s.hardness);
            percent(ui, "Opacity", &mut s.opacity);
            percent(ui, "Flow", &mut s.flow);
            ui.separator();
            if self.tool == Tool::SpotHealing {
                self.sample_options(ui);
                ui.separator();
                let (text, colour) = match target {
                    Target::QuickMask => ("Quick Mask paints selections, not blemishes", theme.red),
                    Target::Mask => ("Select the layer, not its mask, to heal", theme.red),
                    Target::Pixels => (
                        "Paint over a blemish; it heals when you let go",
                        theme.dark_foreground,
                    ),
                };
                ui.label(RichText::new(text).color(colour));
            } else if self.tool.copies() {
                self.sample_options(ui);
                ui.separator();
                let (text, colour) = match (target, self.source) {
                    (Target::QuickMask, _) => (
                        "Quick Mask paints selections, not cloned pixels",
                        theme.red,
                    ),
                    (Target::Mask, _) => (
                        "Select the layer, not its mask, to clone or heal",
                        theme.red,
                    ),
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
                let colour = if active {
                    theme.foreground
                } else {
                    theme.dark_foreground
                };
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
                        ui.painter().add(egui::Shape::convex_polygon(
                            vec![p1, p2, p3],
                            colour,
                            egui::Stroke::NONE,
                        ));

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
                                    repaint_after =
                                        Some(std::time::Duration::from_secs_f64(remaining));
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
                                let item_colour = if item_active {
                                    theme.foreground
                                } else {
                                    theme.dark_foreground
                                };
                                let item_text =
                                    RichText::new(format!("{}  {}", tool.icon(), tool.name()))
                                        .color(item_colour);
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
            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut self.foreground)
                .on_hover_text("Foreground colour — X swaps, D resets");
            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut self.background)
                .on_hover_text("Background colour");
        });
    }
}

/// Grey level of an sRGB colour, as a mask value.
pub fn grey([r, g, b]: [u8; 3]) -> u16 {
    let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    (luma / 255.0 * 65535.0).round() as u16
}

/// Photoshop-style bracket-key size steps: finer for small brushes.
fn step_size(size: f32, bigger: bool) -> f32 {
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
    fn clone_offset_is_fixed_by_the_first_stroke_and_kept() {
        let mut tools = Tools {
            tool: Tool::CloneStamp,
            ..Tools::default()
        };
        let srgb = ColorProfile::srgb();
        assert_eq!(
            tools.paint(Target::Pixels, &srgb, Pos2::new(50.0, 50.0)),
            None
        );
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
        assert_eq!(
            eraser.paint(Target::Mask, &srgb, at),
            Some(Paint::Mask(u16::MAX))
        );
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
        assert_eq!(
            tools.paint(Target::Pixels, &ColorProfile::srgb(), Pos2::ZERO),
            None
        );

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
        let selection_tools = [
            Tool::Marquee,
            Tool::EllipticalMarquee,
            Tool::Lasso,
            Tool::MagicWand,
        ];
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
        ];
        for tool in other_tools {
            assert_eq!(cursor_badge(tool, none), None);
            assert_eq!(cursor_badge(tool, shift), None);
            assert_eq!(cursor_badge(tool, alt), None);
            assert_eq!(cursor_badge(tool, shift_alt), None);
            assert_eq!(cursor_badge(tool, ctrl), None);
        }
    }
}
