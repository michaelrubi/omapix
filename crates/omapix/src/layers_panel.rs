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

#[derive(Clone, Debug, PartialEq)]
struct Rename {
    id: u64,
    text: String,
    needs_focus: bool,
}

impl Rename {
    fn new(id: u64, text: String) -> Self {
        Self {
            id,
            text,
            needs_focus: true,
        }
    }
}

#[derive(Default)]
pub struct LayersPanel {
    /// Layer being renamed, and the name typed so far.
    renaming: Option<Rename>,
    /// Thumbnails by (layer id, is mask).
    thumbs: std::collections::HashMap<(u64, bool), Thumb>,
    /// Layer being dragged to a new place in the stack.
    dragging: Option<u64>,
    /// A command asked for from inside a row (double-click on the row or its
    /// thumbnail).
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
            .multiply_with_opacity(if dimmed { 0.5 } else { 1.0 });
        // The whole row senses clicks and drags beneath its widgets, so the
        // eye, thumbnails and name still get their own clicks.
        let builder = egui::UiBuilder::new()
            .id(row_id(id))
            .sense(Sense::click_and_drag());
        let row = ui.scope_builder(builder, |ui| {
            frame
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
                        let pixel_thumb = if adjustment {
                            // Not selectable, so clicks fall through to the row.
                            ui.add(
                                egui::Label::new(
                                    RichText::new(SLIDERS).size(18.0).color(theme.accent),
                                )
                                .selectable(false),
                            )
                            .on_hover_text("Adjustment layer");
                            None
                        } else {
                            let targeted = selected && editor.target == Target::Pixels;
                            let response = self.thumbnail(ui, editor, id, false, targeted, theme);
                            if response.double_clicked() {
                                self.command = Some(Command::BlendingOptions);
                            } else if response.clicked() {
                                editor.active = id;
                                editor.target = Target::Pixels;
                            }
                            Some(response)
                        };

                        if let Some(enabled) = mask {
                            let targeted = selected && editor.target == Target::Mask;
                            let response = self
                                .thumbnail(ui, editor, id, true, targeted, theme)
                                .on_hover_text(
                                    "Layer mask — click to paint on it, Shift+click to disable",
                                );
                            if response.secondary_clicked() {
                                editor.active = id;
                                editor.target = Target::Mask;
                            }
                            response.context_menu(|ui| {
                                mask_context_menu(ui, editor, &mut self.command, id, enabled);
                            });
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
                                        if let Some(m) =
                                            doc.layer_mut(id).and_then(|l| l.mask.as_mut())
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

                        let name_response =
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if blend != BlendMode::Normal {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(blend.name())
                                                .small()
                                                .color(theme.dark_foreground),
                                        )
                                        .selectable(false),
                                    );
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    self.name(ui, editor, id, &name, selected)
                                })
                                .inner
                            })
                            .inner;

                        (pixel_thumb, name_response)
                    })
                    .inner
                })
                .inner
        });

        let (pixel_thumb, name_response) = row.inner;
        let response = row.response;
        if response.drag_started() && editor.busy().is_none() {
            self.dragging = Some(id);
        }
        if response.double_clicked() {
            self.command = Some(Command::BlendingOptions);
        } else if response.clicked() {
            select(editor, id);
        }

        let row_secondary = response.secondary_clicked()
            || name_response.secondary_clicked()
            || pixel_thumb.as_ref().is_some_and(|r| r.secondary_clicked());
        if row_secondary {
            select(editor, id);
        }
        let mut popup = egui::Popup::context_menu(&response);
        if row_secondary {
            popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
        }
        popup.show(|ui| {
            layer_context_menu(ui, editor, &mut self.command, &mut self.renaming, id);
        });

        response.rect
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
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let response = ui.interact(rect, thumb_id(id, mask), Sense::click());
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

    fn name(
        &mut self,
        ui: &mut Ui,
        editor: &mut Editor,
        id: u64,
        name: &str,
        selected: bool,
    ) -> egui::Response {
        if let Some(rename) = &mut self.renaming
            && rename.id == id
        {
            let response = ui.add(TextEdit::singleline(&mut rename.text).desired_width(ui.available_width()));
            if rename.needs_focus {
                response.request_focus();
                let mut state = TextEdit::load_state(ui.ctx(), response.id).unwrap_or_default();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::default(),
                    egui::text::CCursor::new(rename.text.chars().count()),
                )));
                TextEdit::store_state(ui.ctx(), response.id, state);
                rename.needs_focus = false;
            } else {
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let done = response.lost_focus() || enter;
                let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if done || cancelled {
                    let new_name = rename.text.trim().to_owned();
                    self.renaming = None;
                    if done && !cancelled && !new_name.is_empty() && new_name != name {
                        editor.edit("Rename Layer", |doc, _| {
                            if let Some(l) = doc.layer_mut(id) {
                                l.name = new_name;
                            }
                        });
                    }
                }
            }
            return response;
        }
        let text = if selected {
            RichText::new(name).strong()
        } else {
            RichText::new(name)
        };
        // Not selectable: selectable labels grab drags, which would stop the
        // row being dragged by its name.
        let label = egui::Label::new(text).truncate().selectable(false);
        let resp = ui.add(label);
        let response = ui.interact(resp.rect, name_id(id), Sense::click());
        if response.double_clicked() {
            self.renaming = Some(Rename::new(id, name.to_owned()));
        } else if response.clicked() {
            select(editor, id);
        }
        response
    }
}

