//! Select › Select and Mask… (Ctrl+Alt+R): Photoshop's workspace cut down
//! to its sliders. The refined selection is worked out on a thread of its
//! own as they move, and shown over the image in Quick Mask's red, on black
//! or white, or in black and white (F cycles them, and holding X shows the
//! image as it is); OK makes it the selection, a layer mask, or a new layer
//! with it as its mask. Painting on the image is the Refine Edge brush:
//! the edge is looked for where it goes as well as within the Radius, and
//! Alt paints that away.

use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Color32, ComboBox, RichText, Slider, Stroke, vec2};
use omapix_engine::Mask;
use omapix_engine::refine::{EdgeBrush, EdgeOptions, colours, decontaminate_colours, refine_edge_again};
use omapix_engine::selection::{Combine, Selection};

use crate::editor::{Backdrop, Editor, View};
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

/// Photoshop's View Modes that Omapix has, in the order F goes through
/// them, with the key for each: Quick Mask's red (`None`), then the
/// backdrops.
const VIEWS: [(Option<Backdrop>, &str, egui::Key); 4] = [
    (None, "Overlay", egui::Key::V),
    (Some(Backdrop::Black), "On Black", egui::Key::A),
    (Some(Backdrop::White), "On White", egui::Key::T),
    (Some(Backdrop::Grey), "Black & White", egui::Key::K),
];

/// What the canvas shows for each view.
fn view(backdrop: Option<Backdrop>) -> View {
    backdrop.map_or(View::QuickMask, View::Refined)
}

pub struct SelectAndMask {
    pub options: EdgeOptions,
    pub output: Output,
    /// What the refined selection is shown on: `None` is Quick Mask's red.
    pub backdrop: Option<Backdrop>,
    /// Where the Refine Edge brush has painted, and how many times.
    brush: EdgeBrush,
    painted: u64,
    /// The brush's size in image pixels.
    pub brush_size: f32,
    /// Where the brush last painted, while it's held down.
    last: Option<egui::Pos2>,
    /// The options and painting last sent to the worker.
    sent: Option<(EdgeOptions, u64)>,
    tx: Sender<(EdgeOptions, u64, EdgeBrush)>,
    rx: Receiver<(EdgeOptions, u64, Selection)>,
    /// The newest refined selection, and the options and painting it's for.
    latest: Option<(EdgeOptions, u64, Selection)>,
}

