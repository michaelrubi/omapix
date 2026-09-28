//! The Layers panel, laid out like Photoshop's: blend mode and opacity for
//! the selected layer at the top, the stack (top layer first) in the
//! middle, and layer buttons at the bottom.

use std::collections::HashSet;

use egui::{Align, Button, ComboBox, Layout, RichText, ScrollArea, Sense, Slider, TextEdit, Ui};
use omapix_engine::groups::Place;
use omapix_engine::selection::{Combine, Selection};
use omapix_engine::tiled::Tiled;
use omapix_engine::{BlendMode, DisplayTransform, Document, Pixel, ops};

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
const FOLDER: &str = "\u{f07b}";
const FOLDER_OPEN: &str = "\u{f07c}";
const CARET_RIGHT: &str = "\u{f0da}";
const CARET_DOWN: &str = "\u{f0d7}";
const CLIPPED: &str = "\u{f149}";
const LOCK: &str = "\u{f023}";
const LOCK_OUTLINE: &str = "\u{f0340}";
const LOCK_TRANSPARENT: &str = "\u{f0128}";
const LOCK_PIXELS: &str = "\u{f1fc}";
const LOCK_POSITION: &str = "\u{f047}";

/// How far each level of group nesting is indented, in points.
const INDENT: f32 = 14.0;

/// Width of the show/hide column, in points.
const EYE_WIDTH: f32 = 18.0;

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
    /// The last layer clicked without Shift, where Shift+click selects from.
    anchor: Option<u64>,
    /// Groups shown open. Groups start closed, and open to show the
    /// selected layer.
    expanded: HashSet<u64>,
    /// A command asked for from inside a row (double-click on the row or its
    /// thumbnail).
    command: Option<Command>,
    /// The rows drawn last frame, top first, to find the line between two.
    rows: Vec<(u64, egui::Rect)>,
    /// The layer above the line between rows the pointer is on with Alt
    /// held, where a click clips it to the layer below or releases it.
    clip_line: Option<u64>,
}

impl LayersPanel {
    /// The rows shown, top of the stack first as in Photoshop, leaving out
    /// what's in closed groups. The groups the active layer is in count as
    /// open, as the panel opens them to show it.
    fn rows(&self, doc: &Document, active: u64) -> Vec<u64> {
        let open: HashSet<u64> = ancestors(doc, active).collect();
        doc.layers
            .iter()
            .rev()
            .filter(|l| ancestors(doc, l.id).all(|g| self.expanded.contains(&g) || open.contains(&g)))
            .map(|l| l.id)
            .collect()
    }

    /// The layer on the row above (`up`) or below the active one's, if any.
    pub fn row_beside(&self, doc: &Document, active: u64, up: bool) -> Option<u64> {
        let rows = self.rows(doc, active);
        let at = rows.iter().position(|&id| id == active)?;
        if up {
            at.checked_sub(1).map(|i| rows[i])
        } else {
            rows.get(at + 1).copied()
        }
    }

    /// Alt+] / Alt+[: select just the layer on the row above or below, as
    /// clicking it does. Does nothing at the top or bottom.
    pub fn step_selection(&mut self, editor: &mut Editor, up: bool) {
        if let Some(id) = self.row_beside(&editor.doc, editor.active, up) {
            self.pick(editor, id);
        }
    }

    /// Select layer `id` alone, as clicking its row does.
    pub fn pick(&mut self, editor: &mut Editor, id: u64) {
        select(editor, id);
        self.anchor = Some(id);
    }

    /// Open or close group `id`.
    #[cfg(test)]
    pub fn set_group_expanded(&mut self, id: u64, expanded: bool) {
        if expanded {
            self.expanded.insert(id);
        } else {
            self.expanded.remove(&id);
        }
    }

