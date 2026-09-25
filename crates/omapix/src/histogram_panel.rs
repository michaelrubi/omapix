//! The Histogram panel: composite image histogram and tonal statistics.

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::{
    Color32, ComboBox, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2,
};
use omapix_engine::Histogram;

use crate::editor::Editor;
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HistogramChannel {
    #[default]
    Colors,
    Luminosity,
    Red,
    Green,
    Blue,
}

impl HistogramChannel {
    pub const ALL: [HistogramChannel; 5] = [
        HistogramChannel::Colors,
        HistogramChannel::Luminosity,
        HistogramChannel::Red,
        HistogramChannel::Green,
        HistogramChannel::Blue,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HistogramChannel::Colors => "Colors",
            HistogramChannel::Luminosity => "Luminosity",
            HistogramChannel::Red => "Red",
            HistogramChannel::Green => "Green",
            HistogramChannel::Blue => "Blue",
        }
    }
}

pub struct HistogramPanel {
    pub channel: HistogramChannel,
    histogram: Option<Histogram>,
    cached_revision: u64,
    last_update: Instant,
    rx: Option<Receiver<(u64, Histogram)>>,
}

impl Default for HistogramPanel {
    fn default() -> Self {
        Self {
            channel: HistogramChannel::default(),
            histogram: None,
            cached_revision: 0,
            last_update: Instant::now() - Duration::from_secs(1),
            rx: None,
        }
    }
}

impl HistogramPanel {
    pub fn show(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme) {
        // Poll background histogram computation.
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok((rev, hist)) => {
                    self.histogram = Some(hist);
                    self.cached_revision = rev;
                    self.rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                }
            }
        }

        // Compute off the UI thread when document revision changes (throttled).
        let rev = editor.revision();
        let needs_update = self.histogram.is_none() || self.cached_revision != rev;
        let throttled = self.last_update.elapsed() < Duration::from_millis(250);
        if needs_update
            && self.rx.is_none()
            && (!throttled || self.histogram.is_none())
            && let Some(render) = editor.canvas.render()
        {
            let render = render.clone();
            let (tx, rx) = channel();
            self.rx = Some(rx);
            self.last_update = Instant::now();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let hist = render
                    .with_image(Histogram::from_raster)
                    .unwrap_or_default();
                let _ = tx.send((rev, hist));
                ctx.request_repaint();
            });
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Channel:").small().color(theme.dark_foreground));
            ComboBox::from_id_salt("histogram-channel")
                .selected_text(self.channel.label())
                .show_ui(ui, |ui| {
                    for ch in HistogramChannel::ALL {
                        ui.selectable_value(&mut self.channel, ch, ch.label());
                    }
                });
        });
        ui.add_space(4.0);

        let side = ui.available_width().min(280.0);
        let height = 100.0;
        let (rect, _) = ui.allocate_exact_size(vec2(side, height), Sense::hover());
        let painter = ui.painter_at(rect.expand(2.0));
        painter.rect_filled(rect, 0.0, theme.darker_background);

        // Grid lines behind graph
        let grid = Stroke::new(1.0, theme.selection);
        for i in 1..4 {
            let t = i as f32 / 4.0;
            let x = rect.left() + t * rect.width();
            painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], grid);
            let y = rect.top() + t * rect.height();
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], grid);
        }

        if let Some(hist) = &self.histogram {
            match self.channel {
                HistogramChannel::Colors => {
                    // One scale for all three, so they compare.
                    let scale_max = scale(&[&hist.red, &hist.green, &hist.blue]);
                    draw_histogram_scaled(&painter, rect, &hist.red, 1, Some(scale_max), theme);
                    draw_histogram_scaled(&painter, rect, &hist.green, 2, Some(scale_max), theme);
                    draw_histogram_scaled(&painter, rect, &hist.blue, 3, Some(scale_max), theme);
                }
                HistogramChannel::Luminosity => {
                    draw_histogram(&painter, rect, &hist.luminance, 0, theme);
                }
                HistogramChannel::Red => {
                    draw_histogram(&painter, rect, &hist.red, 1, theme);
                }
                HistogramChannel::Green => {
                    draw_histogram(&painter, rect, &hist.green, 2, theme);
                }
                HistogramChannel::Blue => {
                    draw_histogram(&painter, rect, &hist.blue, 3, theme);
                }
            }

            let stats = match self.channel {
                HistogramChannel::Colors | HistogramChannel::Luminosity => hist.stats(0),
                HistogramChannel::Red => hist.stats(1),
                HistogramChannel::Green => hist.stats(2),
                HistogramChannel::Blue => hist.stats(3),
            };

            ui.add_space(4.0);
            egui::Grid::new("histogram-stats")
                .num_columns(4)
                .spacing(vec2(8.0, 2.0))
                .show(ui, |ui| {
                    ui.label(RichText::new("Mean:").small().color(theme.dark_foreground));
                    ui.label(RichText::new(format!("{:.2}", stats.mean)).small());
                    ui.label(RichText::new("Median:").small().color(theme.dark_foreground));
                    ui.label(RichText::new(format!("{}", stats.median)).small());
                    ui.end_row();

                    ui.label(RichText::new("Std Dev:").small().color(theme.dark_foreground));
                    ui.label(RichText::new(format!("{:.2}", stats.std_dev)).small());
                    ui.label(RichText::new("Pixels:").small().color(theme.dark_foreground));
                    ui.label(RichText::new(format!("{}", stats.pixels)).small());
                    ui.end_row();
                });
        }
    }
}

