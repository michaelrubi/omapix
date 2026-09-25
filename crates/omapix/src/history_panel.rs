//! The History panel: a chronological list of undo steps you can click back
//! to, matching Photoshop's History panel.

use egui::{Align, Button, Layout, RichText, ScrollArea, Sense, Ui};

use crate::commands::Command;
use crate::editor::Editor;
use crate::theme::Theme;

// Nerd Font icons
const IMAGE_ICON: &str = "\u{f03e}";
const EDIT_ICON: &str = "\u{f044}";
const UNDO_ICON: &str = "\u{f0e2}";
const REDO_ICON: &str = "\u{f01e}";

#[derive(Default)]
pub struct HistoryPanel;

pub fn history_row_id(index: usize) -> egui::Id {
    egui::Id::new("history_row").with(index)
}

impl HistoryPanel {
    pub fn show(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme) -> Option<Command> {
        let mut command = None;
        let labels = editor.history_labels();
        let active = editor.history_active_index();
        let total = labels.len();

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("History").strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(editor.undo_label().is_some() || editor.redo_label().is_some(), Button::new("Clear"))
                    .on_hover_text("Clear history")
                    .clicked()
                {
                    editor.clear_history();
                }
            });
        });
        ui.add_space(4.0);

        let bottom_height = 28.0;
        let available_height = (ui.available_height() - bottom_height).max(60.0);

        ScrollArea::vertical()
            .max_height(available_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (i, label) in labels.iter().enumerate() {
                    let is_active = i == active;
                    let is_future = i > active;

                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 24.0),
                        Sense::hover(),
                    );
                    let response = ui.interact(rect, history_row_id(i), Sense::click());

                    if is_active {
                        ui.painter().rect_filled(rect, 2.0, theme.selection);
                    } else if response.hovered() {
                        ui.painter().rect_filled(rect, 2.0, theme.lighter_background);
                    }

                    let icon = if i == 0 { IMAGE_ICON } else { EDIT_ICON };
                    let icon_colour = if is_active {
                        theme.accent
                    } else if response.hovered() {
                        theme.foreground
                    } else {
                        theme.dark_foreground
                    };

                    let text_colour = if is_future && !is_active && !response.hovered() {
                        theme.dark_foreground
                    } else {
                        theme.foreground
                    };

                    if is_active {
                        let bar = egui::Rect::from_min_size(rect.left_top(), egui::vec2(2.0, rect.height()));
                        ui.painter().rect_filled(bar, 1.0, theme.accent);
                    }

                    ui.painter().text(
                        egui::pos2(rect.left() + 8.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        icon,
                        egui::FontId::proportional(13.0),
                        icon_colour,
                    );
                    ui.painter().text(
                        egui::pos2(rect.left() + 24.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(13.0),
                        text_colour,
                    );

                    if response.clicked() {
                        editor.jump_to_history(i);
                    }
                }
            });

        ui.separator();
        ui.horizontal(|ui| {
            let can_undo = editor.undo_label().is_some();
            let can_redo = editor.redo_label().is_some();

            if ui
                .add_enabled(can_undo, Button::new(RichText::new(UNDO_ICON)).frame(false))
                .on_hover_text("Step Backward (Undo)")
                .clicked()
            {
                command = Some(Command::Undo);
            }
            if ui
                .add_enabled(can_redo, Button::new(RichText::new(REDO_ICON)).frame(false))
                .on_hover_text("Step Forward (Redo)")
                .clicked()
            {
                command = Some(Command::Redo);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let status = format!("Step {} of {}", active + 1, total);
                ui.label(RichText::new(status).size(11.0).color(theme.dark_foreground));
            });
        });

        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, pos2, vec2};
    use omapix_engine::color::ColorProfile;
    use omapix_engine::{Document, Raster};

    struct Harness {
        ctx: egui::Context,
        panel: HistoryPanel,
        editor: Editor,
        theme: Theme,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let (w, h) = (60, 40);
            let image = Raster::new(w, h, vec![[30000, 30000, 30000, 65535]; (w * h) as usize]);
            let doc = Document::from_image("test.tif".into(), &image, ColorProfile::srgb(), 16);
            let mut h = Self {
                ctx: egui::Context::default(),
                panel: HistoryPanel,
                editor: Editor::new(doc).unwrap(),
                theme: Theme::default(),
                time: 0.0,
            };
            h.frame(vec![]);
            h
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
            output.textures_delta.clear();
            command
        }

        fn click(&mut self, pos: egui::Pos2) -> Option<Command> {
            self.time += 1.0;
            self.frame(vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ]);
            self.frame(vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ])
        }

        fn row_point(&self, index: usize) -> egui::Pos2 {
            self.ctx.read_response(history_row_id(index)).unwrap().rect.center()
        }
    }

    #[test]
    fn history_panel_shows_steps_and_clicking_jumps_state() {
        let mut h = Harness::new();
        h.editor.edit("Invert", |doc, _| doc.layers[0].opacity = 0.5);
        h.editor.edit("Curves", |doc, _| doc.layers[0].opacity = 0.2);

        h.frame(vec![]);
        assert_eq!(h.editor.history_active_index(), 2);
        assert_eq!(h.editor.doc.layers[0].opacity, 0.2);

        // Click row 1 ("Invert")
        let pt1 = h.row_point(1);
        h.click(pt1);
        assert_eq!(h.editor.history_active_index(), 1);
        assert_eq!(h.editor.doc.layers[0].opacity, 0.5);

        // Click row 0 ("test.tif")
        let pt0 = h.row_point(0);
        h.click(pt0);
        assert_eq!(h.editor.history_active_index(), 0);
        assert_eq!(h.editor.doc.layers[0].opacity, 1.0);

        // Click row 2 ("Curves") to jump forward
        let pt2 = h.row_point(2);
        h.click(pt2);
        assert_eq!(h.editor.history_active_index(), 2);
        assert_eq!(h.editor.doc.layers[0].opacity, 0.2);

        // Hover row 1 without clicking: response is hovered
        let pt1 = h.row_point(1);
        h.frame(vec![Event::PointerMoved(pt1)]);
        let resp = h.ctx.read_response(history_row_id(1)).unwrap();
        assert!(resp.hovered());
    }
}
