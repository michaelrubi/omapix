//! The Layers panel, laid out like Photoshop's: blend mode and opacity for
//! the selected layer at the top, the stack (top layer first) in the
//! middle, and layer buttons at the bottom.

use egui::{Align, Button, ComboBox, Layout, RichText, ScrollArea, Sense, Slider, TextEdit, Ui};
use omapix_engine::BlendMode;

use crate::commands::Command;
use crate::editor::{Editor, Target};
use crate::theme::Theme;

// Nerd Font icons (Omarchy's fonts are all Nerd Fonts).
const EYE: &str = "\u{f06e}";
const EYE_OFF: &str = "\u{f070}";
const MASK: &str = "\u{f042}";
const PLUS: &str = "\u{f067}";
const COPY: &str = "\u{f0c5}";
const TRASH: &str = "\u{f1f8}";
const SLIDERS: &str = "\u{f1de}";

#[derive(Default)]
pub struct LayersPanel {
    /// Layer being renamed, and the name typed so far.
    renaming: Option<(u64, String)>,
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
                for id in ids {
                    self.row(ui, editor, theme, id);
                }
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
        command
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

    fn row(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme, id: u64) {
        let Some(layer) = editor.doc.layer(id) else {
            return;
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
        egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(4, 3))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let eye = if visible { EYE } else { EYE_OFF };
                    let eye_colour = if visible {
                        theme.foreground
                    } else {
                        theme.dark_foreground
                    };
                    if ui
                        .add(Button::new(RichText::new(eye).color(eye_colour)).frame(false))
                        .on_hover_text("Show/hide")
                        .clicked()
                    {
                        editor.edit(
                            if visible { "Hide Layer" } else { "Show Layer" },
                            |doc, _| {
                                if let Some(l) = doc.layer_mut(id) {
                                    l.visible = !l.visible;
                                }
                            },
                        );
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(enabled) = mask {
                            let targeted = selected && editor.target == Target::Mask;
                            let colour = match (enabled, targeted) {
                                (false, _) => theme.red,
                                (true, true) => theme.accent,
                                (true, false) => theme.foreground,
                            };
                            let chip = ui
                                .add(Button::new(RichText::new(MASK).color(colour)).frame(targeted))
                                .on_hover_text(
                                    "Layer mask — click to edit, Shift+click to disable",
                                );
                            if chip.clicked() {
                                if ui.input(|i| i.modifiers.shift) {
                                    editor.edit(
                                        if enabled {
                                            "Disable Layer Mask"
                                        } else {
                                            "Enable Layer Mask"
                                        },
                                        |doc, _| {
                                            if let Some(m) =
                                                doc.layer_mut(id).and_then(|l| l.mask.as_mut())
                                            {
                                                m.enabled = !m.enabled;
                                            }
                                        },
                                    );
                                } else {
                                    editor.active = id;
                                    editor.target = Target::Mask;
                                }
                            }
                        }
                        if blend != BlendMode::Normal {
                            ui.label(
                                RichText::new(blend.name())
                                    .small()
                                    .color(theme.dark_foreground),
                            );
                        }

                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if adjustment {
                                ui.label(RichText::new(SLIDERS).color(theme.accent))
                                    .on_hover_text("Adjustment layer");
                            }
                            self.name(ui, editor, id, &name, selected);
                        });
                    });
                });
            });
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
