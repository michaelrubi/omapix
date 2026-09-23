//! The Layers panel, laid out like Photoshop's: blend mode and opacity for
//! the selected layer at the top, the stack (top layer first) in the
//! middle, and layer buttons at the bottom.

use egui::{Align, Button, ComboBox, Layout, RichText, ScrollArea, Sense, Slider, TextEdit, Ui};
use omapix_engine::tiled::Tiled;
use omapix_engine::{BlendMode, DisplayTransform, Pixel};

use crate::commands::Command;
use crate::editor::{Editor, Target, View};
use crate::theme::Theme;

// Nerd Font icons (Omarchy's fonts are all Nerd Fonts).
const EYE: &str = "\u{f06e}";
const EYE_OFF: &str = "\u{f070}";
const MASK: &str = "\u{f042}";
const PLUS: &str = "\u{f067}";
const COPY: &str = "\u{f0c5}";
const TRASH: &str = "\u{f1f8}";
const SLIDERS: &str = "\u{f1de}";

/// Thumbnail width (or height, for portrait images), in points.
const THUMB: f32 = 40.0;

/// What a thumbnail shows, kept to spot when it needs redrawing (layers
/// share tiles until edited, so comparing tiles is cheap).
enum ThumbSource {
    Pixels(Tiled<Pixel>),
    Mask(Tiled<u16>),
}

impl ThumbSource {
    fn same(&self, other: &ThumbSource) -> bool {
        match (self, other) {
            (ThumbSource::Pixels(a), ThumbSource::Pixels(b)) => a.same_tiles(b),
            (ThumbSource::Mask(a), ThumbSource::Mask(b)) => a.same_tiles(b) && a.fill() == b.fill(),
            _ => false,
        }
    }

    /// Sample the image down to `w`×`h` (nearest pixel) in display colour.
    fn render(&self, w: usize, h: usize, transform: &DisplayTransform) -> egui::ColorImage {
        let (w, h) = (w.max(1), h.max(1));
        let (iw, ih) = match self {
            ThumbSource::Pixels(t) => (t.width(), t.height()),
            ThumbSource::Mask(t) => (t.width(), t.height()),
        };
        let at = |x: usize, y: usize| {
            (
                (((x as f32 + 0.5) / w as f32 * iw as f32) as u32).min(iw - 1),
                (((y as f32 + 0.5) / h as f32 * ih as f32) as u32).min(ih - 1),
            )
        };
        let rgba: Vec<u8> = match self {
            ThumbSource::Pixels(t) => {
                let samples: Vec<Pixel> = (0..w * h)
                    .map(|i| {
                        let (x, y) = at(i % w, i / w);
                        t.get(x, y)
                    })
                    .collect();
                let mut out = vec![[0u8; 4]; samples.len()];
                transform.convert(&samples, &mut out);
                out.into_iter().flatten().collect()
            }
            ThumbSource::Mask(t) => (0..w * h)
                .flat_map(|i| {
                    let (x, y) = at(i % w, i / w);
                    let v = (t.get(x, y) >> 8) as u8;
                    [v, v, v, 255]
                })
                .collect(),
        };
        egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba)
    }
}

struct Thumb {
    source: ThumbSource,
    texture: egui::TextureHandle,
}

#[derive(Default)]
pub struct LayersPanel {
    /// Layer being renamed, and the name typed so far.
    renaming: Option<(u64, String)>,
    /// Thumbnails by (layer id, is mask).
    thumbs: std::collections::HashMap<(u64, bool), Thumb>,
    /// Layer being dragged to a new place in the stack.
    dragging: Option<u64>,
    /// A command asked for from inside a row (double-click on a thumbnail).
    command: Option<Command>,
}