/// The count drawn full height for `channels`: the tallest bar, but no more
/// than three times the tallest between pure black and white, so a clipped
/// end doesn't flatten the rest.
fn scale(channels: &[&[u32; 256]]) -> f32 {
    let tallest = |range: std::ops::Range<usize>| channels.iter().flat_map(|c| &c[range.clone()]).copied().max().unwrap_or(0);
    let (max, interior) = (tallest(0..256), tallest(1..255));
    let scale = if interior > 0 { max.min(interior * 3).max(interior) } else { max };
    scale as f32
}

/// Draw a histogram onto `painter` over `rect`.
/// `channel`: 0 = Luminance/foreground, 1 = Red, 2 = Green, 3 = Blue.
pub fn draw_histogram(
    painter: &egui::Painter,
    rect: Rect,
    bins: &[u32; 256],
    channel: usize,
    theme: &Theme,
) {
    draw_histogram_scaled(painter, rect, bins, channel, None, theme);
}

pub fn draw_histogram_scaled(
    painter: &egui::Painter,
    rect: Rect,
    bins: &[u32; 256],
    channel: usize,
    scale_max: Option<f32>,
    theme: &Theme,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let scale_max = scale_max.unwrap_or_else(|| scale(&[bins]));
    if scale_max <= 0.0 {
        return;
    }

    let (fill, stroke) = match channel {
        1 => (
            Color32::from_rgba_unmultiplied(230, 80, 80, 50),
            Color32::from_rgba_unmultiplied(230, 80, 80, 140),
        ),
        2 => (
            Color32::from_rgba_unmultiplied(80, 200, 100, 50),
            Color32::from_rgba_unmultiplied(80, 200, 100, 140),
        ),
        3 => (
            Color32::from_rgba_unmultiplied(90, 140, 240, 50),
            Color32::from_rgba_unmultiplied(90, 140, 240, 140),
        ),
        _ => {
            let fg = theme.foreground;
            (
                Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 40),
                Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 110),
            )
        }
    };

    let mut mesh = egui::Mesh::default();
    let bottom = rect.bottom();
    let height = rect.height();
    let width = rect.width();
    let left = rect.left();

    let mut outline = Vec::with_capacity(258);
    outline.push(pos2(left, bottom));

    for (i, &count) in bins.iter().enumerate() {
        let x0 = left + (i as f32 / 256.0) * width;
        let x1 = left + ((i + 1) as f32 / 256.0) * width;
        let h = (count as f32 / scale_max).clamp(0.0, 1.0) * height;
        let y = bottom - h;
        let xm = (x0 + x1) * 0.5;
        outline.push(pos2(xm, y));

        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(pos2(x0, bottom), fill);
        mesh.colored_vertex(pos2(x0, y), fill);
        mesh.colored_vertex(pos2(x1, y), fill);
        mesh.colored_vertex(pos2(x1, bottom), fill);
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    outline.push(pos2(rect.right(), bottom));

    painter.add(Shape::mesh(mesh));
    painter.add(Shape::line(outline, Stroke::new(1.0, stroke)));
}
