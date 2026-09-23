//! Painting tools: the toolbar on the left, the options bar under the menus,
//! and Photoshop's single-key tool shortcuts.

use egui::{Button, Key, Modifiers, RichText, Slider, Ui};
use omapix_engine::brush::{BrushSettings, Paint};
use omapix_engine::{ColorProfile, DisplayTransform, Pixel};

use crate::editor::Target;
use crate::theme::Theme;

const BRUSH_ICON: &str = "\u{f1fc}";
const ERASER_ICON: &str = "\u{f12d}";

const MIN_SIZE: f32 = 1.0;
const MAX_SIZE: f32 = 5000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Brush,
    Eraser,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
        }
    }
}

pub struct Tools {
    pub tool: Tool,
    brush: BrushSettings,
    eraser: BrushSettings,
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
            foreground: [0, 0, 0],
            background: [255, 255, 255],
        }
    }
}

impl Tools {
    pub fn settings(&self) -> BrushSettings {
        match self.tool {
            Tool::Brush => self.brush,
            Tool::Eraser => self.eraser,
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
            Tool::Brush => &mut self.brush,
            Tool::Eraser => &mut self.eraser,
        }
    }

    /// What a stroke should do, given what it paints on.
    pub fn paint(&self, target: Target, profile: &ColorProfile) -> Paint {
        match (self.tool, target) {
            // On a mask, the eraser reveals and the brush paints the
            // foreground colour's grey level, as in Photoshop.
            (Tool::Eraser, Target::Mask) => Paint::Mask(u16::MAX),
            (Tool::Brush, Target::Mask) => Paint::Mask(grey(self.foreground)),
            (Tool::Eraser, Target::Pixels) => Paint::Erase,
            (Tool::Brush, Target::Pixels) => Paint::Color(
                profile
                    .from_srgb8(self.foreground)
                    .unwrap_or([0, 0, 0, u16::MAX]),
            ),
        }
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
            let (text, colour) = match target {
                Target::Mask => ("Painting on layer mask", theme.accent),
                Target::Pixels => ("Painting on layer", theme.dark_foreground),
            };
            ui.label(RichText::new(text).color(colour));
        });
    }

    /// The toolbar: tools and the foreground/background colours.
    pub fn toolbar(&mut self, ui: &mut Ui, theme: &Theme) {
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            for (tool, icon, tip) in [
                (Tool::Brush, BRUSH_ICON, "Brush (B)"),
                (Tool::Eraser, ERASER_ICON, "Eraser (E)"),
            ] {
                let selected = self.tool == tool;
                let colour = if selected {
                    theme.accent
                } else {
                    theme.foreground
                };
                let button =
                    Button::new(RichText::new(icon).size(18.0).color(colour)).selected(selected);
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
fn grey([r, g, b]: [u8; 3]) -> u16 {
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
    fn bracket_steps_round_and_clamp() {
        assert_eq!(step_size(100.0, true), 125.0);
        assert_eq!(step_size(100.0, false), 90.0);
        assert_eq!(step_size(3.0, false), 2.0);
        assert_eq!(step_size(1.0, false), 1.0);
        assert_eq!(step_size(5000.0, true), 5000.0);
    }

    #[test]
    fn mask_paint_follows_foreground_grey() {
        let tools = Tools::default();
        let srgb = ColorProfile::srgb();
        assert_eq!(tools.paint(Target::Mask, &srgb), Paint::Mask(0));
        let eraser = Tools {
            tool: Tool::Eraser,
            ..Tools::default()
        };
        assert_eq!(eraser.paint(Target::Mask, &srgb), Paint::Mask(u16::MAX));
        assert_eq!(eraser.paint(Target::Pixels, &srgb), Paint::Erase);
    }
}