impl LayersPanel {
    /// Draw the panel. Returns a command for buttons that act like menu items.
    pub fn show(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme) -> Option<Command> {
        let mut command = None;
        ui.add_space(4.0);
        ui.label(RichText::new("Layers").strong());
        ui.add_space(4.0);
        self.header(ui, editor);
        ui.separator();

        let footer_height = 30.0;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(ui.available_height() - footer_height)
            .show(ui, |ui| {
                // Top of the stack first, as in Photoshop.
                let ids: Vec<u64> = editor.doc.layers.iter().rev().map(|l| l.id).collect();
                self.thumbs.retain(|(id, _), _| ids.contains(id));
                let mut rows = Vec::with_capacity(ids.len());
                for &id in &ids {
                    let rect = self.row(ui, editor, theme, id);
                    // The whole row can be dragged to reorder. It only senses
                    // drags, so clicks still reach the eye, thumbnails and name.
                    let drag = ui.interact(rect, egui::Id::new(("layer-drag", id)), Sense::drag());
                    if drag.drag_started() && editor.busy().is_none() {
                        self.dragging = Some(id);
                    }
                    rows.push((id, rect));
                }
                self.reorder(ui, editor, theme, &ids, &rows);
            });

        ui.separator();
        ui.horizontal(|ui| {
            let busy = editor.busy().is_some();
            let buttons = [
                (PLUS, Command::NewLayer, "New layer (Ctrl+Shift+N)"),
                (COPY, Command::DuplicateLayer, "Duplicate layer (Ctrl+J)"),
                (MASK, Command::AddMask, "Add layer mask"),
                (TRASH, Command::DeleteLayer, "Delete layer"),
            ];
            for (icon, cmd, tip) in buttons {
                if ui
                    .add_enabled(!busy, Button::new(icon).frame(false))
                    .on_hover_text(tip)
                    .clicked()
                {
                    command = Some(cmd);
                }
            }
        });
        command.or(self.command.take())
    }

    fn header(&mut self, ui: &mut Ui, editor: &mut Editor) {
        let Some(layer) = editor.doc.layer(editor.active) else {
            return;
        };
        let mut blend = layer.blend;
        let mut opacity = layer.opacity * 100.0;
        let id = editor.active;

        ComboBox::from_id_salt("blend-mode")
            .selected_text(blend.name())
            .width(ui.available_width())
            .height(600.0)
            .show_ui(ui, |ui| {
                for (i, group) in BlendMode::MENU.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    for &mode in *group {
                        ui.selectable_value(&mut blend, mode, mode.name());
                    }
                }
            });
        if blend != layer.blend {
            editor.edit("Blend Mode", |doc, _| {
                if let Some(l) = doc.layer_mut(id) {
                    l.blend = blend;
                }
            });
        }

