//! The image canvas: view transform, progressive tile streaming,
//! Photoshop-style navigation, and pointer input for painting.
//!
//! The canvas shows a [`Render`]: the flattened document plus its zoom
//! pyramid. Whole renders are produced in the background when the document
//! changes; brush strokes update just the area they touch, in place.
//! Visible tiles are converted to display colour on worker threads and
//! uploaded as textures. Tiles that go stale stay on screen until their
//! replacements are ready, so edits never flicker.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, RwLock};

use egui::{
    Color32, ColorImage, CursorIcon, Key, Modifiers, PointerButton, Pos2, Rect, Sense, Stroke,
    TextureFilter, TextureHandle, TextureOptions, TextureWrapMode, Ui, Vec2, pos2, vec2,
};
use omapix_engine::layer::Layer;
use omapix_engine::pyramid::Pyramid;
use omapix_engine::tiled::TILE;
use omapix_engine::{DisplayTransform, Pixel, Raster, composite, tiles};

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

/// The flattened document at one moment, ready to display. Brush strokes
/// update it in place while tile workers read it, hence the lock.
pub struct Render {
    data: RwLock<RenderData>,
}

struct RenderData {
    image: Raster,
    pyramid: Pyramid,
}

impl RenderData {
    fn level(&self, index: usize) -> &Raster {
        self.pyramid.level(&self.image, index)
    }
}

impl Render {
    pub fn new(image: Raster) -> Self {
        let pyramid = Pyramid::build(&image);
        Self {
            data: RwLock::new(RenderData { image, pyramid }),
        }
    }

