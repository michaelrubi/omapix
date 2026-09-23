//! The image canvas: view transform, progressive tile streaming and
//! Photoshop-style navigation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{
    Color32, ColorImage, CursorIcon, Key, Modifiers, PointerButton, Pos2, Rect, Sense,
    TextureFilter, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2,
};
use omapix_engine::pyramid::Pyramid;
use omapix_engine::{DisplayTransform, Document, tiles};

/// Photoshop's zoom presets, as fractions.
const ZOOM_STEPS: [f32; 21] = [
    1.0 / 32.0,
    1.0 / 24.0,
    1.0 / 16.0,
    1.0 / 12.0,
    1.0 / 8.0,
    1.0 / 6.0,
    1.0 / 4.0,
    1.0 / 3.0,
    1.0 / 2.0,
    2.0 / 3.0,
    1.0,
    2.0,
    3.0,
    4.0,
    5.0,
    6.0,
    7.0,
    8.0,
    12.0,
    16.0,
    32.0,
];
const MIN_ZOOM: f32 = 0.01;
const MAX_ZOOM: f32 = 32.0;

/// Tiles converted in parallel at most. Keeps the queue short so fast
/// zooming doesn't leave a backlog of tiles for levels no longer on screen.
const MAX_IN_FLIGHT: usize = 16;

/// Everything the tile workers need, shared read-only across threads.
pub struct Loaded {
    pub doc: Document,
    pub pyramid: Pyramid,
    pub transform: DisplayTransform,
}

impl Loaded {
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let started = std::time::Instant::now();
        let doc = omapix_engine::io::load(path).map_err(|e| e.to_string())?;
        let loaded = std::time::Instant::now();
        let pyramid = Pyramid::build(&doc.raster);
        let transform = DisplayTransform::to_srgb(&doc.profile).map_err(|e| e.to_string())?;
        log::info!(
            "opened {} in {:?} (decode {:?}, pyramid {:?})",
            path.display(),
            started.elapsed(),
            loaded - started,
            loaded.elapsed()
        );
        Ok(Self {
            doc,
            pyramid,
            transform,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TileKey {
    level: usize,
    col: u32,
    row: u32,
}

struct TileImage {
    key: TileKey,
    image: ColorImage,
}

/// Where the image is on screen.
#[derive(Clone, Copy)]
struct View {
    /// Physical screen pixels per image pixel, so 1.0 is Photoshop's 100 %.
    zoom: f32,
    /// Screen position of the image's top-left corner, relative to the
    /// canvas's top-left, in points.
    origin: Vec2,
    /// Keep fitting the image to the window until the user zooms or pans.
    fit: bool,
}

pub struct Canvas {
    shared: Arc<Loaded>,
    view: View,
    textures: HashMap<TileKey, (TextureHandle, tiles::TileBounds)>,
    in_flight: HashSet<TileKey>,
    tx: Sender<TileImage>,
    rx: Receiver<TileImage>,
    /// Image pixel under the pointer, if any.
    pub hovered_pixel: Option<(u32, u32)>,
    /// Canvas area and scale from the last frame, for menu commands.
    rect: Rect,
    ppp: f32,
}

impl Canvas {
    pub fn new(loaded: Loaded) -> Self {
        let (tx, rx) = channel();
        Self {
            shared: Arc::new(loaded),
            view: View {
                zoom: 1.0,
                origin: Vec2::ZERO,
                fit: true,
            },
            textures: HashMap::new(),
            in_flight: HashSet::new(),
            tx,
            rx,
            hovered_pixel: None,
            rect: Rect::NOTHING,
            ppp: 1.0,
        }
    }

    pub fn doc(&self) -> &Document {
        &self.shared.doc
    }

    /// Zoom as Photoshop shows it: "33.33%", "100%".
    pub fn zoom_label(&self) -> String {
        let text = format!("{:.2}", self.view.zoom * 100.0);
        format!("{}%", text.trim_end_matches('0').trim_end_matches('.'))
    }

    fn image_size(&self) -> Vec2 {
        let r = &self.shared.doc.raster;
        vec2(r.width() as f32, r.height() as f32)
    }

    fn fit_zoom(&self, canvas: Rect, ppp: f32) -> f32 {
        let size = self.image_size();
        let avail = canvas.size() * ppp;
        (avail.x / size.x).min(avail.y / size.y).min(1.0)
    }

    /// Change zoom, keeping the image point under `anchor` (a screen position) still.
    fn zoom_to(&mut self, zoom: f32, anchor: Pos2, canvas: Rect) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let anchor = anchor - canvas.min;
        self.view.origin = anchor - (anchor - self.view.origin) * (zoom / self.view.zoom);
        self.view.zoom = zoom;
        self.view.fit = false;
    }

    fn center_at(&mut self, zoom: f32, canvas: Rect, ppp: f32) {
        self.view.zoom = zoom;
        self.view.origin = (canvas.size() - self.image_size() * zoom / ppp) * 0.5;
    }

    pub fn fit(&mut self) {
        self.view.fit = true;
    }

    pub fn actual_pixels(&mut self) {
        self.center_at(1.0, self.rect, self.ppp);
        self.view.fit = false;
    }

    pub fn step_zoom(&mut self, zoom_in: bool) {
        let canvas = self.rect;
        let z = self.view.zoom;
        let next = if zoom_in {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|&s| s > z * 1.001)
                .unwrap_or(MAX_ZOOM)
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|&s| s < z / 1.001)
                .unwrap_or(MIN_ZOOM)
        };
        self.zoom_to(next, canvas.center(), canvas);
    }