        ui.horizontal(|ui| {
            ui.label("Opacity");
            let slider = Slider::new(&mut opacity, 0.0..=100.0)
                .suffix("%")
                .fixed_decimals(0);
            if ui.add(slider).changed() {
                editor.edit_live("Opacity", |doc| {
                    if let Some(l) = doc.layer_mut(id) {
                        l.opacity = opacity / 100.0;
                    }
                });
            }
        });
    }

    /// While a row is dragged, show where it would land; on release, move it.
    fn reorder(
        &mut self,
        ui: &mut Ui,
        editor: &mut Editor,
        theme: &Theme,
        ids: &[u64],
        rows: &[(u64, egui::Rect)],
    ) {
        let Some(dragged) = self.dragging else { return };
        let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        // Slot k means "above the k-th row from the top".
        let slot = rows
            .iter()
            .position(|(_, r)| pointer.y < r.center().y)
            .unwrap_or(rows.len());
        let y = match rows.get(slot) {
            Some((_, r)) => r.top(),
            None => rows.last().map_or(pointer.y, |(_, r)| r.bottom()),
        };
        if let Some((_, first)) = rows.first() {
            ui.painter()
                .hline(first.x_range(), y, egui::Stroke::new(2.0, theme.accent));
        }
        if ui.input(|i| i.pointer.any_down()) {
            return;
        }
        self.dragging = None;
        let Some(order) = reordered(ids, dragged, slot) else {
            return;
        };
        editor.edit("Move Layer", |doc, _| {
            // `order` is top first; the document stores bottom first.
            let rank = |id: u64| {
                order
                    .iter()
                    .rev()
                    .position(|&o| o == id)
                    .unwrap_or(usize::MAX)
            };
            doc.layers.sort_by_key(|l| rank(l.id));
        });
    }

    /// Draw one layer row; returns its rectangle.
    fn row(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme, id: u64) -> egui::Rect {
        let Some(layer) = editor.doc.layer(id) else {
            return egui::Rect::NOTHING;
        };
        let selected = editor.active == id;
        let (visible, name, blend, mask, adjustment) = (
            layer.visible,
            layer.name.clone(),
            layer.blend,
            layer.mask.as_ref().map(|m| m.enabled),
            layer.adjustment.is_some(),
        );
        let fill = if selected {
            theme.selection
        } else {
            egui::Color32::TRANSPARENT
        };
        let dimmed = self.dragging == Some(id);
        let frame = egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(4, 3))
            .multiply_with_opacity(if dimmed { 0.5 } else { 1.0 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let eye = if visible { EYE } else { EYE_OFF };
                    let eye_colour = if visible {
                        theme.foreground
                    } else {
                        theme.dark_foreground
                    };
                    let eye_button = ui
                        .add(Button::new(RichText::new(eye).color(eye_colour)).frame(false))
                        .on_hover_text("Show/hide");
                    if eye_button.clicked() {
                        editor.edit(
                            if visible { "Hide Layer" } else { "Show Layer" },
                            |doc, _| {
                                if let Some(l) = doc.layer_mut(id) {
                                    l.visible = !l.visible;
                                }
                            },
                        );
                    }

                    // Pixel thumbnail (or the adjustment icon), then the mask
                    // thumbnail. Clicking one picks what painting applies to.
                    if adjustment {
                        ui.label(RichText::new(SLIDERS).size(18.0).color(theme.accent))
                            .on_hover_text("Adjustment layer");
                    } else {
                        let targeted = selected && editor.target == Target::Pixels;
                        if self
                            .thumbnail(ui, editor, id, false, targeted, theme)
                            .clicked()
                        {
                            editor.active = id;
                            editor.target = Target::Pixels;
                        }
                    }
                    if let Some(enabled) = mask {
                        let targeted = selected && editor.target == Target::Mask;
                        let response = self
                            .thumbnail(ui, editor, id, true, targeted, theme)
                            .on_hover_text(
                                "Layer mask — click to paint on it, Shift+click to disable",
                            );
                        if response.clicked() {
                            if ui.input(|i| i.modifiers.alt) {
                                // Alt+click shows the mask on its own, or goes back.
                                editor.active = id;
                                editor.target = Target::Mask;
                                let view = if editor.view() == View::Mask(id) {
                                    View::Image
                                } else {
                                    View::Mask(id)
                                };
                                editor.set_view(view);
                            } else if ui.input(|i| i.modifiers.shift) {
                                let label = if enabled {
                                    "Disable Layer Mask"
                                } else {
                                    "Enable Layer Mask"
                                };
                                editor.edit(label, |doc, _| {
                                    if let Some(m) = doc.layer_mut(id).and_then(|l| l.mask.as_mut())
                                    {
                                        m.enabled = !m.enabled;
                                    }
                                });
                            } else {
                                editor.active = id;
                                editor.target = Target::Mask;
                            }
                        }
                        if !enabled {
                            // Photoshop crosses out a disabled mask.
                            let r = response.rect;
                            ui.painter().line_segment(
                                [r.left_top(), r.right_bottom()],
                                egui::Stroke::new(2.0, theme.red),
                            );
                            ui.painter().line_segment(
                                [r.right_top(), r.left_bottom()],
                                egui::Stroke::new(2.0, theme.red),
                            );
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if blend != BlendMode::Normal {
                            ui.label(
                                RichText::new(blend.name())
                                    .small()
                                    .color(theme.dark_foreground),
                            );
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            self.name(ui, editor, id, &name, selected);
                        });
                    });
                });
            });
        frame.response.rect
    }

    /// A small preview of a layer's pixels or mask, rebuilt only when its
    /// tiles change.
    fn thumbnail(
        &mut self,
        ui: &mut Ui,
        editor: &Editor,
        id: u64,
        mask: bool,
        targeted: bool,
        theme: &Theme,
    ) -> egui::Response {
        let (w, h) = (editor.doc.width as f32, editor.doc.height as f32);
        let size = if w >= h {
            egui::vec2(THUMB, THUMB * h / w)
        } else {
            egui::vec2(THUMB * w / h, THUMB)
        };
        let layer = editor.doc.layer(id);
        let source = match (mask, layer) {
            (true, Some(l)) => l.mask.as_ref().map(|m| ThumbSource::Mask(m.pixels.clone())),
            (false, Some(l)) => Some(ThumbSource::Pixels(l.pixels.clone())),
            _ => None,
        };
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let Some(source) = source else {
            return response;
        };

        let key = (id, mask);
        let stale = self
            .thumbs
            .get(&key)
            .is_none_or(|t| !t.source.same(&source));
        if stale {
            let px = (size * ui.pixels_per_point()).round();
            let image = source.render(px.x as usize, px.y as usize, editor.canvas.transform());
            match self.thumbs.get_mut(&key) {
                Some(t) => {
                    t.texture.set(image, egui::TextureOptions::LINEAR);
                    t.source = source;
                }
                None => {
                    let texture = ui.ctx().load_texture(
                        format!("thumb-{id}-{mask}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.thumbs.insert(key, Thumb { source, texture });
                }
            }
        }
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(0x50));
        if let Some(t) = self.thumbs.get(&key) {
            painter.image(
                t.texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        let outline = if targeted {
            egui::Stroke::new(2.0, theme.accent)
        } else {
            egui::Stroke::new(1.0, theme.muted)
        };
        painter.rect_stroke(rect.expand(1.0), 0.0, outline, egui::StrokeKind::Outside);
        response
    }

    fn name(&mut self, ui: &mut Ui, editor: &mut Editor, id: u64, name: &str, selected: bool) {
        if let Some((rename_id, text)) = &mut self.renaming
            && *rename_id == id
        {
            let response = ui.add(TextEdit::singleline(text).desired_width(ui.available_width()));
            response.request_focus();
            let done = response.lost_focus();
            let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if done || cancelled {
                let new_name = text.trim().to_owned();
                self.renaming = None;
                if done && !cancelled && !new_name.is_empty() && new_name != name {
                    editor.edit("Rename Layer", |doc, _| {
                        if let Some(l) = doc.layer_mut(id) {
                            l.name = new_name;
                        }
                    });
                }
            }
            return;
        }
        let text = if selected {
            RichText::new(name).strong()
        } else {
            RichText::new(name)
        };
        let response = ui.add(egui::Label::new(text).truncate().sense(Sense::click()));
        if response.double_clicked() {
            self.renaming = Some((id, name.to_owned()));
        } else if response.clicked() {
            editor.active = id;
            // An adjustment layer has no pixels to paint; its mask is the target.
            let adjustment = editor.doc.layer(id).is_some_and(|l| l.adjustment.is_some());
            editor.target = if adjustment {
                Target::Mask
            } else {
                Target::Pixels
            };
        }
    }
}

/// The top-first layer order after dropping `dragged` in `slot` (above the
/// slot-th row; `ids.len()` is below the last). `None` if nothing moves.
fn reordered(ids: &[u64], dragged: u64, slot: usize) -> Option<Vec<u64>> {
    let from = ids.iter().position(|&i| i == dragged)?;
    // Removing the row first shifts later slots up by one.
    let to = if slot > from { slot - 1 } else { slot };
    if to == from {
        return None;
    }
    let mut order = ids.to_vec();
    order.remove(from);
    order.insert(to.min(order.len()), dragged);
    Some(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_moves_layers_like_photoshop() {
        let ids = [4, 3, 2, 1]; // top first
        // Drag the top layer below the second.
        assert_eq!(reordered(&ids, 4, 2), Some(vec![3, 4, 2, 1]));
        // Drag the bottom layer to the very top.
        assert_eq!(reordered(&ids, 1, 0), Some(vec![1, 4, 3, 2]));
        // Drag to the very bottom.
        assert_eq!(reordered(&ids, 3, 4), Some(vec![4, 2, 1, 3]));
        // Dropping just above or below itself changes nothing.
        assert_eq!(reordered(&ids, 3, 1), None);
        assert_eq!(reordered(&ids, 3, 2), None);
    }
}