/// A layer row's id, which follows the layer when the stack is reordered.
fn row_id(layer: u64) -> egui::Id {
    egui::Id::new(("layer-row", layer))
}

fn thumb_id(layer: u64, mask: bool) -> egui::Id {
    row_id(layer).with(("thumb", layer, mask))
}

fn name_id(layer: u64) -> egui::Id {
    row_id(layer).with(("name", layer))
}

fn menu_item(
    ui: &mut Ui,
    text: &str,
    shortcut: Option<String>,
    enabled: bool,
    action: impl FnOnce(),
) {
    let mut button = Button::new(text);
    if let Some(s) = shortcut {
        button = button.shortcut_text(s);
    }
    if ui.add_enabled(enabled, button).clicked() {
        action();
        ui.close();
    }
}

fn mask_menu_items(
    ui: &mut Ui,
    editor: &mut Editor,
    command: &mut Option<Command>,
    id: u64,
    enabled: bool,
) {
    menu_item(ui, "Delete Layer Mask", None, true, || {
        *command = Some(Command::DeleteMask);
    });

    let toggle_label = if enabled {
        "Disable Layer Mask"
    } else {
        "Enable Layer Mask"
    };
    menu_item(ui, toggle_label, None, true, || {
        *command = Some(Command::ToggleMask);
    });

    menu_item(
        ui,
        "Invert Mask",
        Command::Invert
            .shortcut()
            .map(|s| ui.ctx().format_shortcut(&s)),
        true,
        || {
            editor.active = id;
            editor.target = Target::Mask;
            *command = Some(Command::Invert);
        },
    );

    let viewing = editor.view() == View::Mask(id);
    let view_label = if viewing {
        "Exit Mask View"
    } else {
        "View Mask"
    };
    menu_item(ui, view_label, Some("Alt+click".into()), true, || {
        editor.active = id;
        editor.target = Target::Mask;
        editor.set_view(if viewing { View::Image } else { View::Mask(id) });
    });

    let overlay_label = if editor.view() == View::MaskOverlay(id) {
        "Hide Mask Overlay"
    } else {
        "Show Mask Overlay"
    };
    menu_item(ui, overlay_label, Some("\\".into()), true, || {
        editor.active = id;
        *command = Some(Command::MaskOverlay);
    });
}