    /// Handle keyboard shortcuts that act on the view.
    fn shortcuts(&mut self, ui: &Ui) {
        let (fit, actual, zoom_in, zoom_out) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::Num0),
                i.consume_key(Modifiers::COMMAND, Key::Num1),
                i.consume_key(Modifiers::COMMAND, Key::Equals)
                    || i.consume_key(Modifiers::COMMAND, Key::Plus),
                i.consume_key(Modifiers::COMMAND, Key::Minus),
            )
        });
        if fit {
            self.fit();
        }
        if actual {
            self.actual_pixels();
        }
        if zoom_in {
            self.step_zoom(true);
        }
        if zoom_out {
            self.step_zoom(false);
        }
    }

    pub fn show(&mut self, ui: &mut Ui, pasteboard: Color32) {
        let canvas = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(canvas, Sense::click_and_drag());
        let ppp = ui.pixels_per_point();
        self.rect = canvas;
        self.ppp = ppp;
        ui.painter().rect_filled(canvas, 0.0, pasteboard);

        self.shortcuts(ui);
        self.navigate(ui, &response, canvas);
        if self.view.fit {
            let z = self.fit_zoom(canvas, ppp);
            self.center_at(z, canvas, ppp);
        }

        self.receive_tiles(ui.ctx());
        self.draw(ui, canvas, ppp);

        self.hovered_pixel = response.hover_pos().and_then(|p| {
            let img = (p - canvas.min - self.snapped_origin(ppp)) / (self.view.zoom / ppp);
            let size = self.image_size();
            (img.x >= 0.0 && img.y >= 0.0 && img.x < size.x && img.y < size.y)
                .then_some((img.x as u32, img.y as u32))
        });
    }

    fn navigate(&mut self, ui: &Ui, response: &egui::Response, canvas: Rect) {
        let space = ui.input(|i| i.key_down(Key::Space));
        let panning = response.dragged_by(PointerButton::Middle)
            || (space && response.dragged_by(PointerButton::Primary));
        if panning {
            self.view.origin += response.drag_delta();
            self.view.fit = false;
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if space && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }

        if !response.hovered() {
            return;
        }
        let Some(pointer) = response.hover_pos() else {
            return;
        };
        let (zoom_delta, scroll, alt) =
            ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.alt));
        if zoom_delta != 1.0 {
            // Ctrl+scroll and touchpad pinch.
            self.zoom_to(self.view.zoom * zoom_delta, pointer, canvas);
        } else if alt && scroll != Vec2::ZERO {
            // Photoshop's Alt+scroll zoom.
            let amount = if scroll.y != 0.0 { scroll.y } else { scroll.x };
            self.zoom_to(self.view.zoom * (amount * 0.005).exp(), pointer, canvas);
        } else if scroll != Vec2::ZERO {
            self.view.origin += scroll;
            self.view.fit = false;
        }
    }

    /// Origin rounded to whole physical pixels, so 100 % stays crisp.
    fn snapped_origin(&self, ppp: f32) -> Vec2 {
        (self.view.origin * ppp).round() / ppp
    }

    /// Pyramid level to draw at the current zoom: the smallest level that
    /// still has at least one pixel per screen pixel.
    fn target_level(&self) -> usize {
        if self.view.zoom >= 1.0 {
            return 0;
        }
        let level = (1.0 / self.view.zoom).log2().floor() as usize;
        level.min(self.shared.pyramid.len() - 1)
    }

    fn receive_tiles(&mut self, ctx: &egui::Context) {
        while let Ok(tile) = self.rx.try_recv() {
            self.in_flight.remove(&tile.key);
            // Level 0 is shown nearest-neighbour when magnified, so pixels stay crisp.
            let magnification = if tile.key.level == 0 {
                TextureFilter::Nearest
            } else {
                TextureFilter::Linear
            };
            let options = TextureOptions {
                magnification,
                minification: TextureFilter::Linear,
                ..Default::default()
            };
            let name = format!("tile-{}-{}-{}", tile.key.level, tile.key.col, tile.key.row);
            let base = &self.shared.doc.raster;
            let level = self.shared.pyramid.level(base, tile.key.level);
            let bounds = tiles::bounds(level.width(), level.height(), tile.key.col, tile.key.row);
            self.textures.insert(
                tile.key,
                (ctx.load_texture(name, tile.image, options), bounds),
            );
        }
    }

    fn draw(&mut self, ui: &Ui, canvas: Rect, ppp: f32) {
        let painter = ui.painter_at(canvas);
        let origin = canvas.min + self.snapped_origin(ppp);
        let scale = self.view.zoom / ppp;
        let size = self.image_size();
        let image_rect = Rect::from_min_size(origin, size * scale);
        // Anything the tiles haven't covered yet shows as dark grey rather
        // than the pasteboard, so the image's extent is visible immediately.
        painter.rect_filled(image_rect, 0.0, Color32::from_gray(24));

        let target = self.target_level();
        let base = &self.shared.doc.raster;
        let mut wanted = Vec::new();

        // Draw coarse to fine, so finer tiles cover coarser ones as they arrive.
        for level_index in (target..self.shared.pyramid.len()).rev() {
            let level = self.shared.pyramid.level(base, level_index);
            // Level pixels to image pixels, per axis (odd sizes round up when halving).
            let to_image = vec2(
                size.x / level.width() as f32,
                size.y / level.height() as f32,
            );
            let (cols, rows) = tiles::grid(level.width(), level.height());
            let visible = canvas.intersect(image_rect);
            if !visible.is_positive() {
                break;
            }
            // Visible range in level pixels.
            let lo = ((visible.min - origin) / scale) / to_image;
            let hi = ((visible.max - origin) / scale) / to_image;
            let tile = tiles::TILE_SIZE as f32;
            let c0 = (lo.x / tile).floor().max(0.0) as u32;
            let r0 = (lo.y / tile).floor().max(0.0) as u32;
            let c1 = ((hi.x / tile).ceil() as u32).min(cols);
            let r1 = ((hi.y / tile).ceil() as u32).min(rows);

            for row in r0..r1 {
                for col in c0..c1 {
                    let key = TileKey {
                        level: level_index,
                        col,
                        row,
                    };
                    match self.textures.get(&key) {
                        Some((texture, b)) => {
                            let min = origin
                                + vec2(b.x as f32 * to_image.x, b.y as f32 * to_image.y) * scale;
                            let max = origin
                                + vec2(
                                    (b.x + b.w) as f32 * to_image.x,
                                    (b.y + b.h) as f32 * to_image.y,
                                ) * scale;
                            let uv = Rect::from_min_max(
                                pos2(
                                    (b.x - b.tex_x) as f32 / b.tex_w as f32,
                                    (b.y - b.tex_y) as f32 / b.tex_h as f32,
                                ),
                                pos2(
                                    (b.x + b.w - b.tex_x) as f32 / b.tex_w as f32,
                                    (b.y + b.h - b.tex_y) as f32 / b.tex_h as f32,
                                ),
                            );
                            painter.image(
                                texture.id(),
                                Rect::from_min_max(min, max),
                                uv,
                                Color32::WHITE,
                            );
                        }
                        None if level_index == target
                            || level_index == self.shared.pyramid.len() - 1 =>
                        {
                            // Always want the target level, plus the tiny top
                            // level as an instant placeholder.
                            let center = vec2((col as f32 + 0.5) * tile, (row as f32 + 0.5) * tile);
                            let focus = (lo + hi) * 0.5;
                            wanted.push((level_index, (center - focus).length(), key));
                        }
                        None => {}
                    }
                }
            }
        }

        // Coarsest level first, then target tiles nearest the middle of the view.
        wanted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.total_cmp(&b.1)));
        for (_, _, key) in wanted {
            if self.in_flight.len() >= MAX_IN_FLIGHT {
                break;
            }
            if self.in_flight.insert(key) {
                self.request(key, ui.ctx().clone());
            }
        }
    }

    fn request(&self, key: TileKey, ctx: egui::Context) {
        let shared = Arc::clone(&self.shared);
        let tx = self.tx.clone();
        rayon::spawn(move || {
            let level = shared.pyramid.level(&shared.doc.raster, key.level);
            let b = tiles::bounds(level.width(), level.height(), key.col, key.row);
            let rgba = tiles::render(level, &shared.transform, b);
            let image =
                ColorImage::from_rgba_unmultiplied([b.tex_w as usize, b.tex_h as usize], &rgba);
            if tx.send(TileImage { key, image }).is_ok() {
                ctx.request_repaint();
            }
        });
    }
}