impl SelectAndMask {
    /// Starts refining `editor`'s selection, if it has one.
    pub fn open(ctx: &egui::Context, editor: &Editor, options: EdgeOptions, output: Output, backdrop: Option<Backdrop>) -> Option<Self> {
        let selection = editor.doc.selection.clone()?;
        let doc = editor.doc.clone();
        let (tx, requests) = channel::<(EdgeOptions, u64, EdgeBrush)>();
        let (answers, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let guide = colours(&doc.composite());
            // The edge found last time, so painting only finds it again
            // near where the brush went.
            let mut last = None;
            while let Ok(mut request) = requests.recv() {
                // Only the newest settings matter.
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let (options, painted, brush) = request;
                let refined = refine_edge_again(&mut last, &selection, &guide, &options, Some(&brush));
                if answers.send((options, painted, refined)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Some(Self {
            options,
            output,
            backdrop,
            brush: EdgeBrush::new(editor.doc.width, editor.doc.height),
            painted: 0,
            brush_size: 100.0,
            last: None,
            sent: None,
            tx,
            rx,
            latest: None,
        })
    }

    /// The refined selection for the current settings, once it's ready.
    pub fn ready(&self) -> Option<&Selection> {
        self.latest
            .as_ref()
            .filter(|(o, p, _)| (*o, *p) == (self.options, self.painted))
            .map(|(_, _, s)| s)
    }

    fn receive(&mut self, editor: &mut Editor, answer: (EdgeOptions, u64, Selection)) {
        editor.preview_selection(answer.2.clone());
        self.latest = Some(answer);
    }

    /// Waits for the worker to refine the selection for the settings
    /// `show` last sent, and previews it.
    #[cfg(test)]
    pub fn wait(&mut self, editor: &mut Editor) {
        while self.ready().is_none() {
            // Only a safety net: the worker competes with the whole test suite.
            let answer = self.rx.recv_timeout(std::time::Duration::from_secs(60)).expect("a preview");
            self.receive(editor, answer);
        }
    }

    /// Show the dialog, previewing on the canvas. Returns `Some(true)` once
    /// OK'd, `Some(false)` if cancelled.
    pub fn show(&mut self, ctx: &egui::Context, editor: &mut Editor, defaults: &EdgeOptions, theme: &Theme) -> Option<bool> {
        let now = (self.options, self.painted);
        if self.sent != Some(now) && self.tx.send((self.options, self.painted, self.brush.clone())).is_ok() {
            self.sent = Some(now);
        }
        if let Some(answer) = self.rx.try_iter().last() {
            self.receive(editor, answer);
        }
        // The view's keys, unless a slider's number is being typed.
        let mut original = false;
        if !ctx.egui_wants_keyboard_input() {
            ctx.input(|i| {
                let at = VIEWS.iter().position(|v| v.0 == self.backdrop).unwrap_or(0);
                if i.key_pressed(egui::Key::F) {
                    let step = if i.modifiers.shift { VIEWS.len() - 1 } else { 1 };
                    self.backdrop = VIEWS[(at + step) % VIEWS.len()].0;
                }
                if let Some(v) = VIEWS.iter().find(|v| i.key_pressed(v.2)) {
                    self.backdrop = v.0;
                }
                original = i.key_down(egui::Key::X);
                for (key, bigger) in [(egui::Key::OpenBracket, false), (egui::Key::CloseBracket, true)] {
                    if i.modifiers.is_none() && i.key_pressed(key) {
                        self.brush_size = crate::tools::step_size(self.brush_size, bigger).clamp(1.0, 5000.0);
                    }
                }
            });
        }
        if editor.previewing_selection() {
            editor.set_view(if original { View::Image } else { view(self.backdrop) });
        }
        let has_pixels = editor.doc.layer(editor.active).is_some_and(|l| l.has_pixels());
        let mut result = None;
        let area = egui::Modal::default_area(egui::Id::new("select-and-mask")).anchor(egui::Align2::RIGHT_TOP, vec2(-320.0, 90.0));
        let modal = egui::Modal::new(egui::Id::new("select-and-mask"))
            .area(area)
            .backdrop_color(Color32::TRANSPARENT)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading("Select and Mask");
                ui.add_space(8.0);
                let o = &mut self.options;
                let slider = |ui: &mut egui::Ui, label: &str, value: &mut f32, range, suffix: &str, tip: &str| {
                    ui.label(label).on_hover_text(tip);
                    // Sizes in px are curved like Photoshop's, so a few px can be
                    // dragged to (PhotoCraft's a0128945).
                    ui.add(Slider::new(value, range).logarithmic(suffix == " px").suffix(suffix).fixed_decimals(1));
                    ui.end_row();
                };
                egui::Grid::new("select-and-mask-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("View")
                        .on_hover_text("F goes through the views, and holding X shows the image as it is");
                    let label = |b| VIEWS.iter().find(|v| v.0 == b).map_or("", |v| v.1);
                    ComboBox::from_id_salt("select-and-mask-view")
                        .selected_text(label(self.backdrop))
                        .show_ui(ui, |ui| {
                            for (backdrop, name, key) in VIEWS {
                                ui.selectable_value(&mut self.backdrop, backdrop, format!("{name} ({key:?})"));
                            }
                        });
                    ui.end_row();
                    slider(
                        ui,
                        "Radius",
                        &mut o.radius,
                        0.0..=250.0,
                        " px",
                        "Edge Detection: how far either side of the edge to look for the photo's own, for hair and fur",
                    );
                    ui.label("");
                    ui.checkbox(&mut o.smart_radius, "Smart Radius")
                        .on_hover_text("Automatically adapt the radius to the edge's sharpness: narrower on hard edges, wider on soft ones");
                    ui.end_row();
                    ui.label("Refine Edge Brush")
                        .on_hover_text("Paint over hair on the image to look for its edge there too, and Alt to paint that away. [ and ] change the size");
                    ui.add(
                        Slider::new(&mut self.brush_size, 1.0..=5000.0)
                            .logarithmic(true)
                            .suffix(" px")
                            .fixed_decimals(0),
                    );
                    ui.end_row();
                    slider(ui, "Smooth", &mut o.smooth, 0.0..=100.0, "", "Rounds off a jagged outline");
                    slider(ui, "Feather", &mut o.feather, 0.0..=250.0, " px", "Softens the edge");
                    slider(ui, "Contrast", &mut o.contrast, 0.0..=100.0, " %", "Hardens soft edges");
                    slider(
                        ui,
                        "Shift Edge",
                        &mut o.shift_edge,
                        -100.0..=100.0,
                        " %",
                        "Moves soft edges out, or in with negative values",
                    );
                    ui.label("");
                    if ui
                        .checkbox(&mut o.decontaminate, "Decontaminate Colors")
                        .on_hover_text("Replaces color fringing in edge transitions with nearby foreground colors")
                        .changed()
                        && o.decontaminate
                        && self.output == Output::Selection
                    {
                        self.output = Output::NewLayerWithMask;
                    }
                    ui.end_row();
                    if o.decontaminate {
                        slider(
                            ui,
                            "Amount",
                            &mut o.decontaminate_amount,
                            0.0..=100.0,
                            " %",
                            "How strongly to replace fringe color with foreground color",
                        );
                    }
                    ui.label("Output To");
                    ComboBox::from_id_salt("select-and-mask-output")
                        .selected_text(self.output.label())
                        .show_ui(ui, |ui| {
                            for output in Output::ALL {
                                let possible = (has_pixels || output != Output::NewLayerWithMask) && (!o.decontaminate || output != Output::Selection);
                                ui.add_enabled_ui(possible, |ui| {
                                    ui.selectable_value(&mut self.output, output, output.label());
                                });
                            }
                        });
                    ui.end_row();
                });
                ui.add_space(8.0);
                let ready = self.latest.as_ref().is_some_and(|(o, p, _)| (*o, *p) == (self.options, self.painted));
                let status = match (ready, self.backdrop) {
                    (false, _) => "Refining…",
                    (true, None) => "Red shows what isn't selected",
                    (true, Some(Backdrop::Black)) => "Black shows what isn't selected",
                    (true, Some(Backdrop::White)) => "White shows what isn't selected",
                    (true, Some(Backdrop::Grey)) => "White is selected, black isn't",
                };
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
        self.paint(ctx, editor, &modal.backdrop_response);
        result
    }

    /// The Refine Edge brush, on the image: what's outside the dialog, as
    /// its backdrop takes the pointer from the canvas.
    fn paint(&mut self, ctx: &egui::Context, editor: &Editor, backdrop: &egui::Response) {
        let canvas = &editor.canvas;
        let (pointer, origin, down, alt) = ctx.input(|i| (i.pointer.interact_pos(), i.pointer.press_origin(), i.pointer.primary_down(), i.modifiers.alt));
        let on_canvas = |p: Option<egui::Pos2>| p.is_some_and(|p| canvas.image_at(p).is_some());
        if backdrop.is_pointer_button_down_on()
            && down
            && on_canvas(origin)
            && let Some(p) = pointer.map(|p| canvas.to_image(p))
        {
            if self.last != Some(p) {
                self.brush.paint(self.last.unwrap_or(p).into(), p.into(), self.brush_size, !alt);
                self.painted += 1;
                self.last = Some(p);
                // Sent to the worker next frame.
                ctx.request_repaint();
            }
        } else {
            self.last = None;
        }
        // Its outline, dark and light so it shows on anything.
        if let Some(at) = backdrop.hover_pos().filter(|&p| on_canvas(Some(p))) {
            let radius = (self.brush_size / 2.0 / canvas.image_per_point()).max(2.0);
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("refine-edge-brush")));
            painter.circle_stroke(at, radius, Stroke::new(1.5, Color32::from_black_alpha(160)));
            painter.circle_stroke(at, radius, Stroke::new(0.75, Color32::from_white_alpha(200)));
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
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
                let (output, active, options) = (self.output, editor.active, self.options);
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
                        if options.decontaminate {
                            layer.pixels = decontaminate_colours(&layer.pixels, &refined.coverage, options.radius, options.decontaminate_amount);
                        }
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