    /// Open or close a group. Alt+click opens or closes it and every group
    /// inside it. Closing a group that contains the active layer selects
    /// the group, as in Photoshop.
    fn toggle_group(&mut self, editor: &mut Editor, id: u64, recursive: bool) {
        let open = !self.expanded.contains(&id);
        let mut targets = vec![id];
        if recursive {
            targets.extend(
                editor
                    .doc
                    .layers
                    .iter()
                    .filter(|l| l.is_group && editor.doc.is_inside(l.id, id))
                    .map(|l| l.id),
            );
        }
        for g in targets {
            if open {
                self.expanded.insert(g);
            } else {
                self.expanded.remove(&g);
            }
        }
        if !open && editor.doc.is_inside(editor.active, id) {
            select(editor, id);
        }
    }

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
                let doc = &editor.doc;
                self.thumbs.retain(|(id, _), _| doc.layer(*id).is_some());
                self.expanded.retain(|id| doc.layer(*id).is_some_and(|l| l.is_group));
                // Show the selected layer, wherever it is.
                self.expanded.extend(ancestors(doc, editor.active));
                let ids = self.rows(doc, editor.active);
                self.clip_line(ui, editor, theme);
                let mut rows = Vec::with_capacity(ids.len());
                for &id in &ids {
                    let rect = self.row(ui, editor, theme, id);
                    rows.push((id, rect));
                }
                self.reorder(ui, editor, theme, &rows);
                self.rows = rows;
            });

        ui.separator();
        ui.horizontal(|ui| {
            let busy = editor.busy().is_some();
            let buttons = [
                (FOLDER, Command::NewGroup, "New group (Ctrl+G groups the layer)"),
                (PLUS, Command::NewLayer, "New layer (Ctrl+Shift+N)"),
                (COPY, Command::DuplicateLayer, "Duplicate layer (Ctrl+J)"),
                (MASK, Command::AddMask, "Add layer mask (Alt+click: hide all)"),
                (TRASH, Command::DeleteLayer, "Delete layer"),
            ];
            for (icon, cmd, tip) in buttons {
                if ui
                    .add_enabled(!busy, Button::new(icon).frame(false))
                    .on_hover_text(tip)
                    .clicked()
                {
                    // Alt+click adds a black mask, as in Photoshop.
                    let hide = cmd == Command::AddMask && ui.input(|i| i.modifiers.alt);
                    command = Some(if hide { Command::AddMaskHideAll } else { cmd });
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
        let is_group = layer.is_group;
        let can_modify = layer.can_modify();
        let locks = layer.locks;
        let has_pixels = layer.has_pixels();

        ui.add_enabled_ui(can_modify, |ui| {
            ComboBox::from_id_salt("blend-mode")
                .selected_text(blend.name())
                .width(ui.available_width())
                .height(600.0)
                .show_ui(ui, |ui| {
                    if is_group {
                        let mode = BlendMode::PassThrough;
                        ui.selectable_value(&mut blend, mode, mode.name());
                        ui.separator();
                    }
                    for (i, group) in BlendMode::MENU.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        for &mode in *group {
                            ui.selectable_value(&mut blend, mode, mode.name());
                        }
                    }
                });
        });
        if can_modify && blend != layer.blend {
            editor.edit("Blend Mode", |doc, _| {
                if let Some(l) = doc.layer_mut(id) {
                    l.blend = blend;
                }
            });
        }

        ui.add_enabled_ui(can_modify, |ui| {
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
        });
        ui.horizontal(|ui| {
            ui.label("Lock");
            let buttons = [
                (LOCK_TRANSPARENT, locks.transparency, has_pixels, "Lock transparent pixels (/)", Command::LockTransparent),
                (LOCK_PIXELS, locks.pixels, has_pixels, "Lock image pixels", Command::LockPixels),
                (LOCK_POSITION, locks.position, true, "Lock position", Command::LockPosition),
                (LOCK, locks.all, true, "Lock all (Ctrl+/)", Command::LockAll),
            ];
            for (icon, locked, enabled, tip, cmd) in buttons {
                let button = Button::selectable(locked, icon);
                if ui.add_enabled(enabled, button).on_hover_text(tip).clicked() {
                    self.command = Some(cmd);
                }
            }
        });
    }

    /// Alt over the line between two rows shows it can be clicked to clip
    /// the upper layer to the lower one, or release it, as in Photoshop.
    fn clip_line(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme) {
        self.clip_line = None;
        if self.dragging.is_some() || editor.busy().is_some() {
            return;
        }
        let (alt, pointer) = ui.input(|i| (i.modifiers.alt, i.pointer.hover_pos()));
        let (true, Some(pointer)) = (alt, pointer) else {
            return;
        };
        let doc = &editor.doc;
        let over_caret = self.rows.iter().any(|(id, _)| {
            doc.layer(*id).is_some_and(|l| l.is_group)
                && ui
                    .ctx()
                    .read_response(caret_id(*id))
                    .is_some_and(|r| r.rect.contains(pointer))
        });
        if over_caret {
            return;
        }
        let line = self.rows.windows(2).find_map(|pair| {
            let [(upper, above), (lower, below)] = pair else {
                return None;
            };
            let y = (above.bottom() + below.top()) / 2.0;
            let near = (pointer.y - y).abs() <= 4.0 && above.x_range().contains(pointer.x);
            let parent = |id: &u64| doc.layer(*id).map(|l| l.parent);
            (near && parent(upper) == parent(lower)).then_some((*upper, y, above.x_range()))
        });
        let Some((upper, y, x)) = line else {
            return;
        };
        self.clip_line = Some(upper);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Alias);
        ui.painter()
            .hline(x, y, egui::Stroke::new(2.0, theme.accent));
        if ui.input(|i| i.pointer.primary_clicked()) {
            let clipped = doc.layer(upper).is_some_and(|l| l.clipped);
            let label = if clipped {
                "Release Clipping Mask"
            } else {
                "Create Clipping Mask"
            };
            editor.edit(label, |doc, _| {
                doc.toggle_clipping(upper);
            });
        }
    }

    /// While a row is dragged, show where it would land; on release, move it.
    fn reorder(
        &mut self,
        ui: &mut Ui,
        editor: &mut Editor,
        theme: &Theme,
        rows: &[(u64, egui::Rect)],
    ) {
        let Some(dragged) = self.dragging else { return };
        let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        // Dragging one of several selected layers drags them all.
        let moving = if editor.is_selected(dragged) {
            editor.selected()
        } else {
            vec![dragged]
        };
        let target = drop_target(&editor.doc, rows, &self.expanded, pointer)
            .filter(|d| can_drop(&editor.doc, &moving, d.place));
        if let Some(Drop { marker, .. }) = &target {
            let stroke = egui::Stroke::new(2.0, theme.accent);
            match *marker {
                Marker::Line { y, depth, left, right } => {
                    // Indented to the level it would land at.
                    let left = left + EYE_WIDTH + depth as f32 * INDENT;
                    ui.painter().hline(left..=right, y, stroke);
                }
                Marker::Row(rect) => {
                    ui.painter()
                        .rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
                }
            }
        }
        if ui.input(|i| i.pointer.any_down()) {
            return;
        }
        self.dragging = None;
        let Some(Drop { place, .. }) = target else {
            return;
        };
        if let Place::IntoTop(g) | Place::IntoBottom(g) = place {
            self.expanded.insert(g);
        }
        // Dropping where it already is isn't an edit.
        let mut moved = editor.doc.clone();
        moved.move_layers(&moving, place);
        let shape = |doc: &Document| -> Vec<(u64, Option<u64>)> {
            doc.layers.iter().map(|l| (l.id, l.parent)).collect()
        };
        if shape(&moved) != shape(&editor.doc) {
            let label = if moving.len() > 1 { "Move Layers" } else { "Move Layer" };
            editor.edit(label, |doc, _| {
                doc.move_layers(&moving, place);
            });
        }
    }

    /// Draw one layer row; returns its rectangle.
    fn row(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme, id: u64) -> egui::Rect {
        let Some(layer) = editor.doc.layer(id) else {
            return egui::Rect::NOTHING;
        };
        // Every selected row is highlighted; the active one's name is bold
        // and its thumbnail outlined.
        let selected = editor.active == id;
        let highlighted = editor.is_selected(id);
        let (visible, name, blend, mask, adjustment, is_group) = (
            layer.visible,
            layer.name.clone(),
            layer.blend,
            layer.mask.as_ref().map(|m| m.enabled),
            layer.adjustment.is_some(),
            layer.is_group,
        );
        let depth = editor.doc.depth(id);
        let clipped = editor.doc.clip_base(id).is_some();
        let clip_base = editor.doc.is_clip_base(id);
        let locks = editor.doc.layer(id).map(|l| l.locks).unwrap_or_default();
        // Shown only if every group it's in is shown too.
        let doc = &editor.doc;
        let shown = visible && ancestors(doc, id).all(|g| doc.layer(g).is_some_and(|l| l.visible));
        let expanded = self.expanded.contains(&id);
        let has_groups = doc.layers.iter().any(|l| l.is_group);
        let fill = if highlighted {
            theme.selection
        } else {
            egui::Color32::TRANSPARENT
        };
        let dimmed = self
            .dragging
            .is_some_and(|d| d == id || editor.is_selected(d) && highlighted);
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
                        let eye_colour = if shown {
                            theme.foreground
                        } else {
                            theme.dark_foreground
                        };
                        let eye_button = ui
                            .add_sized(
                                [EYE_WIDTH, 18.0],
                                Button::new(RichText::new(eye).color(eye_colour)).frame(false),
                            )
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

                        if depth > 0 {
                            ui.add_space(depth as f32 * INDENT);
                        }
                        if is_group {
                            let caret = if expanded { CARET_DOWN } else { CARET_RIGHT };
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(12.0, 18.0), Sense::hover());
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                caret,
                                egui::TextStyle::Button.resolve(ui.style()),
                                theme.dark_foreground,
                            );
                            let toggle = ui
                                .interact(rect, caret_id(id), Sense::click())
                                .on_hover_text(if expanded { "Close group" } else { "Open group" });
                            if toggle.clicked() {
                                let alt = ui.input(|i| i.modifiers.alt);
                                self.toggle_group(editor, id, alt);
                            }
                        } else if has_groups {
                            // Line layers up with groups at the same level.
                            ui.add_space(12.0 + ui.spacing().item_spacing.x);
                        }
                        if clipped {
                            // Indented, with an arrow down to what it's
                            // clipped to, as in Photoshop.
                            ui.add(
                                egui::Label::new(
                                    RichText::new(CLIPPED).color(theme.dark_foreground),
                                )
                                .selectable(false),
                            )
                            .on_hover_text("Clipped to the layer below (Ctrl+Alt+G releases)");
                        }

                        // Pixel thumbnail (or the adjustment or folder icon),
                        // then the mask thumbnail. Clicking one picks what
                        // painting applies to.
                        let pixel_thumb = if is_group {
                            let folder = if expanded { FOLDER_OPEN } else { FOLDER };
                            ui.add_sized(
                                [THUMB, 24.0],
                                egui::Label::new(RichText::new(folder).size(20.0).color(theme.accent))
                                    .selectable(false),
                            )
                            .on_hover_text("Layer group");
                            None
                        } else if adjustment {
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
                            let (command, shift, alt) =
                                ui.input(|i| (i.modifiers.command, i.modifiers.shift, i.modifiers.alt));
                            if response.double_clicked() {
                                self.command = Some(Command::BlendingOptions);
                            } else if response.clicked() && command {
                                if let Some(layer) = editor.doc.layer(id).filter(|l| l.has_pixels()) {
                                    let how = Combine::from_modifiers(shift, alt);
                                    let sel = Selection::from_alpha(&layer.pixels);
                                    editor.set_selection("Load Selection", sel, how);
                                }
                            } else if response.clicked() && shift {
                                self.click(ui, editor, id);
                            } else if response.clicked() {
                                select(editor, id);
                                editor.target = Target::Pixels;
                                self.anchor = Some(id);
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
                                let (command, shift, alt) =
                                    ui.input(|i| (i.modifiers.command, i.modifiers.shift, i.modifiers.alt));
                                if command {
                                    load_mask(editor, id, Combine::from_modifiers(shift, alt));
                                } else if alt {
                                    // Alt+click shows the mask on its own, or goes back.
                                    editor.active = id;
                                    editor.target = Target::Mask;
                                    let view = if editor.view() == View::Mask(id) {
                                        View::Image
                                    } else {
                                        View::Mask(id)
                                    };
                                    editor.set_view(view);
                                } else if shift {
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
                                if locks.any() {
                                    let (icon, tooltip) = if locks.all {
                                        (LOCK, "All locked")
                                    } else {
                                        (LOCK_OUTLINE, "Partially locked")
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(icon).small().color(theme.dark_foreground),
                                        )
                                        .selectable(false),
                                    )
                                    .on_hover_text(tooltip);
                                }
                                if blend != BlendMode::Normal && blend != BlendMode::PassThrough {
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
                                    self.name(ui, editor, id, &name, selected, clip_base)
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
        // A click on the line between rows with Alt held is for clipping.
        let on_line = self.clip_line.is_some();
        let thumb_clicked = pixel_thumb.as_ref().is_some_and(|r| r.clicked())
            || ui.ctx().read_response(thumb_id(id, true)).is_some_and(|r| r.clicked());
        if response.drag_started() && editor.busy().is_none() && !on_line {
            self.dragging = Some(id);
        }
        if response.double_clicked() && !on_line {
            self.command = Some(Command::BlendingOptions);
        } else if response.clicked() && !on_line && !thumb_clicked {
            self.click(ui, editor, id);
        }

        let row_secondary = response.secondary_clicked()
            || name_response.secondary_clicked()
            || pixel_thumb.as_ref().is_some_and(|r| r.secondary_clicked());
        if row_secondary {
            // Right-clicking one of several selected layers keeps them all
            // selected, for the menu to act on.
            let others = if editor.is_selected(id) {
                editor.selected()
            } else {
                Vec::new()
            };
            select_with(editor, id, others);
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
        clip_base: bool,
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
        let mut text = RichText::new(name);
        if selected {
            text = text.strong();
        }
        if clip_base {
            // Photoshop underlines the layer others are clipped to.
            text = text.underline();
        }
        // Not selectable: selectable labels grab drags, which would stop the
        // row being dragged by its name.
        let label = egui::Label::new(text).truncate().selectable(false);
        let resp = ui.add(label);
        let response = ui.interact(resp.rect, name_id(id), Sense::click());
        if response.double_clicked() {
            self.renaming = Some(Rename::new(id, name.to_owned()));
        } else if response.clicked() {
            self.click(ui, editor, id);
        }
        response
    }

    /// Select a layer as clicking its row does. As in Photoshop, Ctrl+click
    /// adds it to the selected layers or takes it out, and Shift+click
    /// selects the rows from the last one clicked to it (Ctrl+Shift adds
    /// them). The layer clicked becomes the active one.
    fn click(&mut self, ui: &Ui, editor: &mut Editor, id: u64) {
        let (command, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        if shift {
            let rows: Vec<u64> = self.rows.iter().map(|(id, _)| *id).collect();
            let anchor = self.anchor.unwrap_or(editor.active);
            let at = |id| rows.iter().position(|&r| r == id);
            let (Some(a), Some(b)) = (at(anchor), at(id)) else {
                select(editor, id);
                return;
            };
            let mut ids = if command { editor.selected() } else { Vec::new() };
            ids.extend_from_slice(&rows[a.min(b)..=a.max(b)]);
            select_with(editor, id, ids);
            return;
        }
        self.anchor = Some(id);
        if !command {
            select(editor, id);
            return;
        }
        let mut ids = editor.selected();
        if !editor.is_selected(id) {
            ids.push(id);
            select_with(editor, id, ids);
        } else if ids.len() > 1 {
            // Taking out the active layer makes the top one left active.
            ids.retain(|&s| s != id);
            let active = if id == editor.active {
                *ids.last().expect("more than one")
            } else {
                editor.active
            };
            select_with(editor, active, ids);
        }
    }
}

/// A layer row's id, which follows the layer when the stack is reordered.
fn row_id(layer: u64) -> egui::Id {
    egui::Id::new(("layer-row", layer))
}

fn thumb_id(layer: u64, mask: bool) -> egui::Id {
    row_id(layer).with(("thumb", layer, mask))
}

fn caret_id(layer: u64) -> egui::Id {
    row_id(layer).with(("caret", layer))
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

    menu_item(ui, Command::MaskDensity.label(), None, true, || {
        editor.active = id;
        *command = Some(Command::MaskDensity);
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

    ui.separator();

    let has_selection = editor.doc.selection.is_some();
    let loads = [
        ("Add Mask to Selection", Combine::Add, true),
        ("Subtract Mask from Selection", Combine::Subtract, has_selection),
        ("Intersect Mask with Selection", Combine::Intersect, has_selection),
    ];
    for (label, how, enabled) in loads {
        menu_item(ui, label, None, enabled, || load_mask(editor, id, how));
    }

    ui.separator();
    let unlocked = editor.doc.layer(id).is_some_and(|l| !l.locks.all);
    let saves = [
        ("Replace Mask with Selection", Combine::Replace),
        ("Add Selection to Mask", Combine::Add),
        ("Subtract Selection from Mask", Combine::Subtract),
        ("Intersect Selection with Mask", Combine::Intersect),
    ];
    for (label, how) in saves {
        menu_item(ui, label, None, has_selection && unlocked, || {
            selection_to_mask(editor, id, label, how)
        });
    }
}

/// Load layer `id`'s mask as a selection, combined with the current one
/// (Ctrl+click on the mask thumbnail, or its context menu).
fn load_mask(editor: &mut Editor, id: u64, how: Combine) {
    if let Some(mask) = editor.doc.layer(id).and_then(|l| l.mask.as_ref()) {
        let sel = Selection::from_mask(&mask.pixels);
        editor.set_selection("Load Selection", sel, how);
    }
}

/// Combine the selection into layer `id`'s mask (the other way round from
/// `load_mask`), as one undo step labelled `label`.
fn selection_to_mask(editor: &mut Editor, id: u64, label: &str, how: Combine) {
    editor.edit(label, |doc, _| {
        let Some(selection) = doc.selection.clone() else {
            return;
        };
        if let Some(mask) = doc.layer_mut(id).and_then(|l| l.mask.as_mut()) {
            mask.pixels = Selection::from_mask(&mask.pixels).combine(&selection, how).coverage;
        }
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
    let is_group = layer.is_group;
    let doc = &editor.doc;
    let index = doc.index_of(id);
    let several = editor.several_selected();
    let can_merge = several || is_group || index.is_some_and(|i| ops::can_merge_down(doc, i));
    let can_delete = doc.removed_count(&editor.selected()) < doc.layers.len();
    let shortcut = |ui: &Ui, cmd: Command| cmd.shortcut().map(|s| ui.ctx().format_shortcut(&s));

    menu_item(ui, "Blending Options…", None, true, || {
        *command = Some(Command::BlendingOptions);
    });

    ui.separator();

    let (duplicate, delete) = if several {
        ("Duplicate Layers", "Delete Layers")
    } else if is_group {
        ("Duplicate Group", "Delete Group")
    } else {
        ("Duplicate Layer", "Delete Layer")
    };
    menu_item(ui, duplicate, shortcut(ui, Command::DuplicateLayer), true, || {
        *command = Some(Command::DuplicateLayer);
    });

    menu_item(ui, delete, None, can_delete, || {
        *command = Some(Command::DeleteLayer);
    });

    menu_item(ui, "Rename", None, true, || {
        if let Some(l) = editor.doc.layer(id) {
            *renaming = Some(Rename::new(id, l.name.clone()));
        }
    });

    ui.separator();

    menu_item(ui, "Group Layers", shortcut(ui, Command::GroupLayers), true, || {
        *command = Some(Command::GroupLayers);
    });
    if is_group {
        menu_item(ui, "Ungroup Layers", shortcut(ui, Command::UngroupLayers), true, || {
            *command = Some(Command::UngroupLayers);
        });
    }
    let (clip, can_clip) = if layer.clipped {
        ("Release Clipping Mask", true)
    } else {
        ("Create Clipping Mask", index.is_some_and(|i| doc.can_clip(i)))
    };
    menu_item(ui, clip, shortcut(ui, Command::ClippingMask), can_clip, || {
        *command = Some(Command::ClippingMask);
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

    let merge = if several {
        "Merge Layers"
    } else if is_group {
        "Merge Group"
    } else {
        "Merge Down"
    };
    menu_item(ui, merge, shortcut(ui, Command::MergeDown), can_merge, || {
        *command = Some(Command::MergeDown);
    });
}

/// Select just this layer, as clicking its row does: paint on its pixels,
/// or on its mask for an adjustment layer or a group (which have no pixels).
fn select(editor: &mut Editor, id: u64) {
    select_with(editor, id, Vec::new());
}

/// Make layer `id` the active one, with `others` selected along with it.
fn select_with(editor: &mut Editor, id: u64, others: Vec<u64>) {
    editor.select_layers(id, others);
    let layer = editor.doc.layer(id);
    let no_pixels = layer.is_some_and(|l| !l.has_pixels() && l.mask.is_some());
    editor.target = if no_pixels {
        Target::Mask
    } else {
        Target::Pixels
    };
}

/// The groups a layer is in, innermost first.
fn ancestors(doc: &Document, id: u64) -> impl Iterator<Item = u64> + '_ {
    let parent = |id: u64| doc.layer(id).and_then(|l| l.parent);
    std::iter::successors(parent(id), move |&g| parent(g))
}

/// Where a dragged layer would land, and how to show it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drop {
    place: Place,
    marker: Marker,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Marker {
    /// A line between rows, across `left..right`, indented `depth` levels.
    Line {
        y: f32,
        depth: usize,
        left: f32,
        right: f32,
    },
    /// A group row outlined: it lands at the top of that group.
    Row(egui::Rect),
}

/// Where dropping a layer at `pointer` puts it, given the rows shown (top
/// first). Over the top half of a row it goes above that row, over the
/// bottom half below it, or at the top of the group if the row is an open
/// group. Over the middle of a group's row, it goes into the group.
fn drop_target(
    doc: &Document,
    rows: &[(u64, egui::Rect)],
    expanded: &HashSet<u64>,
    pointer: egui::Pos2,
) -> Option<Drop> {
    let (&(first, first_rect), &(last, last_rect)) = (rows.first()?, rows.last()?);
    let line = |y: f32, depth: usize| Marker::Line {
        y,
        depth,
        left: first_rect.left(),
        right: first_rect.right(),
    };
    if pointer.y < first_rect.top() {
        return Some(Drop {
            place: Place::Above(first),
            marker: line(first_rect.top(), 0),
        });
    }
    let Some(&(id, rect)) = rows.iter().find(|(_, r)| pointer.y < r.bottom()) else {
        // Below everything: the bottom of the stack.
        let bottom = ancestors(doc, last).last().unwrap_or(last);
        return Some(Drop {
            place: Place::Below(bottom),
            marker: line(last_rect.bottom(), 0),
        });
    };
    let is_group = doc.layer(id)?.is_group;
    let t = (pointer.y - rect.top()) / rect.height();
    Some(if is_group && (0.25..0.75).contains(&t) {
        Drop {
            place: Place::IntoTop(id),
            marker: Marker::Row(rect),
        }
    } else if t < 0.5 {
        Drop {
            place: Place::Above(id),
            marker: line(rect.top(), doc.depth(id)),
        }
    } else if is_group && expanded.contains(&id) {
        Drop {
            place: Place::IntoTop(id),
            marker: line(rect.bottom(), doc.depth(id) + 1),
        }
    } else {
        Drop {
            place: Place::Below(id),
            marker: line(rect.bottom(), doc.depth(id)),
        }
    })
}

/// Whether `place` is somewhere layers `ids` can go: not inside one of
/// them, nor next to the only one.
fn can_drop(doc: &Document, ids: &[u64], place: Place) -> bool {
    let (Place::Above(t) | Place::Below(t) | Place::IntoTop(t) | Place::IntoBottom(t)) = place;
    // Next to one of several, they gather around it.
    let beside = matches!(place, Place::Above(_) | Place::Below(_)) && ids.len() > 1;
    ids.iter().all(|&id| (t != id || beside) && !doc.is_inside(t, id))
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
        modifiers: egui::Modifiers,
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
                modifiers: Default::default(),
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
                events: [vec![Event::ModifiersChanged(self.modifiers)], events].concat(),
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
                    modifiers: self.modifiers,
                },
            ])
        }

        /// Click once, long enough after any earlier click not to double it.
        fn click(&mut self, pos: egui::Pos2) -> Option<Command> {
            self.time += 1.0;
            self.button(pos, true).or(self.button(pos, false))
        }

        /// Whether a layer's row is drawn now. Two frames, since egui also
        /// remembers widgets from the frame before last.
        fn shown(&mut self, layer: u64) -> bool {
            self.frame(vec![]);
            self.frame(vec![]);
            self.ctx.read_response(row_id(layer)).is_some()
        }

        fn name_point(&self, layer: u64) -> egui::Pos2 {
            self.ctx.read_response(name_id(layer)).unwrap().rect.center()
        }

        fn mask_point(&self, layer: u64) -> egui::Pos2 {
            self.ctx.read_response(thumb_id(layer, true)).unwrap().rect.center()
        }

        fn thumb_point(&self, layer: u64) -> egui::Pos2 {
            self.ctx.read_response(thumb_id(layer, false)).unwrap().rect.center()
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
    fn alt_clicking_the_mask_button_hides_all() {
        let mut h = Harness::new();
        // The buttons are the row of clickable widgets below the layers:
        // group, new, duplicate, mask, delete.
        let bottom = h.ctx.read_response(row_id(h.background())).unwrap().rect.bottom();
        let mut buttons: Vec<egui::Rect> = h.ctx.viewport(|v| {
            v.prev_pass
                .widgets
                .layers()
                .flat_map(|(_, w)| w)
                .filter(|w| w.sense.senses_click() && w.rect.top() > bottom)
                .map(|w| w.rect)
                .collect()
        });
        buttons.sort_by(|a, b| a.left().total_cmp(&b.left()));
        let mask = buttons[3].center();

        assert_eq!(h.click(mask), Some(Command::AddMask));
        h.modifiers = egui::Modifiers::ALT;
        assert_eq!(h.click(mask), Some(Command::AddMaskHideAll));
    }

    #[test]
    fn mask_menu_combines_the_mask_with_the_selection() {
        let mut h = Harness::new();
        let (w, hh) = (60, 40);
        // The Curves mask reveals the left half.
        let left = Selection::rectangle(w, hh, (0.0, 0.0), (30.0, 40.0));
        h.editor.doc.layer_mut(CURVES).unwrap().mask.as_mut().unwrap().pixels = left.coverage.clone();
        let top = Selection::rectangle(w, hh, (0.0, 0.0), (60.0, 20.0));
        let selected = |h: &Harness, x, y| h.editor.doc.selection.as_ref().map(|s| s.coverage.get(x, y));

        h.editor.doc.selection = Some(top.clone());
        load_mask(&mut h.editor, CURVES, Combine::Add);
        assert_eq!(selected(&h, 10, 30), Some(u16::MAX));
        assert_eq!(selected(&h, 50, 30), Some(0));

        h.editor.doc.selection = Some(top.clone());
        load_mask(&mut h.editor, CURVES, Combine::Subtract);
        assert_eq!(selected(&h, 10, 10), Some(0));
        assert_eq!(selected(&h, 50, 10), Some(u16::MAX));

        h.editor.doc.selection = Some(top);
        load_mask(&mut h.editor, CURVES, Combine::Intersect);
        assert_eq!(selected(&h, 10, 10), Some(u16::MAX));
        assert_eq!(selected(&h, 50, 10), Some(0));
        assert_eq!(selected(&h, 10, 30), Some(0));
    }

    #[test]
    fn mask_menu_combines_the_selection_into_the_mask() {
        let mut h = Harness::new();
        let (w, hh) = (60, 40);
        let left = Selection::rectangle(w, hh, (0.0, 0.0), (30.0, 40.0));
        h.editor.doc.selection = Some(Selection::rectangle(w, hh, (0.0, 0.0), (60.0, 20.0)));
        let masked = |h: &Harness, x, y| h.editor.doc.layer(CURVES).unwrap().mask.as_ref().unwrap().pixels.get(x, y);
        let cases = [
            (Combine::Replace, [u16::MAX, u16::MAX, 0, 0]),
            (Combine::Add, [u16::MAX, u16::MAX, u16::MAX, 0]),
            (Combine::Subtract, [0, 0, u16::MAX, 0]),
            (Combine::Intersect, [u16::MAX, 0, 0, 0]),
        ];
        for (how, [top_left, top_right, bottom_left, bottom_right]) in cases {
            // The Curves mask reveals the left half; the top half is selected.
            h.editor.doc.layer_mut(CURVES).unwrap().mask.as_mut().unwrap().pixels = left.coverage.clone();
            selection_to_mask(&mut h.editor, CURVES, "Mask", how);
            assert_eq!(
                [masked(&h, 10, 10), masked(&h, 50, 10), masked(&h, 10, 30), masked(&h, 50, 30)],
                [top_left, top_right, bottom_left, bottom_right],
                "{how:?}"
            );
            assert_eq!(h.editor.undo_label(), Some("Mask"));
            assert!(h.editor.doc.selection.is_some(), "the selection stays");
        }
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
    fn drop_target_goes_around_and_into_groups() {
        let mut h = Harness::new();
        let background = h.background();
        // Top first: Curves, then Multiply in a group, then the background.
        let group = h.editor.doc.group_layer(1);
        let rows: Vec<(u64, egui::Rect)> = [CURVES, group, MULTIPLY, background]
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let top = i as f32 * 20.0;
                (id, egui::Rect::from_min_max(pos2(0.0, top), pos2(200.0, top + 20.0)))
            })
            .collect();
        let doc = &h.editor.doc;
        let open: HashSet<u64> = [group].into();
        let at = |y: f32, open: &HashSet<u64>| {
            let drop = drop_target(doc, &rows, open, pos2(50.0, y)).unwrap();
            let depth = match drop.marker {
                Marker::Line { depth, .. } => Some(depth),
                Marker::Row(_) => None,
            };
            (drop.place, depth)
        };
        assert_eq!(at(-5.0, &open), (Place::Above(CURVES), Some(0)));
        assert_eq!(at(15.0, &open), (Place::Below(CURVES), Some(0)));
        // The group's row: above it, into it, or (open) into its top.
        assert_eq!(at(22.0, &open), (Place::Above(group), Some(0)));
        assert_eq!(at(30.0, &open), (Place::IntoTop(group), None));
        assert_eq!(at(38.0, &open), (Place::IntoTop(group), Some(1)));
        assert_eq!(at(38.0, &HashSet::new()), (Place::Below(group), Some(0)));
        // Below the last layer in the group stays in it; above the next
        // row is outside it.
        assert_eq!(at(55.0, &open), (Place::Below(MULTIPLY), Some(1)));
        assert_eq!(at(62.0, &open), (Place::Above(background), Some(0)));
        assert_eq!(at(200.0, &open), (Place::Below(background), Some(0)));

        // A group can't be dropped into itself.
        assert!(!can_drop(doc, &[group], Place::Above(MULTIPLY)));
        assert!(!can_drop(doc, &[group], Place::IntoTop(group)));
        assert!(can_drop(doc, &[MULTIPLY], Place::Above(CURVES)));
        assert!(!can_drop(doc, &[CURVES, group], Place::Above(MULTIPLY)));
    }

    #[test]
    fn groups_open_and_close_and_rows_drag_in_and_out() {
        let mut h = Harness::new();
        h.editor.edit("Group Layers", |doc, active| {
            *active = doc.group_layer(1);
        });
        let group = h.editor.active;
        // Groups start closed.
        assert!(!h.shown(MULTIPLY));
        let caret = h.ctx.read_response(caret_id(group)).unwrap().rect.center();
        h.click(caret);
        assert!(h.shown(MULTIPLY));
        let child = h.ctx.read_response(row_id(MULTIPLY)).unwrap().rect;
        let parent = h.ctx.read_response(row_id(group)).unwrap().rect;
        assert!(child.top() > parent.top());
        let name = |id| h.ctx.read_response(name_id(id)).unwrap().rect.left();
        assert!(name(MULTIPLY) > name(group) + INDENT - 1.0, "indented");
        assert_eq!(h.editor.active, group, "the caret doesn't select");

        // Selecting what's inside and closing the group selects the group.
        h.click(h.row_point(MULTIPLY));
        assert_eq!(h.editor.active, MULTIPLY);
        h.click(caret);
        assert_eq!(h.editor.active, group);
        assert!(!h.shown(MULTIPLY));

        // Drag the Curves layer onto the middle of the group's row: it
        // goes in at the top, and the group opens.
        let drag = |h: &mut Harness, from: egui::Pos2, to: egui::Pos2| {
            h.button(from, true);
            for i in 1..=5 {
                let pos = from + (to - from) * (i as f32 / 5.0);
                h.frame(vec![Event::PointerMoved(pos)]);
            }
            h.button(to, false);
            h.frame(vec![]);
        };
        let to = h.ctx.read_response(row_id(group)).unwrap().rect.center();
        let from = h.row_point(CURVES);
        drag(&mut h, from, to);
        let curves = h.editor.doc.layer(CURVES).unwrap();
        assert_eq!(curves.parent, Some(group));
        assert_eq!(h.editor.undo_label(), Some("Move Layer"));
        assert!(h.shown(CURVES));

        // And back out, above the group.
        let top = h.ctx.read_response(row_id(group)).unwrap().rect.top();
        let from = h.row_point(CURVES);
        drag(&mut h, from, pos2(to.x, top + 2.0));
        assert_eq!(h.editor.doc.layer(CURVES).unwrap().parent, None);
        assert_eq!(h.top_first()[0], CURVES);
    }

    #[test]
    fn clicking_a_group_selects_it_and_its_menu_renders() {
        let mut h = Harness::new();
        h.editor.edit("Group Layers", |doc, active| {
            *active = doc.group_layer(1);
        });
        let group = h.editor.active;
        h.frame(vec![]);
        h.click(h.row_point(group));
        // No mask and no pixels: nothing to paint, but it's selected.
        assert_eq!((h.editor.active, h.editor.target), (group, Target::Pixels));
        let mut command = None;
        let mut renaming = None;
        let mut out = h.ctx.run_ui(Default::default(), |ui| {
            layer_context_menu(ui, &mut h.editor, &mut command, &mut renaming, group);
        });
        out.textures_delta.clear();
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

    #[test]
    fn alt_clicking_between_rows_clips_the_upper_layer() {
        let mut h = Harness::new();
        let background = h.background();
        h.click(h.row_point(CURVES));
        h.frame(vec![]);
        let upper = h.ctx.read_response(row_id(MULTIPLY)).unwrap().rect;
        let lower = h.ctx.read_response(row_id(background)).unwrap().rect;
        let line = pos2(upper.center().x, (upper.bottom() + lower.top()) / 2.0);

        // Without Alt, it's an ordinary click on a row.
        h.click(line);
        assert!(!h.editor.doc.layer(MULTIPLY).unwrap().clipped);

        h.editor.active = CURVES;
        h.modifiers = egui::Modifiers::ALT;
        h.frame(vec![Event::PointerMoved(line)]);
        h.click(line);
        assert!(h.editor.doc.layer(MULTIPLY).unwrap().clipped);
        assert_eq!(h.editor.undo_label(), Some("Create Clipping Mask"));
        assert_eq!(h.editor.active, CURVES, "the click doesn't select");
        assert_eq!(h.editor.doc.clip_base(MULTIPLY), Some(background));

        // The clipped row gets its arrow, pushing its name right.
        h.modifiers = Default::default();
        let before = h.name_point(MULTIPLY).x;
        h.frame(vec![]);
        h.frame(vec![]);
        assert!(h.name_point(MULTIPLY).x > before, "indented");

        // Again releases it.
        h.modifiers = egui::Modifiers::ALT;
        h.click(line);
        assert!(!h.editor.doc.layer(MULTIPLY).unwrap().clipped);
        assert_eq!(h.editor.undo_label(), Some("Release Clipping Mask"));
    }

    #[test]
    fn ctrl_and_shift_click_select_several_layers() {
        let mut h = Harness::new();
        let background = h.background();
        h.click(h.row_point(background));

        // Ctrl+click adds a layer, which becomes the active one.
        h.modifiers = egui::Modifiers::COMMAND;
        h.click(h.row_point(CURVES));
        assert_eq!(h.editor.active, CURVES);
        assert_eq!(h.editor.target, Target::Mask, "painting applies to it");
        assert_eq!(h.editor.selected(), [background, CURVES]);
        // By its name too; and again takes it out.
        h.click(h.name_point(MULTIPLY));
        assert_eq!(h.editor.selected(), [background, MULTIPLY, CURVES]);
        h.click(h.row_point(MULTIPLY));
        assert_eq!(h.editor.selected(), [background, CURVES]);
        // Taking out the active one leaves the top one left active.
        h.click(h.row_point(CURVES));
        assert_eq!((h.editor.active, h.editor.selected()), (background, vec![background]));
        // The last one can't be taken out.
        h.click(h.row_point(background));
        assert_eq!(h.editor.selected(), [background]);

        // Shift+click selects the rows from the last one clicked.
        h.modifiers = egui::Modifiers::SHIFT;
        h.click(h.row_point(CURVES));
        assert_eq!(h.editor.selected(), [background, MULTIPLY, CURVES]);
        assert_eq!(h.editor.active, CURVES);
        h.click(h.row_point(MULTIPLY));
        assert_eq!(h.editor.selected(), [background, MULTIPLY]);

        // A plain click selects just the one again, even the active one.
        h.modifiers = Default::default();
        h.click(h.row_point(MULTIPLY));
        assert_eq!(h.editor.selected(), [MULTIPLY]);

        // Selecting a layer any other way (a new layer, say) selects just it.
        h.modifiers = egui::Modifiers::COMMAND;
        h.click(h.row_point(CURVES));
        assert_eq!(h.editor.selected().len(), 2);
        h.editor.active = background;
        assert_eq!(h.editor.selected(), [background]);
    }

    #[test]
    fn ctrl_click_thumbnail_loads_selection_and_ctrl_click_name_toggles_multi_selection() {
        let mut h = Harness::new();
        let background = h.background();

        // Plain click on background row selects it.
        h.click(h.row_point(background));
        assert_eq!(h.editor.selected(), [background]);
        assert!(h.editor.doc.selection.is_none());

        // Ctrl+click on thumbnail loads its transparency into selection.
        h.modifiers = egui::Modifiers::COMMAND;
        h.click(h.thumb_point(background));
        assert!(h.editor.doc.selection.is_some());
        assert_eq!(
            h.editor.selected(),
            [background],
            "Ctrl+click on thumbnail must not toggle layer selection"
        );

        // Ctrl+click on name still toggles multi-selection!
        h.click(h.name_point(MULTIPLY));
        assert_eq!(
            h.editor.selected(),
            [background, MULTIPLY],
            "Ctrl+click on name must toggle multi-selection"
        );

        // Ctrl+click on mask thumbnail loads mask into selection.
        h.click(h.mask_point(CURVES));
        assert!(h.editor.doc.selection.is_some());
        assert_eq!(h.editor.selected(), [background, MULTIPLY]);
    }

    #[test]
    fn several_selected_rows_drag_together_and_share_a_menu() {
        let mut h = Harness::new();
        let background = h.background();
        h.click(h.row_point(background));
        h.modifiers = egui::Modifiers::COMMAND;
        h.click(h.row_point(CURVES));
        h.modifiers = Default::default();

        // Right-clicking one of them keeps both selected, and makes it
        // active.
        h.secondary_click(h.row_point(background));
        assert_eq!(h.editor.active, background);
        assert_eq!(h.editor.selected(), [background, CURVES]);
        h.frame(vec![Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }]);

        // Dragging the bottom one drags both, keeping their order, to the
        // bottom of the stack.
        let drag = |h: &mut Harness, to: egui::Pos2| {
            h.time += 1.0;
            let from = h.row_point(background);
            h.button(from, true);
            for i in 1..=5 {
                let pos = from + (to - from) * (i as f32 / 5.0);
                h.frame(vec![Event::PointerMoved(pos)]);
            }
            h.button(to, false);
            h.frame(vec![]);
        };
        let bottom = h.row_point(background) + vec2(0.0, 30.0);
        drag(&mut h, bottom);
        assert_eq!(h.top_first(), [MULTIPLY, CURVES, background]);
        assert_eq!(h.editor.undo_label(), Some("Move Layers"));
        assert_eq!(h.editor.selected(), [background, CURVES], "still selected");
        // And to the top.
        let top = h.panel.rows[0].1.left_top() + vec2(50.0, -5.0);
        drag(&mut h, top);
        assert_eq!(h.top_first(), [CURVES, background, MULTIPLY]);

        let mut command = None;
        let mut renaming = None;
        let mut out = h.ctx.run_ui(Default::default(), |ui| {
            layer_context_menu(ui, &mut h.editor, &mut command, &mut renaming, CURVES);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn layer_navigation_steps_through_rows_and_groups() {
        let mut h = Harness::new();
        let background = h.background();
        // Stack top first: CURVES, MULTIPLY, background.
        assert_eq!(h.editor.active, CURVES);
        assert_eq!(h.editor.target, Target::Pixels);

        // At top: step up does nothing (no wrap).
        h.panel.step_selection(&mut h.editor, true);
        assert_eq!(h.editor.active, CURVES);

        // Step down to MULTIPLY (pixel layer: targets pixels).
        h.panel.step_selection(&mut h.editor, false);
        assert_eq!(h.editor.active, MULTIPLY);
        assert_eq!(h.editor.target, Target::Pixels);
        assert_eq!(h.editor.selected(), [MULTIPLY]);

        // Add a mask to CURVES. When stepping back up to it, target becomes Mask.
        h.editor.doc.layer_mut(CURVES).unwrap().mask =
            Some(omapix_engine::layer::Mask::white(60, 40));
        h.panel.step_selection(&mut h.editor, true);
        assert_eq!(h.editor.active, CURVES);
        assert_eq!(h.editor.target, Target::Mask);

        // Step back down to MULTIPLY.
        h.panel.step_selection(&mut h.editor, false);
        assert_eq!(h.editor.active, MULTIPLY);
        assert_eq!(h.editor.target, Target::Pixels);

        // Step down to background.
        h.panel.step_selection(&mut h.editor, false);
        assert_eq!(h.editor.active, background);
        assert_eq!(h.editor.target, Target::Pixels);

        // At bottom: step down does nothing (no wrap).
        h.panel.step_selection(&mut h.editor, false);
        assert_eq!(h.editor.active, background);

        // Now group MULTIPLY into a new group.
        h.editor.active = MULTIPLY;
        let g = h.editor.doc.group_layer(1);
        // Stack top first: CURVES, g, [MULTIPLY], background.
        // Group starts closed.
        h.panel.set_group_expanded(g, false);
        h.editor.active = background;

        // From background, stepping up should hit group `g`, skipping closed contents (MULTIPLY).
        h.panel.step_selection(&mut h.editor, true);
        assert_eq!(h.editor.active, g);

        // From group `g`, stepping up should hit CURVES.
        h.panel.step_selection(&mut h.editor, true);
        assert_eq!(h.editor.active, CURVES);

        // From CURVES, stepping down hits group `g`.
        h.panel.step_selection(&mut h.editor, false);
        assert_eq!(h.editor.active, g);

        // Now open the group `g`.
        h.panel.set_group_expanded(g, true);

        // From CURVES down: CURVES -> g -> MULTIPLY -> background.
        h.editor.active = CURVES;
        h.panel.step_selection(&mut h.editor, false);
        assert_eq!(h.editor.active, g);
        h.panel.step_selection(&mut h.editor, false);
        assert_eq!(h.editor.active, MULTIPLY);
        h.panel.step_selection(&mut h.editor, false);
        assert_eq!(h.editor.active, background);

        // And back up: background -> MULTIPLY -> g -> CURVES.
        h.panel.step_selection(&mut h.editor, true);
        assert_eq!(h.editor.active, MULTIPLY);
        h.panel.step_selection(&mut h.editor, true);
        assert_eq!(h.editor.active, g);
        h.panel.step_selection(&mut h.editor, true);
        assert_eq!(h.editor.active, CURVES);
    }

    #[test]
    fn alt_clicking_group_triangle_toggles_nested_groups_and_preserves_clipping() {
        let mut h = Harness::new();
        let background = h.background();

        // Build nested groups: outer -> inner -> MULTIPLY.
        h.editor.active = MULTIPLY;
        let inner = h.editor.doc.group_layer(1);
        h.editor.active = inner;
        let outer = h.editor.doc.group_layer(2);
        h.editor.active = background;

        // Groups start closed.
        h.frame(vec![]);
        assert!(!h.shown(inner));
        assert!(!h.shown(MULTIPLY));

        // Alt+click on outer caret opens outer and inner.
        let outer_caret = h.ctx.read_response(caret_id(outer)).unwrap().rect.center();
        h.modifiers = egui::Modifiers::ALT;
        h.click(outer_caret);
        assert!(h.shown(outer));
        assert!(h.shown(inner));
        assert!(h.shown(MULTIPLY));
        assert!(h.panel.expanded.contains(&outer));
        assert!(h.panel.expanded.contains(&inner));
        // Alt+click on caret must not clip outer or any layer.
        assert!(!h.editor.doc.layer(outer).unwrap().clipped);
        assert_eq!(h.panel.clip_line, None);

        // Select the active layer inside the innermost group.
        h.modifiers = Default::default();
        h.click(h.row_point(MULTIPLY));
        assert_eq!(h.editor.active, MULTIPLY);

        // Alt+click on outer caret closes outer and all nested groups.
        // Closing a group around the active layer selects the group as in Photoshop,
        // preventing the active layer's ancestors from immediately reopening.
        h.modifiers = egui::Modifiers::ALT;
        h.click(outer_caret);
        assert_eq!(h.editor.active, outer);
        assert!(!h.shown(inner));
        assert!(!h.shown(MULTIPLY));
        assert!(!h.panel.expanded.contains(&outer));
        assert!(!h.panel.expanded.contains(&inner));

        // Plain click on outer opens only outer; inner stays closed.
        h.modifiers = Default::default();
        h.click(outer_caret);
        assert!(h.shown(outer));
        assert!(h.shown(inner));
        assert!(!h.shown(MULTIPLY));
        assert!(h.panel.expanded.contains(&outer));
        assert!(!h.panel.expanded.contains(&inner));

        // Plain click on inner opens inner.
        let inner_caret = h.ctx.read_response(caret_id(inner)).unwrap().rect.center();
        h.click(inner_caret);
        assert!(h.shown(MULTIPLY));
        assert!(h.panel.expanded.contains(&inner));

        // Plain click on outer closes outer while inner remembers its open state.
        h.click(outer_caret);
        assert!(!h.shown(inner));
        assert!(!h.panel.expanded.contains(&outer));
        assert!(h.panel.expanded.contains(&inner));

        // Alt+click on outer while closed opens both outer and inner.
        h.modifiers = egui::Modifiers::ALT;
        h.click(outer_caret);
        assert!(h.shown(inner));
        assert!(h.shown(MULTIPLY));
        assert!(h.panel.expanded.contains(&outer));
        assert!(h.panel.expanded.contains(&inner));

        // Alt+click between rows still clips (between top CURVES and outer group).
        h.frame(vec![]);
        let upper = h.ctx.read_response(row_id(CURVES)).unwrap().rect;
        let lower = h.ctx.read_response(row_id(outer)).unwrap().rect;
        let line = pos2(upper.center().x, (upper.bottom() + lower.top()) / 2.0);
        h.modifiers = egui::Modifiers::ALT;
        h.frame(vec![Event::PointerMoved(line)]);
        h.click(line);
        assert!(h.editor.doc.layer(CURVES).unwrap().clipped);
        assert_eq!(h.editor.undo_label(), Some("Create Clipping Mask"));
    }
}
