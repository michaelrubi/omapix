//! Painting tools: the toolbar on the left, the options bar under the menus,
//! and Photoshop's single-key tool shortcuts.

use egui::{Button, Key, Modifiers, Pos2, RichText, Slider, Ui, Vec2};
use omapix_engine::brush::{BrushSettings, Paint};
use omapix_engine::{ColorProfile, DisplayTransform, Pixel};

use crate::editor::Target;
use crate::theme::Theme;

const BRUSH_ICON: &str = "\u{f1fc}";
const ERASER_ICON: &str = "\u{f12d}";
const CLONE_ICON: &str = "\u{f24d}";
const HEAL_ICON: &str = "\u{f0fa}";
const SPOT_ICON: &str = "\u{f0d0}";
const MARQUEE_ICON: &str = "\u{f096}";
const ELLIPSE_ICON: &str = "\u{f10c}";
const LASSO_ICON: &str = "\u{f0c4}";

const MIN_SIZE: f32 = 1.0;
const MAX_SIZE: f32 = 5000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Brush,
    Eraser,
    CloneStamp,
    SpotHealing,
    Healing,
    Marquee,
    EllipticalMarquee,
    Lasso,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::CloneStamp => "Clone Stamp",
            Tool::SpotHealing => "Spot Healing Brush",
            Tool::Healing => "Healing Brush",
            Tool::Marquee => "Rectangular Marquee",
            Tool::EllipticalMarquee => "Elliptical Marquee",
            Tool::Lasso => "Lasso",
        }
    }

    /// Tools that make selections rather than paint.
    pub fn selects(self) -> bool {
        matches!(self, Tool::Marquee | Tool::EllipticalMarquee | Tool::Lasso)
    }

    /// Tools that copy pixels from a source point set with Alt+click.
    pub fn copies(self) -> bool {
        matches!(self, Tool::CloneStamp | Tool::Healing)
    }
}

pub struct Tools {
    pub tool: Tool,
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
    /// Clone/heal from all visible layers rather than the active one.
    pub sample_all: bool,
    /// Foreground and background colours, in sRGB as shown in the pickers.
    pub foreground: [u8; 3],
    pub background: [u8; 3],
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            tool: Tool::Brush,
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
            sample_all: true,
            foreground: [0, 0, 0],
            background: [255, 255, 255],
        }
    }
}