    /// Recomposite the given 256 px engine tiles from `layers`. Returns the
    /// changed area in image pixels as (x0, y0, x1, y1).
    pub fn update_tiles(
        &self,
        layers: &[Layer],
        engine_tiles: &[(u32, u32)],
    ) -> Option<(u32, u32, u32, u32)> {
        let mut data = self.data.write().expect("render lock");
        let (w, h) = (data.image.width(), data.image.height());
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for &(col, row) in engine_tiles {
            x0 = x0.min(col * TILE);
            y0 = y0.min(row * TILE);
            x1 = x1.max(((col + 1) * TILE).min(w));
            y1 = y1.max(((row + 1) * TILE).min(h));
        }
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        composite::composite_into(layers, &mut data.image, engine_tiles);
        let RenderData { image, pyramid } = &mut *data;
        pyramid.update_region(image, (x0, y0, x1, y1));
        Some((x0, y0, x1, y1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TileKey {
    level: usize,
    col: u32,
    row: u32,
}

struct TileImage {
    /// Value of the canvas's generation counter when requested.
    stamp: u64,
    key: TileKey,
    image: ColorImage,
}

struct TileTexture {
    texture: TextureHandle,
    bounds: tiles::TileBounds,
    stamp: u64,
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

/// Where a clone or heal tool copies from, drawn as a crosshair.
#[derive(Clone, Copy, Debug)]
pub enum SourceMarker {
    /// A fixed image point (source set, no stroke yet).
    Fixed(Pos2),
    /// An offset from the pointer, in image pixels.
    Offset(Vec2),
}

/// Pointer input meant for the active tool, in image pixels.
#[derive(Clone, Copy, Debug)]
pub enum ToolInput {
    StrokeBegin(Pos2),
    StrokeMove(Pos2),
    StrokeEnd,
    /// Alt+click or Alt+drag: sample the colour here.
    Sample(Pos2),
}

pub struct Canvas {
    width: u32,
    height: u32,
    /// Size of every pyramid level, level 0 first. Fixed per document.
    levels: Vec<(u32, u32)>,
    transform: Arc<DisplayTransform>,
    render: Option<Arc<Render>>,
    /// Incremented on every invalidation. Textures made from a request
    /// stamped before a tile's last invalidation are stale.
    generation: u64,
    /// Every tile is stale if stamped before this (a whole new render).
    stale_before: u64,
    /// Per-tile invalidations from brush strokes.
    dirty: HashMap<TileKey, u64>,
    view: View,
    textures: HashMap<TileKey, TileTexture>,
    in_flight: HashSet<TileKey>,
    tx: Sender<TileImage>,
    rx: Receiver<TileImage>,
    checker: Option<TextureHandle>,
    /// A brush stroke is in progress.
    painting: bool,
    /// Image pixel under the pointer, if any.
    pub hovered_pixel: Option<(u32, u32)>,
    /// Canvas area and scale from the last frame, for menu commands.
    rect: Rect,
    ppp: f32,
}

fn level_sizes(width: u32, height: u32) -> Vec<(u32, u32)> {
    // Mirrors Pyramid::build: halve until the longest side is at most 128.
    let mut sizes = vec![(width, height)];
    while let Some(&(w, h)) = sizes.last() {
        if w.max(h) <= 128 {
            break;
        }
        sizes.push((w.div_ceil(2), h.div_ceil(2)));
    }
    sizes
}

impl Canvas {
    pub fn new(width: u32, height: u32, transform: DisplayTransform) -> Self {
        let (tx, rx) = channel();
        Self {
            width,
            height,
            levels: level_sizes(width, height),
            transform: Arc::new(transform),
            render: None,
            generation: 0,
            stale_before: 0,
            dirty: HashMap::new(),
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
            painting: false,
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
        self.stale_before = self.generation;
        self.dirty.clear();
    }

    /// Laid out and showing a render, so view commands and strokes work.
    pub fn ready(&self) -> bool {
        self.rect.is_positive() && self.render.is_some()
    }

    pub fn render(&self) -> Option<&Arc<Render>> {
        self.render.as_ref()
    }

    /// Mark display tiles covering an image-pixel area as out of date.
    pub fn invalidate(&mut self, (x0, y0, x1, y1): (u32, u32, u32, u32)) {
        self.generation += 1;
        for (level, &(lw, lh)) in self.levels.iter().enumerate() {
            let (kx, ky) = (
                self.width as f32 / lw as f32,
                self.height as f32 / lh as f32,
            );
            // Textures carry a 1 px border from their neighbours.
            let lx0 = ((x0 as f32 / kx).floor() as u32).saturating_sub(1);
            let ly0 = ((y0 as f32 / ky).floor() as u32).saturating_sub(1);
            let lx1 = ((x1 as f32 / kx).ceil() as u32 + 1).min(lw);
            let ly1 = ((y1 as f32 / ky).ceil() as u32 + 1).min(lh);
            let t = tiles::TILE_SIZE;
            for row in ly0 / t..=(ly1.saturating_sub(1)) / t {
                for col in lx0 / t..=(lx1.saturating_sub(1)) / t {
                    self.dirty
                        .insert(TileKey { level, col, row }, self.generation);
                }
            }
        }
    }

    fn is_fresh(&self, key: TileKey, stamp: u64) -> bool {
        stamp >= self.stale_before && self.dirty.get(&key).is_none_or(|&d| stamp >= d)
    }

    /// Flattened value of the pixel under the pointer.
    pub fn hovered_value(&self) -> Option<((u32, u32), Pixel)> {
        let (x, y) = self.hovered_pixel?;
        Some(((x, y), self.sample(x, y)?))
    }

    /// Flattened value of an image pixel.
    pub fn sample(&self, x: u32, y: u32) -> Option<Pixel> {
        let data = self.render.as_ref()?.data.read().ok()?;
        (x < self.width && y < self.height).then(|| data.image.get(x, y))
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

    /// Show image point `p` in the middle of the canvas at 100 %.
    pub fn look_at(&mut self, p: Pos2) {
        self.view.zoom = 1.0;
        self.view.fit = false;
        self.view.origin = self.rect.size() * 0.5 - p.to_vec2() / self.ppp;
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

    /// Screen position (points) to image pixels.
    fn to_image(&self, p: Pos2) -> Pos2 {
        ((p - self.rect.min - self.snapped_origin(self.ppp)) / (self.view.zoom / self.ppp))
            .to_pos2()
    }

    /// Draw the canvas and handle navigation. `brush` is the diameter of the
    /// active brush in image pixels, to draw its outline at the pointer;
    /// `source` marks where a clone or heal tool copies from. Returns pointer
    /// input for the active tool.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        pasteboard: Color32,
        brush: Option<f32>,
        source: Option<SourceMarker>,
    ) -> Option<ToolInput> {
        let canvas = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(canvas, Sense::click_and_drag());
        let ppp = ui.pixels_per_point();
        self.rect = canvas;
        self.ppp = ppp;
        ui.painter().rect_filled(canvas, 0.0, pasteboard);

        let navigating = self.navigate(ui, &response, canvas);
        if self.view.fit {
            let z = self.fit_zoom(canvas, ppp);
            self.center_at(z, canvas, ppp);
        }
        let input = if navigating || brush.is_none() {
            None
        } else {
            self.tool_input(ui, &response)
        };

        self.receive_tiles(ui.ctx());
        self.draw(ui, canvas, ppp);

        self.hovered_pixel = response.hover_pos().and_then(|p| {
            let img = self.to_image(p);
            let size = self.image_size();
            (img.x >= 0.0 && img.y >= 0.0 && img.x < size.x && img.y < size.y)
                .then_some((img.x as u32, img.y as u32))
        });
        if let (Some(diameter), Some(pointer)) = (brush, response.hover_pos())
            && !navigating
        {
            self.brush_cursor(ui, pointer, diameter);
        }
        if let Some(marker) = source {
            let scale = self.view.zoom / self.ppp;
            let at = match marker {
                SourceMarker::Fixed(p) => {
                    Some(self.rect.min + self.snapped_origin(self.ppp) + p.to_vec2() * scale)
                }
                SourceMarker::Offset(d) => response.hover_pos().map(|p| p + d * scale),
            };
            if let Some(at) = at {
                let painter = ui.painter_at(self.rect);
                for (width, colour) in
                    [(3.0, Color32::from_black_alpha(160)), (1.0, Color32::WHITE)]
                {
                    let stroke = Stroke::new(width, colour);
                    painter.line_segment([at - vec2(8.0, 0.0), at + vec2(8.0, 0.0)], stroke);
                    painter.line_segment([at - vec2(0.0, 8.0), at + vec2(0.0, 8.0)], stroke);
                }
            }
        }
        input
    }

    fn tool_input(&mut self, ui: &Ui, response: &egui::Response) -> Option<ToolInput> {
        let alt = ui.input(|i| i.modifiers.alt);
        let pointer = ui
            .input(|i| i.pointer.interact_pos())
            .map(|p| self.to_image(p));
        if alt {
            let sampling = response.clicked_by(PointerButton::Primary)
                || response.dragged_by(PointerButton::Primary);
            return pointer.filter(|_| sampling).map(ToolInput::Sample);
        }
        if response.drag_started_by(PointerButton::Primary) {
            self.painting = true;
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .map(|p| self.to_image(p));
            return origin.or(pointer).map(ToolInput::StrokeBegin);
        }
        if self.painting && response.dragged_by(PointerButton::Primary) {
            return pointer.map(ToolInput::StrokeMove);
        }
        if self.painting && (response.drag_stopped() || !ui.input(|i| i.pointer.primary_down())) {
            self.painting = false;
            return Some(ToolInput::StrokeEnd);
        }
        if response.clicked_by(PointerButton::Primary) {
            // A click without dragging paints a single dab; the next frame
            // ends the stroke.
            self.painting = true;
            return pointer.map(ToolInput::StrokeBegin);
        }
        None
    }

    fn brush_cursor(&self, ui: &Ui, pointer: Pos2, diameter: f32) {
        let radius = diameter * self.view.zoom / self.ppp / 2.0;
        let painter = ui.painter_at(self.rect);
        if radius >= 3.0 {
            // Dark and light rings, visible on any image.
            painter.circle_stroke(
                pointer,
                radius,
                Stroke::new(1.5, Color32::from_black_alpha(160)),
            );
            painter.circle_stroke(
                pointer,
                radius,
                Stroke::new(0.75, Color32::from_white_alpha(200)),
            );
            ui.ctx().set_cursor_icon(CursorIcon::None);
        } else {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
    }

    /// Pan and zoom. Returns true while the pointer is being used to pan.
    fn navigate(&mut self, ui: &Ui, response: &egui::Response, canvas: Rect) -> bool {
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

        if let Some(pointer) = response.hover_pos() {
            let (zoom_delta, scroll, modifiers) =
                ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers));
            if zoom_delta != 1.0 {
                // Ctrl+scroll and touchpad pinch.
                self.zoom_to(self.view.zoom * zoom_delta, pointer, canvas);
            } else if modifiers.alt && scroll != Vec2::ZERO {
                // Photoshop's Alt+scroll zoom.
                let amount = if scroll.y != 0.0 { scroll.y } else { scroll.x };
                self.zoom_to(self.view.zoom * (amount * 0.005).exp(), pointer, canvas);
            } else if scroll != Vec2::ZERO
                && (modifiers == Modifiers::NONE || modifiers.shift_only())
            {
                self.view.origin += scroll;
                self.view.fit = false;
            }
        }
        space || panning
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
        level.min(self.levels.len() - 1)
    }

    fn receive_tiles(&mut self, ctx: &egui::Context) {
        while let Ok(tile) = self.rx.try_recv() {
            self.in_flight.remove(&tile.key);
            if !self.is_fresh(tile.key, tile.stamp) {
                // Made before the area last changed; it will be requested again.
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
            let (lw, lh) = self.levels[tile.key.level];
            let bounds = tiles::bounds(lw, lh, tile.key.col, tile.key.row);
            match self.textures.get_mut(&tile.key) {
                // Replace pixels in place, keeping the GPU texture.
                Some(existing) => {
                    existing.texture.set(tile.image, options);
                    existing.stamp = tile.stamp;
                }
                None => {
                    let name = format!("tile-{}-{}-{}", tile.key.level, tile.key.col, tile.key.row);
                    let texture = ctx.load_texture(name, tile.image, options);
                    self.textures.insert(
                        tile.key,
                        TileTexture {
                            texture,
                            bounds,
                            stamp: tile.stamp,
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

    /// Image pixels per level pixel, per axis (odd sizes round up when halving).
    fn ratio(&self, level: usize) -> Vec2 {
        let (lw, lh) = self.levels[level];
        vec2(
            self.width as f32 / lw as f32,
            self.height as f32 / lh as f32,
        )
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
        let levels = self.levels.len();
        let target = self.target_level();
        let top = levels - 1;
        let visible = canvas.intersect(image_rect);
        if !visible.is_positive() {
            return;
        }

        let (lw, lh) = self.levels[target];
        let to_image = self.ratio(target);
        let (cols, rows) = tiles::grid(lw, lh);
        let lo = ((visible.min - origin) / scale) / to_image;
        let hi = ((visible.max - origin) / scale) / to_image;
        let tile = tiles::TILE_SIZE as f32;
        let c0 = (lo.x / tile).floor().max(0.0) as u32;
        let r0 = (lo.y / tile).floor().max(0.0) as u32;
        let c1 = ((hi.x / tile).ceil() as u32).min(cols);
        let r1 = ((hi.y / tile).ceil() as u32).min(rows);
        let focus = (lo + hi) * 0.5;

        let mut wanted: Vec<(bool, f32, TileKey)> = Vec::new();
        for row in r0..r1 {
            for col in c0..c1 {
                let key = TileKey {
                    level: target,
                    col,
                    row,
                };
                let b = tiles::bounds(lw, lh, col, row);
                // The tile's content, in image pixels.
                let region = Rect::from_min_max(
                    pos2(b.x as f32 * to_image.x, b.y as f32 * to_image.y),
                    pos2(
                        (b.x + b.w) as f32 * to_image.x,
                        (b.y + b.h) as f32 * to_image.y,
                    ),
                );
                if !self
                    .textures
                    .get(&key)
                    .is_some_and(|t| self.is_fresh(key, t.stamp))
                {
                    let distance = (vec2((col as f32 + 0.5) * tile, (row as f32 + 0.5) * tile)
                        - focus)
                        .length();
                    wanted.push((false, distance, key));
                }

                // Draw the best texture available for this region: the tile
                // itself, or the part of a coarser tile that covers it.
                let source = (target..levels).find_map(|l| {
                    let k = self.ratio(l);
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
                        let k = self.ratio(top);
                        let key = TileKey {
                            level: top,
                            col: (region.min.x / k.x) as u32 / tiles::TILE_SIZE,
                            row: (region.min.y / k.y) as u32 / tiles::TILE_SIZE,
                        };
                        wanted.push((true, 0.0, key));
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
        let stamp = self.generation;
        rayon::spawn(move || {
            let rgba_and_size = {
                let Ok(data) = render.data.read() else { return };
                let level = data.level(key.level);
                let b = tiles::bounds(level.width(), level.height(), key.col, key.row);
                (
                    tiles::render(level, &transform, b),
                    [b.tex_w as usize, b.tex_h as usize],
                )
            };
            let (rgba, size) = rgba_and_size;
            let image = ColorImage::from_rgba_unmultiplied(size, &rgba);
            if tx.send(TileImage { stamp, key, image }).is_ok() {
                ctx.request_repaint();
            }
        });
    }
}
