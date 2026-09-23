//! The image canvas: view transform, progressive tile streaming and
//! Photoshop-style navigation.
//!
//! The canvas shows a [`Render`]: the flattened document plus its zoom
//! pyramid, produced in the background whenever the document changes.
//! Visible tiles are converted to display colour on worker threads and
//! uploaded as textures. When a new render arrives, the old textures stay on
//! screen until their replacements are ready, so edits never flicker.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{
    Color32, ColorImage, CursorIcon, Key, Modifiers, PointerButton, Pos2, Rect, Sense,
    TextureFilter, TextureHandle, TextureOptions, TextureWrapMode, Ui, Vec2, pos2, vec2,
};
use omapix_engine::pyramid::Pyramid;
use omapix_engine::{DisplayTransform, Pixel, Raster, tiles};

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

/// Size of one checkerboard square behind transparent areas, in points.
const CHECKER: f32 = 8.0;

/// The flattened document at one moment, ready to display.
pub struct Render {
    pub image: Raster,
    pub pyramid: Pyramid,
}

impl Render {
    pub fn new(image: Raster) -> Self {
        let pyramid = Pyramid::build(&image);
        Self { image, pyramid }
    }

    fn level(&self, index: usize) -> &Raster {
        self.pyramid.level(&self.image, index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TileKey {
    level: usize,
    col: u32,
    row: u32,
}

struct TileImage {
    generation: u64,
    key: TileKey,
    image: ColorImage,
}

struct TileTexture {
    texture: TextureHandle,
    bounds: tiles::TileBounds,
    /// Render generation the texture was made from.
    generation: u64,
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
    width: u32,
    height: u32,
    transform: Arc<DisplayTransform>,
    render: Option<Arc<Render>>,
    /// Bumped for every new render, to tell fresh tiles from stale ones.
    generation: u64,
    view: View,
    textures: HashMap<TileKey, TileTexture>,
    in_flight: HashSet<TileKey>,
    tx: Sender<TileImage>,
    rx: Receiver<TileImage>,
    checker: Option<TextureHandle>,
    /// Image pixel under the pointer, if any.
    pub hovered_pixel: Option<(u32, u32)>,
    /// Canvas area and scale from the last frame, for menu commands.
    rect: Rect,
    ppp: f32,
}

impl Canvas {
    pub fn new(width: u32, height: u32, transform: DisplayTransform) -> Self {
        let (tx, rx) = channel();
        Self {
            width,
            height,
            transform: Arc::new(transform),
            render: None,
            generation: 0,
            view: View {
                zoom: 1.0,
                origin: Vec2::ZERO,
                fit: true,
            },
            textures: HashMap::new(),
            in_flight: HashSet::new(),
            tx,
            rx,
            checker: None,
            hovered_pixel: None,
            rect: Rect::NOTHING,
            ppp: 1.0,
        }
    }

    /// Show a new render of the document. Old tiles stay visible until
    /// their replacements are converted.
    pub fn set_render(&mut self, render: Arc<Render>) {
        self.render = Some(render);
        self.generation += 1;
    }

    /// Flattened value of the pixel under the pointer.
    pub fn hovered_value(&self) -> Option<((u32, u32), Pixel)> {
        let (x, y) = self.hovered_pixel?;
        Some(((x, y), self.render.as_ref()?.image.get(x, y)))
    }

    /// Zoom as Photoshop shows it: "33.33%", "100%".
    pub fn zoom_label(&self) -> String {
        let text = format!("{:.2}", self.view.zoom * 100.0);
        format!("{}%", text.trim_end_matches('0').trim_end_matches('.'))
    }

    fn image_size(&self) -> Vec2 {
        vec2(self.width as f32, self.height as f32)
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

    pub fn show(&mut self, ui: &mut Ui, pasteboard: Color32) {
        let canvas = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(canvas, Sense::click_and_drag());
        let ppp = ui.pixels_per_point();
        self.rect = canvas;
        self.ppp = ppp;
        ui.painter().rect_filled(canvas, 0.0, pasteboard);

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
        let (zoom_delta, scroll, modifiers) =
            ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers));
        if zoom_delta != 1.0 {
            // Ctrl+scroll and touchpad pinch.
            self.zoom_to(self.view.zoom * zoom_delta, pointer, canvas);
        } else if modifiers.alt && scroll != Vec2::ZERO {
            // Photoshop's Alt+scroll zoom.
            let amount = if scroll.y != 0.0 { scroll.y } else { scroll.x };
            self.zoom_to(self.view.zoom * (amount * 0.005).exp(), pointer, canvas);
        } else if scroll != Vec2::ZERO && (modifiers == Modifiers::NONE || modifiers.shift_only()) {
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
    fn target_level(&self, levels: usize) -> usize {
        if self.view.zoom >= 1.0 {
            return 0;
        }
        let level = (1.0 / self.view.zoom).log2().floor() as usize;
        level.min(levels - 1)
    }

    fn receive_tiles(&mut self, ctx: &egui::Context) {
        let Some(render) = self.render.clone() else {
            return;
        };
        while let Ok(tile) = self.rx.try_recv() {
            self.in_flight.remove(&tile.key);
            if tile.generation != self.generation {
                // Made from an older render; it will be requested again.
                continue;
            }
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
            let level = render.level(tile.key.level);
            let bounds = tiles::bounds(level.width(), level.height(), tile.key.col, tile.key.row);
            let name = format!("tile-{}-{}-{}", tile.key.level, tile.key.col, tile.key.row);
            match self.textures.get_mut(&tile.key) {
                // Replace pixels in place, keeping the GPU texture.
                Some(existing) => {
                    existing.texture.set(tile.image, options);
                    existing.generation = tile.generation;
                }
                None => {
                    let texture = ctx.load_texture(name, tile.image, options);
                    self.textures.insert(
                        tile.key,
                        TileTexture {
                            texture,
                            bounds,
                            generation: tile.generation,
                        },
                    );
                }
            }
        }
    }

    fn checker_texture(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.checker
            .get_or_insert_with(|| {
                let (a, b) = (Color32::from_gray(0x5a), Color32::from_gray(0x44));
                let image = ColorImage::new([2, 2], vec![a, b, b, a]);
                let options = TextureOptions {
                    magnification: TextureFilter::Nearest,
                    minification: TextureFilter::Nearest,
                    wrap_mode: TextureWrapMode::Repeat,
                    ..Default::default()
                };
                ctx.load_texture("checker", image, options)
            })
            .id()
    }

    fn draw(&mut self, ui: &Ui, canvas: Rect, ppp: f32) {
        let painter = ui.painter_at(canvas);
        let origin = canvas.min + self.snapped_origin(ppp);
        let scale = self.view.zoom / ppp;
        let size = self.image_size();
        let image_rect = Rect::from_min_size(origin, size * scale);

        // Checkerboard behind transparent areas, one repeating quad.
        let checker = self.checker_texture(ui.ctx());
        let uv = Rect::from_min_size(Pos2::ZERO, image_rect.size() / (CHECKER * 2.0));
        painter.image(checker, image_rect, uv, Color32::WHITE);

        let Some(render) = self.render.clone() else {
            return;
        };
        let levels = render.pyramid.len();
        let target = self.target_level(levels);
        let top = levels - 1;
        let visible = canvas.intersect(image_rect);
        if !visible.is_positive() {
            return;
        }

        let level = render.level(target);
        let to_image = vec2(
            size.x / level.width() as f32,
            size.y / level.height() as f32,
        );
        let (cols, rows) = tiles::grid(level.width(), level.height());
        let lo = ((visible.min - origin) / scale) / to_image;
        let hi = ((visible.max - origin) / scale) / to_image;
        let tile = tiles::TILE_SIZE as f32;
        let c0 = (lo.x / tile).floor().max(0.0) as u32;
        let r0 = (lo.y / tile).floor().max(0.0) as u32;
        let c1 = ((hi.x / tile).ceil() as u32).min(cols);
        let r1 = ((hi.y / tile).ceil() as u32).min(rows);
        let focus = (lo + hi) * 0.5;

        let mut wanted: Vec<(bool, f32, TileKey)> = Vec::new();
        let mut want = |key: TileKey,
                        distance: f32,
                        textures: &HashMap<TileKey, TileTexture>,
                        generation: u64| {
            let fresh = textures
                .get(&key)
                .is_some_and(|t| t.generation == generation);
            if !fresh {
                wanted.push((key.level == top, distance, key));
            }
        };

        for row in r0..r1 {
            for col in c0..c1 {
                let key = TileKey {
                    level: target,
                    col,
                    row,
                };
                let b = tiles::bounds(level.width(), level.height(), col, row);
                // The tile's content, in image pixels.
                let region = Rect::from_min_max(
                    pos2(b.x as f32 * to_image.x, b.y as f32 * to_image.y),
                    pos2(
                        (b.x + b.w) as f32 * to_image.x,
                        (b.y + b.h) as f32 * to_image.y,
                    ),
                );
                let distance =
                    (vec2((col as f32 + 0.5) * tile, (row as f32 + 0.5) * tile) - focus).length();
                want(key, distance, &self.textures, self.generation);

                // Draw the best texture available for this region: the tile
                // itself, or the part of a coarser tile that covers it.
                let source = (target..levels).find_map(|l| {
                    let lv = render.level(l);
                    let k = to_image_ratio(size, lv);
                    let key = TileKey {
                        level: l,
                        col: (region.min.x / k.x) as u32 / tiles::TILE_SIZE,
                        row: (region.min.y / k.y) as u32 / tiles::TILE_SIZE,
                    };
                    self.textures.get(&key).map(|t| (t, k))
                });
                match source {
                    Some((t, k)) => {
                        let b = t.bounds;
                        let tex_min = vec2(b.tex_x as f32, b.tex_y as f32);
                        let tex_size = vec2(b.tex_w as f32, b.tex_h as f32);
                        let uv = Rect::from_min_max(
                            ((region.min.to_vec2() / k - tex_min) / tex_size).to_pos2(),
                            ((region.max.to_vec2() / k - tex_min) / tex_size).to_pos2(),
                        );
                        let screen = Rect::from_min_max(
                            origin + region.min.to_vec2() * scale,
                            origin + region.max.to_vec2() * scale,
                        );
                        painter.image(t.texture.id(), screen, uv, Color32::WHITE);
                    }
                    None => {
                        // Nothing yet: ask for the tiny top-level tile too, as
                        // an instant placeholder.
                        let lv = render.level(top);
                        let k = to_image_ratio(size, lv);
                        let key = TileKey {
                            level: top,
                            col: (region.min.x / k.x) as u32 / tiles::TILE_SIZE,
                            row: (region.min.y / k.y) as u32 / tiles::TILE_SIZE,
                        };
                        want(key, 0.0, &self.textures, self.generation);
                    }
                }
            }
        }

        // Placeholders first, then the tiles nearest the middle of the view.
        wanted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.total_cmp(&b.1)));
        wanted.dedup_by_key(|w| w.2);
        for (_, _, key) in wanted {
            if self.in_flight.len() >= MAX_IN_FLIGHT {
                break;
            }
            if self.in_flight.insert(key) {
                self.request(key, &render, ui.ctx().clone());
            }
        }
    }

    fn request(&self, key: TileKey, render: &Arc<Render>, ctx: egui::Context) {
        let render = Arc::clone(render);
        let transform = Arc::clone(&self.transform);
        let tx = self.tx.clone();
        let generation = self.generation;
        rayon::spawn(move || {
            let level = render.level(key.level);
            let b = tiles::bounds(level.width(), level.height(), key.col, key.row);
            let rgba = tiles::render(level, &transform, b);
            let image =
                ColorImage::from_rgba_unmultiplied([b.tex_w as usize, b.tex_h as usize], &rgba);
            if tx
                .send(TileImage {
                    generation,
                    key,
                    image,
                })
                .is_ok()
            {
                ctx.request_repaint();
            }
        });
    }
}

/// Image pixels per level pixel, per axis (odd sizes round up when halving).
fn to_image_ratio(size: Vec2, level: &Raster) -> Vec2 {
    vec2(
        size.x / level.width() as f32,
        size.y / level.height() as f32,
    )
}
