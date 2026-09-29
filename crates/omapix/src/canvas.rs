//! The image canvas: view transform, progressive tile streaming,
//! Photoshop-style navigation, and pointer input for painting.
//!
//! The canvas shows a [`Render`]: the flattened document plus its zoom
//! pyramid. The editor redraws it in place when the document changes, the
//! part on screen first; brush strokes update just the area they touch.
//! Visible tiles are converted to display colour on worker threads and
//! uploaded as textures. Tiles that go stale stay on screen until their
//! replacements are ready, so edits never flicker.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use egui::{
    Color32, ColorImage, CursorIcon, Key, Modifiers, PointerButton, Pos2, Rect, Sense, Stroke,
    StrokeKind, TextureFilter, TextureHandle, TextureOptions, TextureWrapMode, Ui, Vec2, pos2, vec2,
};
use omapix_engine::pyramid::Pyramid;
use omapix_engine::tiled::TILE;
use omapix_engine::{DisplayTransform, Pixel, Raster, tiles};

use crate::tools::CursorBadge;
use crate::gpu::LiveDraw;
use crate::live::LiveFrame;

/// Photoshop's zoom presets, as fractions.
pub const ZOOM_STEPS: [f32; 21] = [
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

/// Marching ants: dash and gap lengths in points, how fast they march in
/// points per second, and how often they're redrawn.
const ANT_DASH: f32 = 4.0;
const ANT_GAP: f32 = 4.0;
const ANT_PERIOD: f32 = ANT_DASH + ANT_GAP;
const ANT_SPEED: f64 = 12.0;
const ANT_INTERVAL: Duration = Duration::from_millis(80);

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

    pub fn with_image<R>(&self, f: impl FnOnce(&Raster) -> R) -> Option<R> {
        self.data.read().ok().map(|d| f(&d.image))
    }

    /// The smallest pyramid level at least `side` pixels on its longer
    /// side (or the largest there is), for a thumbnail that sharp.
    pub fn level_at_least(&self, side: u32) -> Raster {
        let data = self.data.read().expect("render lock");
        let big_enough = (0..data.pyramid.len()).rev().find(|&i| {
            let level = data.level(i);
            level.width().max(level.height()) >= side
        });
        data.level(big_enough.unwrap_or(0)).clone()
    }

    #[cfg(test)]
    pub fn sample_for_test(&self, x: u32, y: u32) -> Pixel {
        self.data.read().expect("render lock").image.get(x, y)
    }

    #[cfg(test)]
    pub fn level_for_test(&self, level: usize) -> Raster {
        self.data.read().expect("render lock").level(level).clone()
    }

    /// Write whole 256 px tiles (col, row) of pyramid level `level`, as
    /// made by [`omapix_engine::composite::composite_tiles`], then update
    /// the levels above it. Nothing is written if `cancel` is set, which is checked under the
    /// lock, so a cancelled background render can't overwrite a brush
    /// stroke drawn since. Returns whether the tiles were written.
    pub fn write_tiles(
        &self,
        level: usize,
        tiles: &[(u32, u32)],
        data: &[Vec<Pixel>],
        cancel: Option<&AtomicBool>,
    ) -> bool {
        let mut data_lock = self.data.write().expect("render lock");
        if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
            return false;
        }
        let RenderData { image, pyramid } = &mut *data_lock;
        let target = pyramid.level_mut(image, level);
        let (w, h) = (target.width(), target.height());
        for (&(col, row), tile) in tiles.iter().zip(data) {
            target.put_tile(col, row, tile);
        }
        for &(col, row) in tiles {
            let (x0, y0) = (col * TILE, row * TILE);
            if x0 < w && y0 < h {
                let area = (x0, y0, (x0 + TILE).min(w), (y0 + TILE).min(h));
                pyramid.update_from(image, level, area);
            }
        }
        true
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

/// What the active tool wants from the canvas this frame.
#[derive(Default)]
pub struct Overlay<'a> {
    /// Send pointer input to the tool.
    pub tool: bool,
    /// Alt+click samples (eyedropper, clone source) rather than starting a
    /// stroke with Alt held (selection subtract).
    pub alt_samples: bool,
    /// The Eyedropper tool samples on primary click or drag.
    pub samples: bool,
    /// Brush diameter in image pixels, for its outline at the pointer.
    pub brush: Option<f32>,
    /// Show the Move tool's cursor rather than a crosshair (without a brush).
    pub moves: bool,
    pub source: Option<SourceMarker>,
    /// Selection outlines, image pixels, drawn as marching ants.
    pub selection: &'a [Vec<(f32, f32)>],
    /// A shape being drawn (lasso path or marquee), image pixels.
    pub drawing: Option<&'a [Pos2]>,
    /// Modifier badge (+, -, ×, copy) shown near the cursor.
    pub badge: Option<CursorBadge>,
    /// Free Transform's box, its corners in image pixels clockwise from the
    /// top left, and the cursor for what's under the pointer.
    pub transform: Option<([Pos2; 4], CursorIcon)>,
    /// The box is the Crop tool's: what it leaves out is darkened.
    pub crop: bool,
}