fn mask_context_menu(
    ui: &mut Ui,
    editor: &mut Editor,
    command: &mut Option<Command>,
    id: u64,
    enabled: bool,
) {
    mask_menu_items(ui, editor, command, id, enabled);
}

fn layer_context_menu(
    ui: &mut Ui,
    editor: &mut Editor,
    command: &mut Option<Command>,
    renaming: &mut Option<Rename>,
    id: u64,
) {
    let Some(layer) = editor.doc.layer(id) else {
        return;
    };
    let has_mask = layer.mask.is_some();
    let mask_enabled = layer.mask.as_ref().is_some_and(|m| m.enabled);
    let index = editor.doc.layers.iter().position(|l| l.id == id);
    let can_merge_down =
        index.is_some_and(|i| i > 0 && editor.doc.layers[i - 1].adjustment.is_none());
    let can_delete = editor.doc.layers.len() > 1;

    menu_item(ui, "Blending Options…", None, true, || {
        *command = Some(Command::BlendingOptions);
    });

    ui.separator();

    menu_item(
        ui,
        "Duplicate Layer",
        Command::DuplicateLayer
            .shortcut()
            .map(|s| ui.ctx().format_shortcut(&s)),
        true,
        || {
            *command = Some(Command::DuplicateLayer);
        },
    );

    menu_item(ui, "Delete Layer", None, can_delete, || {
        *command = Some(Command::DeleteLayer);
    });

    menu_item(ui, "Rename", None, true, || {
        if let Some(l) = editor.doc.layer(id) {
            *renaming = Some(Rename::new(id, l.name.clone()));
        }
    });

    ui.separator();

    if has_mask {
        mask_menu_items(ui, editor, command, id, mask_enabled);
    } else {
        menu_item(ui, "Add Layer Mask", None, true, || {
            *command = Some(Command::AddMask);
        });
    }

    ui.separator();

    menu_item(
        ui,
        "Merge Down",
        Command::MergeDown
            .shortcut()
            .map(|s| ui.ctx().format_shortcut(&s)),
        can_merge_down,
        || {
            *command = Some(Command::MergeDown);
        },
    );
}

