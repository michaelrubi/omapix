//! The Blending Options dialog: Photoshop's Blend If sliders, applied live.

use egui::{Color32, ComboBox, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use omapix_engine::layer::{BlendIf, BlendIfChannel};

use crate::editor::Editor;
use crate::theme::Theme;

/// Handles closer than this (in points) to the pointer can be grabbed.
const GRAB: f32 = 10.0;

pub struct BlendingOptions {
    /// Layer being edited.
    pub layer: u64,
    /// Handle being dragged: which slider, which handle (0–3), whether
    /// it moves alone (Alt held when the drag started), and where the pair
    /// was when the drag began.
    dragging: Option<Drag>,
}

#[derive(Clone, Copy)]
struct Drag {
    underlying: bool,
    handle: usize,
    split: bool,
    start: [f32; 4],
    start_value: f32,
}

impl BlendingOptions {
    pub fn new(layer: u64) -> Self {
        Self {
            layer,
            dragging: None,
        }
    }

    /// Draw the dialog. Returns `Some(true)` for OK, `Some(false)` for
    /// Cancel, `None` while it stays open.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        editor: &mut Editor,
        theme: &Theme,
    ) -> Option<bool> {
        let id = self.layer;
        let layer = editor.doc.layer(id)?;
        let name = layer.name.clone();
        let original = layer.blend_if.unwrap_or_default();
        let mut blend_if = original;
        let mut result = None;

        let area = egui::Modal::default_area(egui::Id::new("blending"))
            .anchor(egui::Align2::RIGHT_TOP, vec2(-320.0, 90.0));
        let response = egui::Modal::new(egui::Id::new("blending"))
            .area(area)
            .backdrop_color(Color32::TRANSPARENT)
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.heading("Blending Options");
                ui.label(RichText::new(name).color(theme.dark_foreground));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Blend If");
                    let names = [
                        (BlendIfChannel::Gray, "Gray"),
                        (BlendIfChannel::Red, "Red"),
                        (BlendIfChannel::Green, "Green"),
                        (BlendIfChannel::Blue, "Blue"),
                    ];
                    let current = names
                        .iter()
                        .find(|(c, _)| *c == blend_if.channel)
                        .map_or("Gray", |(_, n)| n);
                    ComboBox::from_id_salt("blend-if-channel")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (channel, label) in names {
                                ui.selectable_value(&mut blend_if.channel, channel, label);
                            }
                        });
                });
                ui.add_space(6.0);
                for underlying in [false, true] {
                    let range = if underlying {
                        &mut blend_if.underlying
                    } else {
                        &mut blend_if.this
                    };
                    ui.horizontal(|ui| {
                        ui.label(if underlying {
                            "Underlying Layer"
                        } else {
                            "This Layer"
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(readout(*range)).monospace());
                        });
                    });
                    self.slider(ui, range, underlying, theme);
                    ui.add_space(6.0);
                }
                ui.label(
                    RichText::new(
                        "Drag a handle to move it; Alt+drag to split it for a smooth transition.",
                    )
                    .small()
                    .color(theme.dark_foreground),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        result = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        result = Some(false);
                    }
                    if ui.button("Reset").clicked() {
                        blend_if = BlendIf {
                            channel: blend_if.channel,
                            ..BlendIf::default()
                        };
                    }
                });
            });
        if response.should_close() && result.is_none() {
            result = Some(false);
        }
        if blend_if != original {
            editor.edit_live("Blending Options", |doc| {
                if let Some(layer) = doc.layer_mut(id) {
                    layer.blend_if = (!blend_if.is_neutral()
                        || blend_if.channel != BlendIfChannel::Gray)
                        .then_some(blend_if);
                }
            });
        }
        result
    }

    /// A black-to-white bar with four handles below it.
    fn slider(&mut self, ui: &mut Ui, range: &mut [f32; 4], underlying: bool, theme: &Theme) {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click_and_drag());
        let bar = Rect::from_min_max(
            rect.min + vec2(6.0, 0.0),
            pos2(rect.max.x - 6.0, rect.min.y + 12.0),
        );
        let x_of = |v: f32| bar.left() + v * bar.width();
        let value_at = |p: Pos2| ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0);

        if let Some(pointer) = response.interact_pointer_pos() {
            if response.drag_started() || response.clicked() {
                let split = ui.input(|i| i.modifiers.alt);
                let nearest = (0..4)
                    .map(|h| (h, (x_of(range[h]) - pointer.x).abs()))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .filter(|(_, d)| *d <= GRAB);
                self.dragging = nearest.map(|(h, _)| {
                    // Overlapping handles: pick by which side the pointer is on.
                    let pair = if h < 2 { [0, 1] } else { [2, 3] };
                    let handle = if range[pair[0]] == range[pair[1]] && split {
                        if pointer.x < x_of(range[pair[0]]) {
                            pair[0]
                        } else {
                            pair[1]
                        }
                    } else {
                        h
                    };
                    Drag {
                        underlying,
                        handle,
                        split,
                        start: *range,
                        start_value: value_at(pointer),
                    }
                });
            }
            if let Some(drag) = self.dragging
                && drag.underlying == underlying
                && response.dragged()
            {
                *range = moved(drag, value_at(pointer));
            }
        }
        let released = response.drag_stopped() || !ui.input(|i| i.pointer.primary_down());
        if released && self.dragging.is_some_and(|d| d.underlying == underlying) {
            self.dragging = None;
        }

        let painter = ui.painter();
        let mut mesh = egui::Mesh::default();
        let base = mesh.vertices.len() as u32;
        for (p, c) in [
            (bar.left_top(), Color32::BLACK),
            (bar.right_top(), Color32::WHITE),
            (bar.right_bottom(), Color32::WHITE),
            (bar.left_bottom(), Color32::BLACK),
        ] {
            mesh.colored_vertex(p, c);
        }
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
        painter.add(Shape::mesh(mesh));
        painter.rect_stroke(
            bar,
            0.0,
            Stroke::new(1.0, theme.muted),
            egui::StrokeKind::Outside,
        );

        for (h, &v) in range.iter().enumerate() {
            let x = x_of(v);
            let top = bar.bottom() + 2.0;
            // A split pair shows as two half-triangles, as in Photoshop.
            let (left, right) = match h {
                0 if range[0] != range[1] => (x - 6.0, x),
                1 if range[0] != range[1] => (x, x + 6.0),
                2 if range[2] != range[3] => (x - 6.0, x),
                3 if range[2] != range[3] => (x, x + 6.0),
                _ => (x - 6.0, x + 6.0),
            };
            let points = vec![
                pos2(x, top),
                pos2(right, top + 10.0),
                pos2(left, top + 10.0),
            ];
            let (fill, edge) = if h < 2 {
                (Color32::BLACK, Color32::WHITE)
            } else {
                (Color32::WHITE, Color32::BLACK)
            };
            painter.add(Shape::convex_polygon(points, fill, Stroke::new(1.0, edge)));
        }
    }
}

