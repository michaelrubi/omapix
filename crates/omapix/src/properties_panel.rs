//! The Properties panel: settings of the selected adjustment layer, edited
//! live, as in Photoshop's Properties panel.

use egui::{Color32, ComboBox, Pos2, Rect, RichText, Sense, Slider, Stroke, Ui, pos2, vec2};
use omapix_engine::adjust::{
    Adjustment, ChannelMixer, ColorBalance, Curve, Curves, HueSaturation, Levels, SelectiveColor,
};

use crate::editor::Editor;
use crate::theme::Theme;

/// How close (in points) the pointer must be to grab a curve point.
const GRAB: f32 = 8.0;
/// Dragging a point this far outside the graph removes it.
const REMOVE: f32 = 24.0;

#[derive(Default)]
pub struct PropertiesPanel {
    /// Curves channel being edited: 0 = RGB, 1–3 = red, green, blue.
    channel: usize,
    /// Curve point being dragged.
    dragging: Option<usize>,
    /// Color Balance tonal range: 0 = shadows, 1 = midtones, 2 = highlights.
    tone: usize,
    /// Selective Color selected range: 0..=8.
    selective_color_range: usize,
    /// Channel Mixer selected output channel: 0 = Red, 1 = Green, 2 = Blue.
    mixer_channel: usize,
}