/// Select a layer as clicking its row does: paint on its pixels, or on its
/// mask for an adjustment layer (which has no pixels).
fn select(editor: &mut Editor, id: u64) {
    editor.active = id;
    let adjustment = editor.doc.layer(id).is_some_and(|l| l.adjustment.is_some());
    editor.target = if adjustment {
        Target::Mask
    } else {
        Target::Pixels
    };
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
    use egui::{Event, PointerButton, pos2, vec2};
    use omapix_engine::adjust::{Adjustment, Curves};
    use omapix_engine::layer::Layer;
    use omapix_engine::{ColorProfile, Document, Raster};

    /// The panel running headless, driven by synthetic mouse events.
    struct Harness {
        ctx: egui::Context,
        panel: LayersPanel,
        editor: Editor,
        theme: Theme,
        time: f64,
    }

    /// Layer ids, top first: an adjustment, a Multiply layer, the background.
    const CURVES: u64 = 100;
    const MULTIPLY: u64 = 101;

    impl Harness {
        fn new() -> Self {
            let (w, h) = (60, 40);
            let image = Raster::new(w, h, vec![[30000, 30000, 30000, 65535]; (w * h) as usize]);
            let mut doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
            let mut multiply = Layer::empty(MULTIPLY, "Layer 1", w, h);
            multiply.blend = BlendMode::Multiply;
            doc.layers.push(multiply);
            let curves = Adjustment::Curves(Curves::default());
            doc.layers.push(Layer::adjustment(CURVES, curves, w, h));
            let mut harness = Self {
                ctx: egui::Context::default(),
                panel: LayersPanel::default(),
                editor: Editor::new(doc).unwrap(),
                theme: Theme::default(),
                time: 0.0,
            };
            harness.frame(vec![]);
            harness
        }

        fn background(&self) -> u64 {
            self.editor.doc.layers[0].id
        }

        fn top_first(&self) -> Vec<u64> {
            self.editor.doc.layers.iter().rev().map(|l| l.id).collect()
        }

        fn frame(&mut self, events: Vec<Event>) -> Option<Command> {
            self.time += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    pos2(0.0, 0.0),
                    vec2(300.0, 600.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (panel, editor, theme) = (&mut self.panel, &mut self.editor, &self.theme);
            let mut command = None;
            let mut output = self
                .ctx
                .run_ui(input, |ui| command = panel.show(ui, editor, theme));
            // There's no renderer to upload textures to.
            output.textures_delta.clear();
            command
        }

        /// A point in a row's empty space, over the blend-mode label if any.
        fn row_point(&self, layer: u64) -> egui::Pos2 {
            let rect = self.ctx.read_response(row_id(layer)).unwrap().rect;
            pos2(rect.right() - 8.0, rect.center().y)
        }

        fn button(&mut self, pos: egui::Pos2, pressed: bool) -> Option<Command> {
            self.frame(vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ])
        }

        /// Click once, long enough after any earlier click not to double it.
        fn click(&mut self, pos: egui::Pos2) -> Option<Command> {
            self.time += 1.0;
            self.button(pos, true).or(self.button(pos, false))
        }

        fn name_point(&self, layer: u64) -> egui::Pos2 {
            self.ctx.read_response(name_id(layer)).unwrap().rect.center()
        }

        fn mask_point(&self, layer: u64) -> egui::Pos2 {
            self.ctx.read_response(thumb_id(layer, true)).unwrap().rect.center()
        }

        fn secondary_button(&mut self, pos: egui::Pos2, pressed: bool) -> Option<Command> {
            self.frame(vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed,
                    modifiers: Default::default(),
                },
            ])
        }

        fn secondary_click(&mut self, pos: egui::Pos2) -> Option<Command> {
            self.time += 1.0;
            self.secondary_button(pos, true)
                .or(self.secondary_button(pos, false))
        }

        fn double_click(&mut self, pos: egui::Pos2) -> Option<Command> {
            self.click(pos);
            self.button(pos, true).or(self.button(pos, false))
        }
    }

    #[test]
    fn right_clicking_a_row_selects_and_opens_menu() {
        let mut h = Harness::new();
        let background = h.background();
        let pos = h.row_point(background);
        h.secondary_click(pos);
        assert_eq!(h.editor.active, background);
        assert_eq!(h.editor.target, Target::Pixels);
        assert!(egui::Popup::is_id_open(
            &h.ctx,
            row_id(background).with("popup")
        ));

        // Right-clicking the name also opens the row's context menu.
        let name_pos = h.name_point(MULTIPLY);
        h.secondary_click(name_pos);
        assert_eq!(h.editor.active, MULTIPLY);
        assert!(egui::Popup::is_id_open(
            &h.ctx,
            row_id(MULTIPLY).with("popup")
        ));
    }

    #[test]
    fn layer_and_mask_context_menus_render() {
        let mut h = Harness::new();
        let background = h.background();
        let mut command = None;
        let mut renaming = None;

        // Background layer: no mask, bottom layer.
        let mut out = h.ctx.run_ui(Default::default(), |ui| {
            layer_context_menu(ui, &mut h.editor, &mut command, &mut renaming, background);
        });
        out.textures_delta.clear();

        // Curves layer: has mask, adjustment layer.
        let mut out = h.ctx.run_ui(Default::default(), |ui| {
            layer_context_menu(ui, &mut h.editor, &mut command, &mut renaming, CURVES);
            mask_context_menu(ui, &mut h.editor, &mut command, CURVES, true);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn right_clicking_mask_thumbnail_targets_mask_and_opens_mask_menu() {
        let mut h = Harness::new();
        let mask_pos = h.mask_point(CURVES);
        h.secondary_click(mask_pos);
        assert_eq!(h.editor.active, CURVES);
        assert_eq!(h.editor.target, Target::Mask);
        assert!(egui::Popup::is_id_open(
            &h.ctx,
            thumb_id(CURVES, true).with("popup")
        ));
    }

    #[test]
    fn clicking_anywhere_on_a_row_selects_the_layer() {
        let mut h = Harness::new();
        let background = h.background();

        h.editor.target = Target::Mask;
        h.click(h.row_point(background));
        assert_eq!(h.editor.active, background);
        assert_eq!(h.editor.target, Target::Pixels);

        // Over the "Multiply" label, which used to swallow the click.
        h.click(h.row_point(MULTIPLY));
        assert_eq!(h.editor.active, MULTIPLY);
        assert_eq!(h.editor.target, Target::Pixels);

        // An adjustment layer has no pixels, so its mask is painted.
        h.click(h.row_point(CURVES));
        assert_eq!(h.editor.active, CURVES);
        assert_eq!(h.editor.target, Target::Mask);

        assert_eq!(h.top_first(), [CURVES, MULTIPLY, background]);
    }

    #[test]
    fn double_clicking_a_row_opens_blending_options() {
        let mut h = Harness::new();
        let command = h.double_click(h.row_point(MULTIPLY));
        assert_eq!(command, Some(Command::BlendingOptions));
        assert_eq!(h.editor.active, MULTIPLY);
    }

    #[test]
    fn rows_still_drag_to_reorder() {
        let mut h = Harness::new();
        let background = h.background();
        // Drag the background (by its empty space) above the top row.
        let from = h.row_point(background);
        let to = h.row_point(CURVES) - vec2(0.0, 20.0);
        h.button(from, true);
        for i in 1..=5 {
            let pos = from + (to - from) * (i as f32 / 5.0);
            h.frame(vec![Event::PointerMoved(pos)]);
        }
        h.button(to, false);
        assert_eq!(h.top_first(), [background, CURVES, MULTIPLY]);
        assert_eq!(h.panel.dragging, None);
    }

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

    #[test]
    fn renaming_submits_on_enter() {
        let mut h = Harness::new();
        let name_pos = h.name_point(MULTIPLY);
        h.double_click(name_pos);
        assert!(h.panel.renaming.is_some());

        // First frame: focus is requested.
        h.frame(vec![]);
        assert!(h.panel.renaming.is_some());

        // Second frame: user types text and presses Enter.
        h.frame(vec![
            Event::Text("Shading".into()),
            Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        ]);
        assert_eq!(h.panel.renaming, None);
        assert_eq!(h.editor.doc.layer(MULTIPLY).unwrap().name, "Shading");
    }

    #[test]
    fn renaming_submits_on_click_off() {
        let mut h = Harness::new();
        let name_pos = h.name_point(MULTIPLY);
        let bg_pos = h.row_point(h.background());
        h.double_click(name_pos);
        assert!(h.panel.renaming.is_some());

        // First frame: focus is requested.
        h.frame(vec![]);
        assert!(h.panel.renaming.is_some());

        // Second frame: user types text.
        h.frame(vec![Event::Text("Shading".into())]);
        assert!(h.panel.renaming.is_some());

        // Click off on another row.
        h.click(bg_pos);
        assert_eq!(h.panel.renaming, None);
        assert_eq!(h.editor.doc.layer(MULTIPLY).unwrap().name, "Shading");
    }

    #[test]
    fn renaming_cancels_on_escape() {
        let mut h = Harness::new();
        let name_pos = h.name_point(MULTIPLY);
        h.double_click(name_pos);
        assert!(h.panel.renaming.is_some());

        // First frame: focus is requested.
        h.frame(vec![]);
        assert!(h.panel.renaming.is_some());

        // Second frame: user types text and presses Escape.
        h.frame(vec![
            Event::Text("Shading".into()),
            Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        ]);
        assert_eq!(h.panel.renaming, None);
        assert_eq!(h.editor.doc.layer(MULTIPLY).unwrap().name, "Layer 1");
    }
}