/// The range after dragging `drag.handle` to `value`: alone if split,
/// otherwise together with its partner. Handles never cross the other pair.
fn moved(drag: Drag, value: f32) -> [f32; 4] {
    let mut r = drag.start;
    let black = drag.handle < 2;
    if drag.split {
        r[drag.handle] = value;
    } else {
        let delta = value - drag.start_value;
        let pair = if black { [0, 1] } else { [2, 3] };
        let lo = -drag.start[pair[0]];
        let hi = 1.0 - drag.start[pair[1]];
        let delta = delta.clamp(lo, hi);
        r[pair[0]] += delta;
        r[pair[1]] += delta;
    }
    // Keep order: black ≤ black split ≤ white split ≤ white.
    if black {
        r[1] = r[1].min(r[2]);
        r[0] = r[0].min(r[1]);
    } else {
        r[2] = r[2].max(r[1]);
        r[3] = r[3].max(r[2]);
    }
    if drag.split {
        // Within a pair, the dragged handle pushes its partner.
        match drag.handle {
            0 => r[1] = r[1].max(r[0]),
            1 => r[0] = r[0].min(r[1]),
            2 => r[3] = r[3].max(r[2]),
            _ => r[2] = r[2].min(r[3]),
        }
    }
    r
}

/// Photoshop's readout: "0   255", or "0/40   200/255" when split.
fn readout(r: [f32; 4]) -> String {
    let v = |x: f32| (x * 255.0).round() as u32;
    let pair = |a: f32, b: f32| {
        if v(a) == v(b) {
            format!("{}", v(a))
        } else {
            format!("{}/{}", v(a), v(b))
        }
    };
    format!("{}   {}", pair(r[0], r[1]), pair(r[2], r[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(handle: usize, split: bool, start: [f32; 4], from: f32) -> Drag {
        Drag {
            underlying: false,
            handle,
            split,
            start,
            start_value: from,
        }
    }

    #[test]
    fn pairs_move_together_unless_split() {
        let full = [0.0, 0.0, 1.0, 1.0];
        assert_eq!(moved(drag(0, false, full, 0.0), 0.3), [0.3, 0.3, 1.0, 1.0]);
        assert_eq!(moved(drag(1, true, full, 0.0), 0.3), [0.0, 0.3, 1.0, 1.0]);
        assert_eq!(moved(drag(3, false, full, 1.0), 0.7), [0.0, 0.0, 0.7, 0.7]);
    }

    #[test]
    fn handles_never_cross() {
        let r = [0.2, 0.3, 0.6, 0.8];
        let out = moved(drag(1, true, r, 0.3), 0.9);
        assert!(out[1] <= out[2], "{out:?}");
        let out = moved(drag(0, false, r, 0.2), 1.0);
        assert!(
            out[0] <= out[1] && out[1] <= out[2] && out[2] <= out[3],
            "{out:?}"
        );
    }

    #[test]
    fn readout_shows_splits_like_photoshop() {
        assert_eq!(readout([0.0, 0.0, 1.0, 1.0]), "0   255");
        assert_eq!(
            readout([0.0, 40.0 / 255.0, 200.0 / 255.0, 1.0]),
            "0/40   200/255"
        );
    }
}