impl PropertiesPanel {
    /// Draw the panel for the active layer if it's an adjustment layer.
    /// Returns false (drawing nothing) otherwise.
    pub fn show(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme) -> bool {
        let id = editor.active;
        let Some(mut adjustment) = editor.doc.layer(id).and_then(|l| l.adjustment.clone()) else {
            return false;
        };
        let original = adjustment.clone();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Properties — {}", adjustment.name())).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset").clicked() {
                    adjustment = match adjustment {
                        Adjustment::Curves(_) => Adjustment::Curves(Curves::default()),
                        Adjustment::Levels(_) => Adjustment::Levels(Levels::default()),
                        Adjustment::HueSaturation(_) => {
                            Adjustment::HueSaturation(HueSaturation::default())
                        }
                        Adjustment::ColorBalance(_) => {
                            Adjustment::ColorBalance(ColorBalance::default())
                        }
                        Adjustment::SelectiveColor(_) => {
                            Adjustment::SelectiveColor(SelectiveColor::default())
                        }
                        Adjustment::ChannelMixer(_) => {
                            Adjustment::ChannelMixer(ChannelMixer::default())
                        }
                    };
                }
            });
        });
        ui.add_space(4.0);
        match &mut adjustment {
            Adjustment::Curves(c) => self.curves(ui, c, theme),
            Adjustment::Levels(l) => levels(ui, l),
            Adjustment::HueSaturation(h) => hue_saturation(ui, h),
            Adjustment::ColorBalance(b) => self.color_balance(ui, b),
            Adjustment::SelectiveColor(s) => self.selective_color(ui, s),
            Adjustment::ChannelMixer(m) => self.channel_mixer(ui, m, theme),
        }
        if adjustment != original {
            let label = format!("Adjust {}", adjustment.name());
            editor.edit_live(&label, |doc| {
                if let Some(layer) = doc.layer_mut(id) {
                    layer.adjustment = Some(adjustment);
                }
            });
        }
        ui.separator();
        true
    }

    fn curves(&mut self, ui: &mut Ui, curves: &mut Curves, theme: &Theme) {
        let names = ["RGB", "Red", "Green", "Blue"];
        ComboBox::from_id_salt("curves-channel")
            .selected_text(names[self.channel])
            .show_ui(ui, |ui| {
                for (i, name) in names.iter().enumerate() {
                    ui.selectable_value(&mut self.channel, i, *name);
                }
            });
        let colour = [
            theme.foreground,
            Color32::from_rgb(230, 80, 80),
            Color32::from_rgb(80, 200, 100),
            Color32::from_rgb(90, 140, 240),
        ][self.channel];
        let curve = match self.channel {
            1 => &mut curves.red,
            2 => &mut curves.green,
            3 => &mut curves.blue,
            _ => &mut curves.master,
        };

        let side = ui.available_width().min(300.0);
        let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
        let to_screen = |(x, y): (f32, f32)| {
            pos2(
                rect.left() + x * rect.width(),
                rect.bottom() - y * rect.height(),
            )
        };
        let to_curve = |p: Pos2| {
            (
                ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
                ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0),
            )
        };

        // Grab, add, move and remove points.
        if let Some(pointer) = response.interact_pointer_pos() {
            if response.drag_started() || response.clicked() {
                let nearest = curve
                    .points
                    .iter()
                    .enumerate()
                    .map(|(i, &p)| (i, to_screen(p).distance(pointer)))
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                self.dragging = match nearest {
                    Some((i, d)) if d <= GRAB => Some(i),
                    _ if rect.contains(pointer) => Some(insert_point(curve, to_curve(pointer))),
                    _ => None,
                };
            }
            if let Some(i) = self.dragging
                && response.dragged()
            {
                let outside = !rect.expand(REMOVE).contains(pointer);
                let endpoint = i == 0 || i + 1 == curve.points.len();
                if outside && !endpoint {
                    curve.points.remove(i);
                    self.dragging = None;
                } else {
                    move_point(curve, i, to_curve(pointer));
                }
            }
        }
        if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            self.dragging = None;
        }

        let painter = ui.painter_at(rect.expand(4.0));
        painter.rect_filled(rect, 0.0, theme.darker_background);
        let grid = Stroke::new(1.0, theme.selection);
        for i in 1..4 {
            let t = i as f32 / 4.0;
            painter.line_segment([to_screen((t, 0.0)), to_screen((t, 1.0))], grid);
            painter.line_segment([to_screen((0.0, t)), to_screen((1.0, t))], grid);
        }
        painter.line_segment(
            [to_screen((0.0, 0.0)), to_screen((1.0, 1.0))],
            Stroke::new(1.0, theme.muted),
        );
        let line: Vec<Pos2> = (0..=128)
            .map(|i| {
                let x = i as f32 / 128.0;
                to_screen((x, curve.eval(x).clamp(0.0, 1.0)))
            })
            .collect();
        painter.add(egui::Shape::line(line, Stroke::new(1.5, colour)));
        for (i, &p) in curve.points.iter().enumerate() {
            let at = to_screen(p);
            let r = Rect::from_center_size(at, vec2(7.0, 7.0));
            if self.dragging == Some(i) {
                painter.rect_filled(r, 0.0, colour);
            } else {
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, colour), egui::StrokeKind::Middle);
            }
        }
        let readout = match self.dragging {
            Some(i) => {
                let (x, y) = curve.points[i];
                format!("Input {:.0}   Output {:.0}", x * 255.0, y * 255.0)
            }
            None => "Click the curve to add a point; drag one off to remove it".into(),
        };
        ui.label(RichText::new(readout).small().color(theme.dark_foreground));
    }

    fn color_balance(&mut self, ui: &mut Ui, b: &mut ColorBalance) {
        ui.horizontal(|ui| {
            for (i, name) in ["Shadows", "Midtones", "Highlights"]
                .into_iter()
                .enumerate()
            {
                ui.selectable_value(&mut self.tone, i, name);
            }
        });
        let values = match self.tone {
            0 => &mut b.shadows,
            2 => &mut b.highlights,
            _ => &mut b.midtones,
        };
        for (value, (left, right)) in
            values
                .iter_mut()
                .zip([("Cyan", "Red"), ("Magenta", "Green"), ("Yellow", "Blue")])
        {
            ui.horizontal(|ui| {
                ui.label(RichText::new(left).small());
                ui.add(
                    Slider::new(value, -100.0..=100.0)
                        .fixed_decimals(0)
                        .show_value(true),
                );
                ui.label(RichText::new(right).small());
            });
        }
        ui.checkbox(&mut b.preserve_luminosity, "Preserve Luminosity");
    }

    fn selective_color(&mut self, ui: &mut Ui, s: &mut SelectiveColor) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Colors:").small());
            ComboBox::from_id_salt("selective-color-range")
                .selected_text(SelectiveColor::RANGE_NAMES[self.selective_color_range])
                .show_ui(ui, |ui| {
                    for (i, name) in SelectiveColor::RANGE_NAMES.iter().enumerate() {
                        ui.selectable_value(&mut self.selective_color_range, i, *name);
                    }
                });
        });
        ui.add_space(4.0);
        let values = &mut s.ranges[self.selective_color_range];
        for (value, label) in values.iter_mut().zip(["Cyan", "Magenta", "Yellow", "Black"]) {
            ui.add(
                Slider::new(value, -100.0..=100.0)
                    .text(label)
                    .fixed_decimals(0)
                    .suffix("%"),
            );
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.relative, true, "Relative");
            ui.radio_value(&mut s.relative, false, "Absolute");
        });
    }

    fn channel_mixer(&mut self, ui: &mut Ui, m: &mut ChannelMixer, theme: &Theme) {
        if !m.monochrome {
            let names = ["Red", "Green", "Blue"];
            ui.horizontal(|ui| {
                ui.label(RichText::new("Output Channel:").small());
                ComboBox::from_id_salt("mixer-channel")
                    .selected_text(names[self.mixer_channel.min(2)])
                    .show_ui(ui, |ui| {
                        for (i, name) in names.iter().enumerate() {
                            ui.selectable_value(&mut self.mixer_channel, i, *name);
                        }
                    });
            });
        } else {
            ui.label(RichText::new("Output Channel: Gray").small());
        }
        ui.add_space(4.0);
        let values = if m.monochrome {
            &mut m.gray
        } else {
            match self.mixer_channel {
                0 => &mut m.red,
                1 => &mut m.green,
                _ => &mut m.blue,
            }
        };
        for (value, label) in values.iter_mut().zip(["Red", "Green", "Blue", "Constant"]) {
            ui.add(
                Slider::new(value, -200.0..=200.0)
                    .text(label)
                    .fixed_decimals(0)
                    .suffix("%"),
            );
        }
        let total = values[0] + values[1] + values[2];
        ui.label(
            RichText::new(format!("Total: {:.0}%", total))
                .small()
                .color(theme.dark_foreground),
        );
        ui.add_space(4.0);
        ui.checkbox(&mut m.monochrome, "Monochrome");
    }
}

