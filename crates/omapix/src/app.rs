use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use egui::{Align, Key, Layout, Modifiers, RichText, Ui};

use crate::canvas::{Canvas, Loaded};
use crate::theme::{self, Theme};

/// A View menu entry: label, shortcut text, and what it does to the canvas.
type ViewAction = (&'static str, &'static str, fn(&mut Canvas));

const IMAGE_EXTENSIONS: [&str; 5] = ["tif", "tiff", "png", "jpg", "jpeg"];

pub struct App {
    theme: Theme,
    theme_rx: Receiver<Theme>,
    canvas: Option<Canvas>,
    /// A file being opened in the background.
    opening: Option<(PathBuf, Receiver<Result<Loaded, String>>)>,
    /// A file dialog open in the background.
    picking: Option<Receiver<Option<PathBuf>>>,
    error: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let ctx = &cc.egui_ctx;
        // Ctrl+= / Ctrl+- zoom the image, not the interface.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        theme::install_font(ctx);
        let theme = Theme::load();
        ctx.set_visuals(theme.visuals());

        let mut app = Self {
            theme,
            theme_rx: theme::watch(ctx.clone()),
            canvas: None,
            opening: None,
            picking: None,
            error: None,
        };
        if let Some(path) = path {
            app.open(path, ctx);
        }
        app
    }

    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let target = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Loaded::open(&target));
            ctx.request_repaint();
        });
        self.opening = Some((path, rx));
        self.error = None;
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        if self.picking.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let file = rfd::FileDialog::new()
                .set_title("Open image")
                .add_filter("Images", &IMAGE_EXTENSIONS)
                .pick_file();
            let _ = tx.send(file);
            ctx.request_repaint();
        });
        self.picking = Some(rx);
    }

    /// Pick up results from background work: theme changes, file dialog, loading.
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(theme) = self.theme_rx.try_iter().last() {
            ctx.set_visuals(theme.visuals());
            self.theme = theme;
        }
        if let Some(rx) = &self.picking
            && let Ok(result) = rx.try_recv()
        {
            self.picking = None;
            if let Some(path) = result {
                self.open(path, ctx);
            }
        }
        if let Some((path, rx)) = &self.opening
            && let Ok(result) = rx.try_recv()
        {
            match result {
                Ok(loaded) => {
                    let title = format!("{} — Omapix", loaded.doc.file_name());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
                    self.canvas = Some(Canvas::new(loaded));
                }
                Err(err) => self.error = Some(format!("Couldn't open {}: {err}", path.display())),
            }
            self.opening = None;
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            self.open(path, ctx);
        }
    }

    fn global_shortcuts(&mut self, ctx: &egui::Context) {
        let (open, quit) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::COMMAND, Key::Q),
            )
        });
        if open {
            self.pick_file(ctx);
        }
        if quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn menu_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(egui::Button::new("Open…").shortcut_text("Ctrl+O"))
                    .clicked()
                {
                    self.pick_file(&ctx);
                }
                ui.separator();
                if ui
                    .add(egui::Button::new("Quit").shortcut_text("Ctrl+Q"))
                    .clicked()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                let enabled = self.canvas.is_some();
                let items: [ViewAction; 4] = [
                    ("Zoom In", "Ctrl+=", |c| c.step_zoom(true)),
                    ("Zoom Out", "Ctrl+-", |c| c.step_zoom(false)),
                    ("Fit on Screen", "Ctrl+0", Canvas::fit),
                    ("100%", "Ctrl+1", Canvas::actual_pixels),
                ];
                for (label, keys, action) in items {
                    if ui
                        .add_enabled(enabled, egui::Button::new(label).shortcut_text(keys))
                        .clicked()
                        && let Some(canvas) = &mut self.canvas
                    {
                        action(canvas);
                    }
                }
            });
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let dim = self.theme.dark_foreground;
        ui.horizontal(|ui| {
            if let Some(canvas) = &self.canvas {
                let doc = canvas.doc();
                ui.label(canvas.zoom_label());
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "{} × {} px",
                        doc.raster.width(),
                        doc.raster.height()
                    ))
                    .color(dim),
                );
                ui.label(RichText::new(format!("{}-bit", doc.source_bits)).color(dim));
                ui.label(RichText::new(doc.profile.description()).color(dim));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(err) = &self.error {
                    ui.label(RichText::new(err).color(self.theme.red));
                } else if let Some((path, _)) = &self.opening {
                    ui.spinner();
                    ui.label(format!(
                        "Opening {}…",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                } else if let Some(canvas) = &self.canvas
                    && let Some((x, y)) = canvas.hovered_pixel
                {
                    let [r, g, b, _] = canvas.doc().raster.get(x, y);
                    let v = |c: u16| c >> 8;
                    ui.label(
                        RichText::new(format!("R {:>3}  G {:>3}  B {:>3}", v(r), v(g), v(b)))
                            .color(dim),
                    );
                    ui.label(format!("X {x}  Y {y}"));
                }
            });
        });
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.4);
                ui.label(RichText::new("Omapix").size(28.0).color(self.theme.accent));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Open an image with Ctrl+O, or drop one here")
                        .color(self.theme.dark_foreground),
                );
                ui.add_space(12.0);
                if ui.button("Open…").clicked() {
                    self.pick_file(&ctx);
                }
            });
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.global_shortcuts(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let bar = egui::Frame::new()
            .fill(self.theme.dark_background)
            .inner_margin(egui::Margin::symmetric(8, 4));
        egui::Panel::top("menu")
            .frame(bar)
            .show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status")
            .frame(bar)
            .show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Some(canvas) = &mut self.canvas {
                canvas.show(ui, self.theme.pasteboard());
            } else {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, self.theme.pasteboard());
                self.empty_state(ui);
            }
        });
    }
}
