//! The Channels panel, as in Photoshop: the image's red, green and blue
//! channels and the saved alpha channels. Click one to see it on its own in
//! grey (RGB goes back to the image), and Ctrl+click to load it as a
//! selection, with Shift to add, Alt to subtract and both to intersect.
//! Ctrl+click on RGB loads the luminosity.

use egui::{Align2, Button, FontId, RichText, ScrollArea, Sense, Ui};
use omapix_engine::selection::{Channel, Combine, Selection};

use crate::commands::Command;
use crate::editor::{Editor, View};
use crate::theme::Theme;

const LOAD: &str = "\u{f10c}";
const SAVE: &str = "\u{f0c7}";
const TRASH: &str = "\u{f1f8}";

pub fn channel_row_id(index: usize) -> egui::Id {
    egui::Id::new("channel_row").with(index)
}

/// A row: the image, one of its channels, or an alpha channel.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Composite,
    Colour(Channel),
    Alpha(u64),
}

impl Row {
    fn view(self) -> View {
        match self {
            Row::Composite => View::Image,
            Row::Colour(c) => View::Channel(c),
            Row::Alpha(id) => View::Alpha(id),
        }
    }

    /// Load it as a selection.
    fn load(self, editor: &mut Editor, how: Combine) {
        match self {
            Row::Composite => editor.load_channel(Channel::Luminosity, how),
            Row::Colour(c) => editor.load_channel(c, how),
            Row::Alpha(id) => {
                if let Some(c) = editor.doc.channel(id) {
                    let selection = Selection::from_mask(&c.pixels);
                    editor.set_selection("Load Selection", selection, how);
                }
            }
        }
    }
}

/// The row being shown: any view but a single channel shows the image.
fn shown(view: View) -> Row {
    match view {
        View::Channel(c) => Row::Colour(c),
        View::Alpha(id) => Row::Alpha(id),
        _ => Row::Composite,
    }
}

pub fn show(ui: &mut Ui, editor: &mut Editor, theme: &Theme) -> Option<Command> {
    let mut rows = vec![
        (Row::Composite, "RGB".to_owned(), Some(Command::ViewComposite)),
        (Row::Colour(Channel::Red), "Red".to_owned(), Some(Command::ViewRed)),
        (Row::Colour(Channel::Green), "Green".to_owned(), Some(Command::ViewGreen)),
        (Row::Colour(Channel::Blue), "Blue".to_owned(), Some(Command::ViewBlue)),
    ];
    rows.extend(editor.doc.channels.iter().map(|c| (Row::Alpha(c.id), c.name.clone(), None)));
    let showing = shown(editor.view());
    let font = FontId::proportional(13.0);

    ui.add_space(4.0);
    let footer = 28.0;
    ScrollArea::vertical()
        .max_height((ui.available_height() - footer).max(60.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, (row, name, command)) in rows.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
                let response = ui.interact(rect, channel_row_id(i), Sense::click());
                if *row == showing {
                    ui.painter().rect_filled(rect, 2.0, theme.selection);
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 2.0, theme.lighter_background);
                }
                let left = rect.left_center() + egui::vec2(8.0, 0.0);
                ui.painter().text(left, Align2::LEFT_CENTER, name, font.clone(), theme.foreground);
                if let Some(shortcut) = command.and_then(Command::shortcut) {
                    let right = rect.right_center() - egui::vec2(8.0, 0.0);
                    let text = crate::hotkeys::format_shortcut(&shortcut);
                    ui.painter().text(right, Align2::RIGHT_CENTER, text, font.clone(), theme.dark_foreground);
                }
                if response.clicked() {
                    let (ctrl, shift, alt) = ui.input(|i| (i.modifiers.command, i.modifiers.shift, i.modifiers.alt));
                    if ctrl {
                        row.load(editor, Combine::from_modifiers(shift, alt));
                    } else {
                        editor.set_view(row.view());
                    }
                }
            }
        });

    ui.separator();
    let mut command = None;
    ui.horizontal(|ui| {
        let button = |ui: &mut Ui, icon: &str, enabled: bool, tip: &str| {
            ui.add_enabled(enabled, Button::new(RichText::new(icon)).frame(false))
                .on_hover_text(tip)
                .clicked()
        };
        if button(ui, LOAD, true, "Load channel as selection (Ctrl+click)") {
            showing.load(editor, Combine::Replace);
        }
        if button(ui, SAVE, editor.doc.selection.is_some(), "Save selection as channel") {
            command = Some(Command::SaveSelection);
        }
        if button(ui, TRASH, matches!(showing, Row::Alpha(_)), "Delete channel") {
            command = Some(Command::DeleteChannel);
        }
    });
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton};
    use omapix_engine::{ColorProfile, Document, Raster};

    struct Harness {
        ctx: egui::Context,
        editor: Editor,
        theme: Theme,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let (w, h) = (60, 40);
            let image = Raster::new(w, h, vec![[10000, 20000, 40000, 65535]; (w * h) as usize]);
            let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
            let mut h = Self {
                ctx: egui::Context::default(),
                editor: Editor::new(doc).unwrap(),
                theme: Theme::default(),
                time: 0.0,
            };
            h.frame(vec![]);
            h
        }

        fn frame(&mut self, events: Vec<Event>) -> Option<Command> {
            self.time += 0.5;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 600.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (editor, theme) = (&mut self.editor, &self.theme);
            let mut command = None;
            let mut output = self.ctx.run_ui(input, |ui| command = show(ui, editor, theme));
            output.textures_delta.clear();
            command
        }

        fn click_row(&mut self, index: usize, modifiers: Modifiers) {
            let pos = self.ctx.read_response(channel_row_id(index)).unwrap().rect.center();
            for pressed in [true, false] {
                self.frame(vec![
                    Event::ModifiersChanged(modifiers),
                    Event::PointerMoved(pos),
                    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers },
                ]);
            }
        }
    }

    #[test]
    fn clicking_shows_a_channel_and_ctrl_click_loads_it() {
        let mut h = Harness::new();
        h.click_row(1, Modifiers::NONE);
        assert_eq!(h.editor.view(), View::Channel(Channel::Red));
        h.click_row(0, Modifiers::NONE);
        assert_eq!(h.editor.view(), View::Image);

        // Ctrl+click Blue selects as much as each pixel is blue.
        h.click_row(3, Modifiers::COMMAND);
        let selection = h.editor.doc.selection.as_ref().unwrap();
        assert_eq!(selection.coverage.get(5, 5), 40000);
        assert_eq!(h.editor.view(), View::Image, "loading doesn't change the view");

        // A saved selection gets a row, shown alone when clicked, and
        // Ctrl+Alt+click subtracts it (in proportion, as in Photoshop).
        h.editor.edit("Save Selection", |doc, _| {
            doc.save_selection();
        });
        h.frame(vec![]);
        let id = h.editor.doc.channels[0].id;
        h.click_row(4, Modifiers::NONE);
        assert_eq!(h.editor.view(), View::Alpha(id));
        h.click_row(4, Modifiers::COMMAND | Modifiers::ALT);
        let left = h.editor.doc.selection.as_ref().unwrap().coverage.get(5, 5);
        assert!(left.abs_diff((40000u32 * (65535 - 40000) / 65535) as u16) <= 2, "{left}");
    }
}