/// Pointer input meant for the active tool, in image pixels.
#[derive(Clone, Copy, Debug)]
pub enum ToolInput {
    StrokeBegin(Pos2),
    StrokeMove(Pos2),
    StrokeEnd,
    /// Alt+click or Alt+drag: sample the colour here.
    Sample(Pos2),
    /// Alt+right-drag: change the brush's size (diameter, image pixels) and
    /// hardness by these amounts, as in Photoshop.
    BrushDrag { size: f32, hardness: f32 },
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
    /// Where an Alt+right-drag resizing the brush started (screen points),
    /// to keep its outline there.
    resizing: Option<Pos2>,
    /// Image pixel under the pointer, if any.
    pub hovered_pixel: Option<(u32, u32)>,
    /// Where the pointer is over the canvas, in image pixels.
    pub pointer: Option<Pos2>,
    /// Canvas area and scale from the last frame, for menu commands.
    rect: Rect,
    ppp: f32,
    /// A Move drag or a slider drag shown live on the GPU instead of the
    /// tiles (see live.rs).
    pub live: Option<LiveFrame>,
    /// The display transform as a lookup table, for drawing live.
    display_lut: std::sync::OnceLock<Arc<Vec<[f32; 4]>>>,
    /// Every display tile on screen was up to date when last drawn.
    pub fresh: bool,
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
            resizing: None,
            live: None,
            display_lut: std::sync::OnceLock::new(),
            fresh: false,
            hovered_pixel: None,
            pointer: None,
            rect: Rect::NOTHING,
            ppp: 1.0,
        }
    }

    /// Show a new render of the document. Old tiles stay visible until
    /// their replacements are converted.
    pub fn set_render(&mut self, render: Arc<Render>) {
        self.render = Some(render);
        self.fresh = false;
        self.generation += 1;
        self.stale_before = self.generation;
        self.dirty.clear();
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    #[cfg(test)]
    pub fn levels(&self) -> &[(u32, u32)] {
        &self.levels
    }

    /// Rebuild state when the document size changes (e.g. 90° rotation, undo/redo).
    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.levels = level_sizes(width, height);
        self.render = None;
        self.textures.clear();
        self.dirty.clear();
        self.in_flight.clear();
        self.live = None;
        self.generation += 1;
        self.stale_before = self.generation;
        self.fresh = false;
        self.view.fit = true;
        if self.rect.is_positive() {
            let z = self.fit_zoom(self.rect, self.ppp);
            self.center_at(z, self.rect, self.ppp);
        }
    }

    /// Document colour to display colour.
    pub fn transform(&self) -> &DisplayTransform {
        &self.transform
    }

    /// Laid out and showing a render, so view commands and strokes work.
    pub fn ready(&self) -> bool {
        self.rect.is_positive() && self.render.is_some()
    }

    pub fn render(&self) -> Option<&Arc<Render>> {
        self.render.as_ref()
    }

    /// Lay the canvas out at `zoom` over `rect`, as a frame would.
    #[cfg(test)]
    pub fn lay_out_for_test(&mut self, rect: Rect, zoom: f32) {
        self.rect = rect;
        self.ppp = 1.0;
        self.view.fit = false;
        self.view.zoom = zoom;
    }

    /// The pyramid level drawn at the current zoom, and the part of the
    /// image on screen as (x0, y0, x1, y1) in image pixels. `None` before
    /// the canvas is laid out, or while the image is off screen.
    pub fn visible_area(&self) -> Option<(usize, (u32, u32, u32, u32))> {
        if !self.rect.is_positive() {
            return None;
        }
        let lo = self.to_image(self.rect.min);
        let hi = self.to_image(self.rect.max);
        let (w, h) = (self.width as f32, self.height as f32);
        let x0 = lo.x.clamp(0.0, w).floor() as u32;
        let y0 = lo.y.clamp(0.0, h).floor() as u32;
        let x1 = hi.x.clamp(0.0, w).ceil() as u32;
        let y1 = hi.y.clamp(0.0, h).ceil() as u32;
        (x0 < x1 && y0 < y1).then_some((self.target_level(), (x0, y0, x1, y1)))
    }

    /// Mark display tiles covering 256 px tiles (col, row) of pyramid level
    /// `level` as out of date, after [`Render::write_tiles`].
    pub fn invalidate_tiles(&mut self, level: usize, tiles: &[(u32, u32)]) {
        // Not fresh again until a frame has drawn the new tiles.
        self.fresh = false;
        self.generation += 1;
        let (w, h) = (self.width, self.height);
        for &(col, row) in tiles {
            let (x0, y0) = ((col * TILE) << level, (row * TILE) << level);
            if x0 < w && y0 < h {
                let (x1, y1) = (((col + 1) * TILE) << level, ((row + 1) * TILE) << level);
                self.mark_dirty((x0, y0, x1.min(w), y1.min(h)));
            }
        }
    }

    /// Mark display tiles covering an image-pixel area as out of date.
    fn mark_dirty(&mut self, (x0, y0, x1, y1): (u32, u32, u32, u32)) {
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

    pub fn zoom(&self) -> f32 {
        self.view.zoom
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        let canvas = self.rect;
        self.zoom_to(zoom, canvas.center(), canvas);
    }

    /// Centre the view on image point `p` (in image pixels) at the current zoom.
    pub fn center_on(&mut self, p: Pos2) {
        if self.view.fit {
            self.view.zoom = self.fit_zoom(self.rect, self.ppp);
            self.view.fit = false;
        }
        self.view.origin = self.rect.size() * 0.5 - p.to_vec2() * (self.view.zoom / self.ppp);
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
        overlay: Overlay<'_>,
    ) -> (Option<ToolInput>, egui::Response) {
        let Overlay {
            tool,
            alt_samples,
            samples,
            brush,
            moves,
            source,
            selection,
            drawing,
            badge,
            transform,
            crop,
        } = overlay;
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
        let input = if navigating || !tool {
            None
        } else {
            self.tool_input(ui, &response, alt_samples, samples)
        };

        self.receive_tiles(ui.ctx());
        self.draw(ui, canvas, ppp);

        self.hovered_pixel = response.hover_pos().and_then(|p| {
            let img = self.to_image(p);
            let size = self.image_size();
            (img.x >= 0.0 && img.y >= 0.0 && img.x < size.x && img.y < size.y)
                .then_some((img.x as u32, img.y as u32))
        });
        self.pointer = response.hover_pos().map(|p| self.to_image(p));
        self.draw_outlines(ui, selection, drawing);
        if let Some((corners, cursor)) = transform {
            self.draw_transform_box(ui, corners, crop);
            if response.hover_pos().is_some() && !navigating {
                ui.ctx().set_cursor_icon(cursor);
            }
        } else if let Some(pointer) = response.hover_pos()
            && !navigating
            && tool
        {
            match brush {
                Some(diameter) => self.brush_cursor(ui, self.resizing.unwrap_or(pointer), diameter),
                None => self.drawn_cursor(ui, pointer, moves),
            }
            if let Some(badge) = badge {
                self.cursor_badge(ui, pointer, badge);
            }
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
        (input, response)
    }

    /// Image pixels per screen point, at the current zoom.
    pub fn image_per_point(&self) -> f32 {
        self.ppp / self.view.zoom
    }

    /// Free Transform's box: its outline, a handle at each corner and side,
    /// and a mark at the centre. For `crop`, the box is upright, and
    /// what's outside it is darkened.
    fn draw_transform_box(&self, ui: &Ui, corners: [Pos2; 4], crop: bool) {
        let painter = ui.painter_at(self.rect);
        let c = corners.map(|p| self.to_screen((p.x, p.y)));
        if crop {
            let (inside, all, shade) = (Rect::from_two_pos(c[0], c[2]), self.rect, Color32::from_black_alpha(140));
            for r in [
                Rect::from_x_y_ranges(all.x_range(), all.min.y..=inside.min.y),
                Rect::from_x_y_ranges(all.x_range(), inside.max.y..=all.max.y),
                Rect::from_x_y_ranges(all.min.x..=inside.min.x, inside.y_range()),
                Rect::from_x_y_ranges(inside.max.x..=all.max.x, inside.y_range()),
            ] {
                if r.is_positive() {
                    painter.rect_filled(r, 0.0, shade);
                }
            }
        }
        let mut outline = c.to_vec();
        outline.push(c[0]);
        painter.add(egui::Shape::line(outline.clone(), Stroke::new(3.0, Color32::from_black_alpha(160))));
        painter.add(egui::Shape::line(outline, Stroke::new(1.0, Color32::WHITE)));
        let sides = (0..4).map(|i| c[i] + (c[(i + 1) % 4] - c[i]) / 2.0);
        for at in c.into_iter().chain(sides) {
            let square = Rect::from_center_size(at, vec2(7.0, 7.0));
            painter.rect_filled(square, 0.0, Color32::WHITE);
            painter.rect_stroke(square, 0.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Middle);
        }
        let centre = c[0] + (c[2] - c[0]) / 2.0;
        painter.circle_stroke(centre, 3.0, Stroke::new(1.0, Color32::WHITE));
    }

    /// Screen position (points) of an image position.
    fn to_screen(&self, (x, y): (f32, f32)) -> Pos2 {
        self.rect.min + self.snapped_origin(self.ppp) + vec2(x, y) * (self.view.zoom / self.ppp)
    }

    /// Marching ants round the selection, and the shape being drawn.
    fn draw_outlines(&self, ui: &Ui, selection: &[Vec<(f32, f32)>], drawing: Option<&[Pos2]>) {
        if selection.is_empty() && drawing.is_none() {
            return;
        }

        let time = ui.input(|i| i.time);
        let offset = ((time * ANT_SPEED) % (ANT_PERIOD as f64)) as f32;

        let painter = ui.painter_at(self.rect);
        let mut on_screen = false;

        let mut draw_ants = |mut points: Vec<Pos2>, closed: bool| {
            if points.len() < 2 {
                return;
            }
            if closed && points.first() != points.last() {
                points.push(points[0]);
            }

            let mut min = points[0];
            let mut max = points[0];
            for &p in &points[1..] {
                min.x = min.x.min(p.x);
                min.y = min.y.min(p.y);
                max.x = max.x.max(p.x);
                max.y = max.y.max(p.y);
            }
            let bbox = Rect::from_min_max(min, max);
            if !bbox.intersects(self.rect) {
                return;
            }
            on_screen = true;

            painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(1.0, Color32::BLACK),
            ));
            let mut shapes = Vec::new();
            dashed_path(
                &points,
                Stroke::new(1.0, Color32::WHITE),
                ANT_DASH,
                ANT_GAP,
                offset,
                &mut shapes,
            );
            painter.extend(shapes);
        };

        for outline in selection {
            // Outlines traced from big selections have a point every pixel
            // or so, far more than show when zoomed out.
            let mut points: Vec<Pos2> = Vec::with_capacity(outline.len());
            for p in outline.iter().map(|&p| self.to_screen(p)) {
                if points.last().is_none_or(|last| last.distance_sq(p) >= 0.25) {
                    points.push(p);
                }
            }
            draw_ants(points, true);
        }
        if let Some(path) = drawing {
            draw_ants(
                path.iter().map(|p| self.to_screen((p.x, p.y))).collect(),
                false,
            );
        }

        if on_screen {
            ui.ctx().request_repaint_after(ANT_INTERVAL);
        }
    }

    fn tool_input(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        alt_samples: bool,
        samples: bool,
    ) -> Option<ToolInput> {
        let alt = ui.input(|i| i.modifiers.alt);
        // Alt+right-drag: left and right change the size, up and down the
        // hardness (down is harder), with the outline staying put.
        if alt && alt_samples && response.dragged_by(PointerButton::Secondary) {
            let origin = ui.input(|i| i.pointer.press_origin());
            self.resizing = self.resizing.or(origin);
            let d = response.drag_delta();
            return Some(ToolInput::BrushDrag {
                size: 2.0 * d.x * self.ppp / self.view.zoom,
                hardness: d.y / 200.0,
            });
        }
        self.resizing = None;
        let pointer = ui
            .input(|i| i.pointer.interact_pos())
            .map(|p| self.to_image(p));
        if (alt && alt_samples) || samples {
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
            self.drawn_cursor(ui, pointer, false);
        }
    }

    /// A crosshair, or with `arrows` the Move tool's four-way arrow, drawn
    /// in place of the system cursor. Hyprland hides the system cursor on
    /// any key press (`cursor:hide_on_key_press`), which would hide it
    /// whenever Shift or Alt is pressed to add, subtract or copy.
    fn drawn_cursor(&self, ui: &Ui, pointer: Pos2, arrows: bool) {
        let c = pointer;
        let (gap, arm) = if arrows { (0.0, 9.0) } else { (2.5, 8.0) };
        let mut lines = Vec::new();
        for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            let d = vec2(dx, dy);
            let end = c + d * arm;
            lines.push([c + d * gap, end]);
            if arrows {
                let side = vec2(-dy, dx) * 3.0;
                lines.push([end, end - d * 3.0 + side]);
                lines.push([end, end - d * 3.0 - side]);
            }
        }
        outlined_lines(&ui.painter_at(self.rect), &lines);
        ui.ctx().set_cursor_icon(CursorIcon::None);
    }

    /// Modifier badge (+, -, ×, copy) drawn near the cursor.
    fn cursor_badge(&self, ui: &Ui, pointer: Pos2, badge: CursorBadge) {
        let painter = ui.painter_at(self.rect);
        let c = pointer + vec2(10.0, 10.0);
        let draw_lines = |segments: &[[Pos2; 2]]| outlined_lines(&painter, segments);
        match badge {
            CursorBadge::Add => draw_lines(&[
                [pos2(c.x, c.y - 3.5), pos2(c.x, c.y + 3.5)],
                [pos2(c.x - 3.5, c.y), pos2(c.x + 3.5, c.y)],
            ]),
            CursorBadge::Subtract => draw_lines(&[[pos2(c.x - 3.5, c.y), pos2(c.x + 3.5, c.y)]]),
            CursorBadge::Intersect => draw_lines(&[
                [pos2(c.x - 2.5, c.y - 2.5), pos2(c.x + 2.5, c.y + 2.5)],
                [pos2(c.x - 2.5, c.y + 2.5), pos2(c.x + 2.5, c.y - 2.5)],
            ]),
            // A pipette: a slanted tube with a bulb at its top.
            CursorBadge::Eyedropper => draw_lines(&[
                [pos2(c.x - 4.0, c.y + 4.0), pos2(c.x + 2.0, c.y - 2.0)],
                [pos2(c.x + 0.5, c.y - 3.5), pos2(c.x + 3.5, c.y - 0.5)],
                [pos2(c.x + 2.0, c.y - 2.0), pos2(c.x + 4.0, c.y - 4.0)],
            ]),
            CursorBadge::Copy => {
                let back = Rect::from_min_size(c - vec2(3.5, 3.5), vec2(5.0, 5.0));
                let front = Rect::from_min_size(c - vec2(1.0, 1.0), vec2(5.0, 5.0));
                for (width, colour) in
                    [(2.5, Color32::from_black_alpha(180)), (1.0, Color32::WHITE)]
                {
                    painter.rect_stroke(back, 0.0, Stroke::new(width, colour), StrokeKind::Middle);
                }
                painter.rect_filled(front, 0.0, Color32::from_black_alpha(180));
                for (width, colour) in
                    [(2.5, Color32::from_black_alpha(180)), (1.0, Color32::WHITE)]
                {
                    painter.rect_stroke(front, 0.0, Stroke::new(width, colour), StrokeKind::Middle);
                }
            }
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

        let target = self.target_level();
        // A live move is drawn instead of the tiles, which are still kept
        // up to date underneath, for when it ends.
        let live = self.live.as_ref().filter(|f| f.stack.level == target).map(|frame| {
            let k = (1u32 << frame.stack.level) as f32;
            let lut = self.display_lut.get_or_init(|| Arc::new(crate::gpu::display_lut(&self.transform)));
            let (dx, dy) = frame.offset;
            LiveDraw {
                frame: LiveFrame {
                    offset: ((dx as f32 / k).round() as i32, (dy as f32 / k).round() as i32),
                    transform: frame.transform.map(|t| t.in_units(k.into())),
                    ..frame.clone()
                },
                display_lut: Arc::clone(lut),
                origin: [origin.x * ppp, origin.y * ppp],
                scale: self.view.zoom * k,
            }
        });
        let showing_live = live.is_some();
        if let Some(live) = live {
            painter.add(live.callback(canvas));
        }
        let Some(render) = self.render.clone() else {
            return;
        };
        let levels = self.levels.len();
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
                        if !showing_live {
                            painter.image(t.texture.id(), screen, uv, Color32::WHITE);
                        }
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

        self.fresh = wanted.is_empty();
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

/// Turn a polyline into dashed line segments with a phase offset.
///
/// Segments wrap seamlessly around corners, and `offset` shifts the dashes
/// along the polyline.
fn dashed_path(
    path: &[Pos2],
    stroke: Stroke,
    dash: f32,
    gap: f32,
    offset: f32,
    shapes: &mut Vec<egui::Shape>,
) {
    if path.len() < 2 {
        return;
    }
    let period = dash + gap;
    if period <= 0.0 {
        return;
    }
    let rem = (-offset).rem_euclid(period);
    let mut drawing_dash = rem < dash;
    let mut dist_left = if drawing_dash { dash - rem } else { period - rem };

    for w in path.windows(2) {
        let (start, end) = (w[0], w[1]);
        let vector = end - start;
        let seg_len = vector.length();
        if seg_len <= 0.0001 {
            continue;
        }
        let dir = vector / seg_len;
        let mut pos = 0.0;

        while pos < seg_len {
            let next_pos = (pos + dist_left).min(seg_len);
            if drawing_dash {
                shapes.push(egui::Shape::line_segment(
                    [start + dir * pos, start + dir * next_pos],
                    stroke,
                ));
            }
            let consumed = next_pos - pos;
            dist_left -= consumed;
            pos = next_pos;

            if dist_left <= 0.0001 {
                drawing_dash = !drawing_dash;
                dist_left = if drawing_dash { dash } else { gap };
            }
        }
    }
}

/// White lines with a dark outline, visible on any image.
fn outlined_lines(painter: &egui::Painter, segments: &[[Pos2; 2]]) {
    for (width, colour) in [(3.0, Color32::from_black_alpha(180)), (1.0, Color32::WHITE)] {
        let stroke = Stroke::new(width, colour);
        for &seg in segments {
            painter.line_segment(seg, stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment_points(shape: &egui::Shape) -> [Pos2; 2] {
        match shape {
            egui::Shape::LineSegment { points, .. } => *points,
            _ => panic!("expected LineSegment"),
        }
    }

    #[test]
    fn cancelled_writes_change_nothing() {
        let render = Render::new(Raster::new(300, 300, vec![[0; 4]; 300 * 300]));
        let tile = vec![vec![[9; 4]; omapix_engine::tiled::TILE_PIXELS]];
        let cancel = AtomicBool::new(true);
        assert!(!render.write_tiles(0, &[(1, 1)], &tile, Some(&cancel)));
        assert_eq!(render.sample_for_test(299, 299), [0; 4]);
        cancel.store(false, Ordering::Release);
        assert!(render.write_tiles(0, &[(1, 1)], &tile, Some(&cancel)));
        assert_eq!(render.sample_for_test(299, 299), [9; 4]);
        assert_eq!(render.sample_for_test(255, 255), [0; 4]);
        // The pyramid follows.
        assert_eq!(render.level_for_test(1).get(149, 149), [9; 4]);
    }

    #[test]
    fn dashed_path_creates_alternating_segments() {
        let path = [pos2(0.0, 0.0), pos2(20.0, 0.0)];
        let mut shapes = Vec::new();
        dashed_path(
            &path,
            Stroke::new(1.0, Color32::WHITE),
            4.0,
            4.0,
            0.0,
            &mut shapes,
        );

        assert_eq!(shapes.len(), 3);
        assert_eq!(segment_points(&shapes[0]), [pos2(0.0, 0.0), pos2(4.0, 0.0)]);
        assert_eq!(segment_points(&shapes[1]), [pos2(8.0, 0.0), pos2(12.0, 0.0)]);
        assert_eq!(segment_points(&shapes[2]), [pos2(16.0, 0.0), pos2(20.0, 0.0)]);
    }

    #[test]
    fn dashed_path_wraps_around_corners() {
        let path = [pos2(0.0, 0.0), pos2(6.0, 0.0), pos2(6.0, 6.0)];
        let mut shapes = Vec::new();
        dashed_path(
            &path,
            Stroke::new(1.0, Color32::WHITE),
            4.0,
            4.0,
            0.0,
            &mut shapes,
        );

        assert_eq!(shapes.len(), 2);
        assert_eq!(segment_points(&shapes[0]), [pos2(0.0, 0.0), pos2(4.0, 0.0)]);
        // The 4px gap goes from (4,0) to (6,0) [2px], then (6,0) to (6,2) [2px].
        // Next 4px dash is from (6,2) to (6,6).
        assert_eq!(segment_points(&shapes[1]), [pos2(6.0, 2.0), pos2(6.0, 6.0)]);
    }

    #[test]
    fn dashed_path_offset_shifts_segments_forward() {
        let path = [pos2(0.0, 0.0), pos2(20.0, 0.0)];
        let mut shapes = Vec::new();
        dashed_path(
            &path,
            Stroke::new(1.0, Color32::WHITE),
            4.0,
            4.0,
            1.0,
            &mut shapes,
        );

        assert_eq!(shapes.len(), 3);
        // Offset 1.0 shifts dashes forward along the path by 1.0 point.
        assert_eq!(segment_points(&shapes[0]), [pos2(1.0, 0.0), pos2(5.0, 0.0)]);
        assert_eq!(segment_points(&shapes[1]), [pos2(9.0, 0.0), pos2(13.0, 0.0)]);
        assert_eq!(segment_points(&shapes[2]), [pos2(17.0, 0.0), pos2(20.0, 0.0)]);
    }

    #[test]
    fn dashed_path_closed_loop_length_invariant() {
        // A closed loop with perimeter 160 (multiple of 8 = 4 dash + 4 gap).
        let path = [
            pos2(0.0, 0.0),
            pos2(40.0, 0.0),
            pos2(40.0, 40.0),
            pos2(0.0, 40.0),
            pos2(0.0, 0.0),
        ];

        for i in 0..80 {
            let offset = i as f32 * 0.1;
            let mut shapes = Vec::new();
            dashed_path(
                &path,
                Stroke::new(1.0, Color32::WHITE),
                4.0,
                4.0,
                offset,
                &mut shapes,
            );

            let total_dash_len: f32 = shapes
                .iter()
                .map(|s| {
                    let pts = segment_points(s);
                    (pts[1] - pts[0]).length()
                })
                .sum();

            assert!(
                (total_dash_len - 80.0).abs() < 1e-3,
                "offset {offset} produced total dash length {total_dash_len}, expected 80.0"
            );
        }
    }
}