/// Add a point at `p` between its neighbours; returns its index.
fn insert_point(curve: &mut Curve, (x, y): (f32, f32)) -> usize {
    let i = curve
        .points
        .iter()
        .position(|&(px, _)| px > x)
        .unwrap_or(curve.points.len());
    curve.points.insert(i, (x, y));
    i
}

/// Move point `i`, keeping points in order along the input axis.
fn move_point(curve: &mut Curve, i: usize, (x, y): (f32, f32)) {
    const GAP: f32 = 0.01;
    let lo = if i == 0 {
        0.0
    } else {
        curve.points[i - 1].0 + GAP
    };
    let hi = if i + 1 == curve.points.len() {
        1.0
    } else {
        curve.points[i + 1].0 - GAP
    };
    curve.points[i] = (x.clamp(lo, hi.max(lo)), y);
}

fn levels(ui: &mut Ui, l: &mut Levels) {
    // Shown on Photoshop's 0–255 scale.
    fn level(ui: &mut Ui, label: &str, value: &mut f32) {
        let mut v = *value * 255.0;
        if ui
            .add(
                Slider::new(&mut v, 0.0..=255.0)
                    .text(label)
                    .fixed_decimals(0),
            )
            .changed()
        {
            *value = v / 255.0;
        }
    }
    ui.label(RichText::new("Input").small());
    level(ui, "Black", &mut l.in_black);
    ui.add(
        Slider::new(&mut l.gamma, 0.1..=9.99)
            .text("Midtones")
            .fixed_decimals(2)
            .logarithmic(true),
    );
    level(ui, "White", &mut l.in_white);
    ui.label(RichText::new("Output").small());
    level(ui, "Black", &mut l.out_black);
    level(ui, "White", &mut l.out_white);
    if l.in_white <= l.in_black + 0.004 {
        l.in_white = (l.in_black + 0.004).min(1.0);
    }
}

fn hue_saturation(ui: &mut Ui, h: &mut HueSaturation) {
    ui.add(
        Slider::new(&mut h.hue, -180.0..=180.0)
            .text("Hue")
            .fixed_decimals(0),
    );
    ui.add(
        Slider::new(&mut h.saturation, -100.0..=100.0)
            .text("Saturation")
            .fixed_decimals(0),
    );
    ui.add(
        Slider::new(&mut h.lightness, -100.0..=100.0)
            .text("Lightness")
            .fixed_decimals(0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_stay_ordered_when_dragged_past_neighbours() {
        let mut c = Curve::default();
        let i = insert_point(&mut c, (0.5, 0.6));
        assert_eq!(i, 1);
        move_point(&mut c, 1, (1.2, 0.9));
        assert!(c.points[1].0 < c.points[2].0);
        move_point(&mut c, 1, (-1.0, 0.1));
        assert!(c.points[1].0 > c.points[0].0);
    }

    #[test]
    fn properties_panel_renders_selective_color_and_channel_mixer() {
        let (w, h) = (10, 10);
        let image = omapix_engine::Raster::new(
            w,
            h,
            vec![[30000, 30000, 30000, 65535]; (w * h) as usize],
        );
        let mut doc = omapix_engine::Document::from_image(
            "t.tif".into(),
            &image,
            omapix_engine::ColorProfile::srgb(),
            16,
        );
        let sc = Adjustment::SelectiveColor(SelectiveColor::default());
        doc.layers
            .push(omapix_engine::Layer::adjustment(101, sc, w, h));
        let cm = Adjustment::ChannelMixer(ChannelMixer::default());
        doc.layers
            .push(omapix_engine::Layer::adjustment(102, cm, w, h));

        let mut editor = Editor::new(doc).unwrap();
        let mut panel = PropertiesPanel::default();
        let theme = Theme::default();
        let ctx = egui::Context::default();

        // Target Selective Color layer
        editor.active = 101;
        let mut shown = false;
        let input = egui::RawInput::default();
        let mut out = ctx.run_ui(input, |ui| {
            shown = panel.show(ui, &mut editor, &theme);
        });
        out.textures_delta.clear();
        assert!(shown, "panel should show for selective color adjustment");

        // Target Channel Mixer layer
        editor.active = 102;
        let mut shown = false;
        let input = egui::RawInput::default();
        let mut out = ctx.run_ui(input, |ui| {
            shown = panel.show(ui, &mut editor, &theme);
        });
        out.textures_delta.clear();
        assert!(shown, "panel should show for channel mixer adjustment");
    }
}