impl Tools {
    pub fn settings(&self) -> BrushSettings {
        match self.tool {
            Tool::Brush | Tool::Marquee | Tool::EllipticalMarquee | Tool::Lasso => self.brush,
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
            Tool::Brush | Tool::Marquee | Tool::EllipticalMarquee | Tool::Lasso => &mut self.brush,
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
        if self.tool.selects() {
            return None;
        }
        if self.tool == Tool::SpotHealing {
            return (target == Target::Pixels).then_some(Paint::SpotHeal);
        }
        if self.tool.copies() {
            if target == Target::Mask {
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
            (Tool::Eraser, Target::Mask) => Paint::Mask(u16::MAX),
            (Tool::Brush, Target::Mask) => Paint::Mask(grey(self.foreground)),
            (Tool::Eraser, Target::Pixels) => Paint::Erase,
            (_, Target::Pixels) => {
                Paint::Color(
                    profile
                        .from_srgb8(self.foreground)
                        .unwrap_or([0, 0, 0, u16::MAX]),
                )
            }
            (_, Target::Mask) => Paint::Mask(grey(self.foreground)),
        })
    }

    /// Set the foreground colour from a document pixel (the eyedropper).
    pub fn sample(&mut self, pixel: Pixel, profile: &ColorProfile) {
        if let Ok(transform) = DisplayTransform::to_srgb(profile) {
            let mut out = [[0u8; 4]];
            transform.convert(&[[pixel[0], pixel[1], pixel[2], u16::MAX]], &mut out);
            self.foreground = [out[0][0], out[0][1], out[0][2]];
        }
    }

    /// Photoshop's single-key shortcuts. Call only when no text field has
    /// keyboard focus.
    pub fn keys(&mut self, ctx: &egui::Context) {
        ctx.input_mut(|i| {
            let shift = Modifiers::SHIFT;
            if i.consume_key(Modifiers::NONE, Key::B) {
                self.tool = Tool::Brush;
            }
            if i.consume_key(Modifiers::NONE, Key::E) {
                self.tool = Tool::Eraser;
            }
            if i.consume_key(Modifiers::NONE, Key::S) {
                self.tool = Tool::CloneStamp;
            }
            // J is the Spot Healing Brush; Shift+J switches to the Healing
            // Brush (Photoshop cycles the J tools with Shift+J).
            if i.consume_key(Modifiers::SHIFT, Key::J) {
                self.tool = if self.tool == Tool::Healing {
                    Tool::SpotHealing
                } else {
                    Tool::Healing
                };
            }
            if i.consume_key(Modifiers::NONE, Key::J) {
                self.tool = Tool::SpotHealing;
            }
            // M is the Rectangular Marquee; Shift+M switches to the Elliptical
            // Marquee (Photoshop cycles the M tools with Shift+M).
            if i.consume_key(Modifiers::SHIFT, Key::M) {
                self.tool = if self.tool == Tool::EllipticalMarquee {
                    Tool::Marquee
                } else {
                    Tool::EllipticalMarquee
                };
            }
            if i.consume_key(Modifiers::NONE, Key::M) {
                self.tool = Tool::Marquee;
            }
            if i.consume_key(Modifiers::NONE, Key::L) {
                self.tool = Tool::Lasso;
            }
            if i.consume_key(Modifiers::NONE, Key::X) {
                std::mem::swap(&mut self.foreground, &mut self.background);
            }
            if i.consume_key(Modifiers::NONE, Key::D) {
                self.foreground = [0, 0, 0];
                self.background = [255, 255, 255];
            }
            // Shift+[ / Shift+] change hardness in 25 % steps; [ / ] change size.
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
                    self.settings_mut().opacity = if n == 0 { 1.0 } else { n as f32 / 10.0 };
                }
            }
        });
    }

    /// The options bar: settings for the current tool.
    pub fn options_bar(&mut self, ui: &mut Ui, target: Target, theme: &Theme) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.tool.name()).strong());
            ui.separator();
            if self.tool.selects() {
                let hint = "Drag to select · Shift adds · Alt subtracts · Shift+Alt intersects · \
                            click outside to deselect · Shift+F6 feathers";
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
                ui.label("Sample");
                ui.selectable_value(&mut self.sample_all, false, "Current Layer");
                ui.selectable_value(&mut self.sample_all, true, "All Layers");
                ui.separator();
                let (text, colour) = match target {
                    Target::Mask => ("Select the layer, not its mask, to heal", theme.red),
                    Target::Pixels => (
                        "Paint over a blemish; it heals when you let go",
                        theme.dark_foreground,
                    ),
                };
                ui.label(RichText::new(text).color(colour));
            } else if self.tool.copies() {
                ui.label("Sample");
                ui.selectable_value(&mut self.sample_all, false, "Current Layer");
                ui.selectable_value(&mut self.sample_all, true, "All Layers");
                ui.separator();
                let (text, colour) = match (target, self.source) {
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
                    Target::Mask => ("Painting on layer mask", theme.accent),
                    Target::Pixels => ("Painting on layer", theme.dark_foreground),
                };
                ui.label(RichText::new(text).color(colour));
            }
        });
    }

    /// The toolbar: tools and the foreground/background colours.
    pub fn toolbar(&mut self, ui: &mut Ui, theme: &Theme) {
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            for (tool, icon, tip) in [
                (Tool::Brush, BRUSH_ICON, "Brush (B)"),
                (Tool::Eraser, ERASER_ICON, "Eraser (E)"),
                (Tool::CloneStamp, CLONE_ICON, "Clone Stamp (S)"),
                (Tool::SpotHealing, SPOT_ICON, "Spot Healing Brush (J)"),
                (Tool::Healing, HEAL_ICON, "Healing Brush (Shift+J)"),
                (Tool::Marquee, MARQUEE_ICON, "Rectangular Marquee (M)"),
                (Tool::EllipticalMarquee, ELLIPSE_ICON, "Elliptical Marquee (Shift+M)"),
                (Tool::Lasso, LASSO_ICON, "Lasso (L)"),
            ] {
                let active = self.tool == tool;
                let colour = if active {
                    theme.foreground
                } else {
                    theme.dark_foreground
                };
                let button = Button::new(RichText::new(icon).size(18.0).color(colour))
                    .frame(false)
                    .min_size(Vec2::splat(28.0));
                if ui.add(button).on_hover_text(tip).clicked() {
                    self.tool = tool;
                }
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
}
