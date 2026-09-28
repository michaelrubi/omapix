//! Select › Select and Mask… (Ctrl+Alt+R): Photoshop's workspace cut down
//! to its sliders. The refined selection is worked out on a thread of its
//! own as they move, and shown over the image in Quick Mask's red; OK makes
//! it the selection, a layer mask, or a new layer with it as its mask.

use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Color32, ComboBox, RichText, Slider, vec2};
use omapix_engine::refine::{EdgeOptions, colours, refine_edge};
use omapix_engine::selection::{Combine, Selection};
use omapix_engine::Mask;

use crate::editor::Editor;
use crate::theme::Theme;

/// Where the refined selection goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Output {
    #[default]
    Selection,
    LayerMask,
    NewLayerWithMask,
}

impl Output {
    const ALL: [Output; 3] = [Output::Selection, Output::LayerMask, Output::NewLayerWithMask];

    fn label(self) -> &'static str {
        match self {
            Output::Selection => "Selection",
            Output::LayerMask => "Layer Mask",
            Output::NewLayerWithMask => "New Layer with Layer Mask",
        }
    }
}

pub struct SelectAndMask {
    pub options: EdgeOptions,
    pub output: Output,
    /// The options last sent to the worker.
    sent: Option<EdgeOptions>,
    tx: Sender<EdgeOptions>,
    rx: Receiver<(EdgeOptions, Selection)>,
    /// The newest refined selection, and the options it's for.
    latest: Option<(EdgeOptions, Selection)>,
}

impl SelectAndMask {
    /// Starts refining `editor`'s selection, if it has one.
    pub fn open(ctx: &egui::Context, editor: &Editor, options: EdgeOptions, output: Output) -> Option<Self> {
        let selection = editor.doc.selection.clone()?;
        let doc = editor.doc.clone();
        let (tx, requests) = channel::<EdgeOptions>();
        let (answers, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let guide = colours(&doc.composite());
            while let Ok(mut options) = requests.recv() {
                // Only the newest settings matter.
                while let Ok(newer) = requests.try_recv() {
                    options = newer;
                }
                if answers.send((options, refine_edge(&selection, &guide, &options))).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Some(Self {
            options,
            output,
            sent: None,
            tx,
            rx,
            latest: None,
        })
    }

    /// The refined selection for the current settings, once it's ready.
    pub fn ready(&self) -> Option<&Selection> {
        self.latest.as_ref().filter(|(o, _)| *o == self.options).map(|(_, s)| s)
    }

    /// Show the dialog, previewing on the canvas. Returns `Some(true)` once
    /// OK'd, `Some(false)` if cancelled.
    pub fn show(&mut self, ctx: &egui::Context, editor: &mut Editor, defaults: &EdgeOptions, theme: &Theme) -> Option<bool> {
        if self.sent != Some(self.options) && self.tx.send(self.options).is_ok() {
            self.sent = Some(self.options);
        }
        if let Some(answer) = self.rx.try_iter().last() {
            editor.preview_selection(answer.1.clone());
            self.latest = Some(answer);
        }
        let has_pixels = editor.doc.layer(editor.active).is_some_and(|l| l.has_pixels());
        let mut result = None;
        let area = egui::Modal::default_area(egui::Id::new("select-and-mask"))
            .anchor(egui::Align2::RIGHT_TOP, vec2(-320.0, 90.0));
        egui::Modal::new(egui::Id::new("select-and-mask"))
            .area(area)
            .backdrop_color(Color32::TRANSPARENT)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading("Select and Mask");
                ui.add_space(8.0);
                let o = &mut self.options;
                let slider = |ui: &mut egui::Ui, label: &str, value: &mut f32, range, suffix: &str, tip: &str| {
                    ui.label(label).on_hover_text(tip);
                    ui.add(Slider::new(value, range).suffix(suffix).fixed_decimals(1));
                    ui.end_row();
                };
                egui::Grid::new("select-and-mask-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    slider(ui, "Radius", &mut o.radius, 0.0..=250.0, " px", "Edge Detection: how far either side of the edge to look for the photo's own, for hair and fur");
                    slider(ui, "Smooth", &mut o.smooth, 0.0..=100.0, "", "Rounds off a jagged outline");
                    slider(ui, "Feather", &mut o.feather, 0.0..=250.0, " px", "Softens the edge");
                    slider(ui, "Contrast", &mut o.contrast, 0.0..=100.0, " %", "Hardens soft edges");
                    slider(ui, "Shift Edge", &mut o.shift_edge, -100.0..=100.0, " %", "Moves soft edges out, or in with negative values");
                    ui.label("Output To");
                    ComboBox::from_id_salt("select-and-mask-output")
                        .selected_text(self.output.label())
                        .show_ui(ui, |ui| {
                            for output in Output::ALL {
                                let possible = has_pixels || output != Output::NewLayerWithMask;
                                ui.add_enabled_ui(possible, |ui| {
                                    ui.selectable_value(&mut self.output, output, output.label());
                                });
                            }
                        });
                    ui.end_row();
                });
                ui.add_space(8.0);
                let ready = self.latest.as_ref().is_some_and(|(o, _)| *o == self.options);
                let status = if ready { "Red shows what isn't selected" } else { "Refining…" };
                ui.label(RichText::new(status).color(theme.dark_foreground));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.add_enabled(ready, egui::Button::new("OK")).clicked() || (enter && ready) {
                        result = Some(true);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        result = Some(false);
                    }
                    if ui.button("Defaults").clicked() {
                        self.options = *defaults;
                    }
                });
            });
        result
    }

    /// Put the refined selection where it's meant to go, as one undo step.
    pub fn apply(&self, editor: &mut Editor) {
        editor.end_selection_preview();
        let Some(refined) = self.ready().cloned() else {
            return;
        };
        let label = "Select and Mask";
        match self.output {
            Output::Selection => editor.set_selection(label, refined, Combine::Replace),
            Output::LayerMask | Output::NewLayerWithMask => {
                let (output, active) = (self.output, editor.active);
                editor.edit(label, |doc, current| {
                    let target = if output == Output::NewLayerWithMask {
                        let Some(index) = doc.index_of(active) else {
                            return;
                        };
                        // Photoshop hides the original.
                        let copy = doc.duplicate_layer(index);
                        if let Some(original) = doc.layer_mut(active) {
                            original.visible = false;
                        }
                        *current = copy;
                        copy
                    } else {
                        active
                    };
                    if let Some(layer) = doc.layer_mut(target) {
                        layer.mask = Some(Mask {
                            pixels: refined.coverage,
                            enabled: true,
                        });
                    }
                    doc.selection = None;
                });
            }
        }
    }

    /// Stop previewing without changing anything.
    pub fn cancel(&self, editor: &mut Editor) {
        editor.end_selection_preview();
    }
}
