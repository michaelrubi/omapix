//! Brush strokes, painted the way Photoshop's Brush tool paints.
//!
//! A stroke lays dabs along the pointer's path: round ones a tenth of
//! their diameter apart, unless the Brush Settings ([`crate::dynamics`])
//! say otherwise. Dabs build up a per-stroke coverage buffer: each dab adds `flow` of its shape, and the
//! finished coverage is applied at `opacity`. So a single stroke never
//! exceeds its opacity however often it crosses itself, while low flow
//! builds up gradually, exactly like Photoshop's opacity/flow pair.
//!
//! The stroke keeps the untouched original of whatever it paints on and
//! recomputes touched tiles from it, so the result never depends on how
//! many dabs landed on a pixel beyond what coverage records.
//!
//! Clone and heal strokes paint pixels copied from a sampling image at a
//! fixed offset. Healing then corrects the copied patch's tone and colour
//! to match its new surroundings when the stroke ends (see [`Stroke::finish`]).

use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;

use crate::Pixel;
use crate::dynamics::{Dab, Dynamics, Step, grain, mix};
use crate::tiled::{TILE, TILE_PIXELS, Tiled};
use crate::tip::Tip;
use crate::toning::{self, ToneRange};

const MAX: f32 = u16::MAX as f32;
/// Added to both sides of a heal's ratios, so noise in near-black doesn't
/// count as a pixel being several times lighter than another.
const DARK: f32 = 0.01;
/// A dab over at least this many pixels is laid a tile to a core: below
/// it, starting the others up costs more than it saves.
const PARALLEL: usize = 300 * 300;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BrushSettings {
    /// Diameter in image pixels.
    pub size: f32,
    /// 0 = soft edge fading from the centre, 1 = hard edge.
    pub hardness: f32,
    /// Maximum coverage of one stroke, 0–1.
    pub opacity: f32,
    /// Coverage each dab adds, 0–1.
    pub flow: f32,
    /// A pen's pressure sets the size of each dab (Photoshop's pressure
    /// button beside Size).
    pub size_pressure: bool,
    /// A pen's pressure sets how far each dab's coverage can build up
    /// (beside Opacity).
    pub opacity_pressure: bool,
    /// A pen's pressure scales each dab's flow, so light strokes build up
    /// slowly (Photoshop's Transfer › Flow Jitter › Pen Pressure).
    pub flow_pressure: bool,
    /// The tip's shape and how it varies (Photoshop's Brush Settings).
    pub dynamics: Dynamics,
    /// The sampled tip it stamps ([`Tip::id`]), or none for a round one.
    /// The stroke is given the tip itself ([`Stroke::with_tip`]).
    pub tip: Option<u64>,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            size: 100.0,
            hardness: 0.0,
            opacity: 1.0,
            flow: 1.0,
            size_pressure: true,
            opacity_pressure: true,
            flow_pressure: false,
            dynamics: Dynamics::default(),
            tip: None,
        }
    }
}

/// What a stroke does to the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    /// Paint a colour (document colour space, alpha ignored) onto pixels.
    Color(Pixel),
    /// Erase pixels to transparency.
    Erase,
    /// Paint a grey level onto a layer mask (0 = hide, 65535 = reveal).
    Mask(u16),
    /// Copy pixels from the sampling image, `dx, dy` pixels away.
    Clone { dx: i32, dy: i32 },
    /// Like `Clone`, then blend the copy's tone into its surroundings.
    Heal { dx: i32, dy: i32 },
    /// Heal without a source: when the stroke ends, pick the best-matching
    /// nearby patch automatically (Photoshop's Spot Healing Brush). While
    /// painting, the stroke shows as a translucent dark overlay.
    SpotHeal,
    /// Dodge (or with `burn`, Burn) the tones in `range`, Protect Tones
    /// keeping the colours' hue (see [`crate::toning`]). The stroke's
    /// opacity is the tool's Exposure. On a mask it lightens or darkens the
    /// mask's greys.
    Tone { range: ToneRange, burn: bool, protect: bool },
    /// Saturate or desaturate (Photoshop's Sponge). The stroke's opacity is
    /// its Flow. Masks are left alone.
    Sponge { saturate: bool, vibrance: bool },
}

/// The surface a stroke paints on.
#[derive(Clone)]
pub enum Surface {
    Pixels(Tiled<Pixel>),
    Mask(Tiled<u16>),
}

impl Surface {
    fn size(&self) -> (u32, u32) {
        match self {
            Surface::Pixels(t) => (t.width(), t.height()),
            Surface::Mask(t) => (t.width(), t.height()),
        }
    }
}

pub struct Stroke {
    settings: BrushSettings,
    paint: Paint,
    original: Surface,
    /// The image clone and heal strokes copy from.
    source: Option<Tiled<Pixel>>,
    /// Selection coverage limiting where the stroke has effect.
    limit: Option<Tiled<u16>>,
    /// Keep each pixel's alpha (Lock Transparent Pixels).
    keep_alpha: bool,
    /// The picture each dab stamps, in place of a round one.
    tip: Option<Arc<Tip>>,
    /// Coverage of touched tiles, 0–1 per pixel.
    coverage: HashMap<(u32, u32), Vec<f32>>,
    /// The last point, and the pen's pressure there.
    last: Option<(f32, f32, f32)>,
    /// Distance travelled since the last dab.
    carried: f32,
    /// What the dabs' random variation comes from: where the stroke began.
    seed: u64,
    /// Steps and dabs laid so far.
    steps: u64,
    dabs: u64,
    /// The first point is waiting for the stroke's direction, which its
    /// dabs depend on.
    waiting: bool,
}

impl Stroke {
    pub fn new(settings: BrushSettings, paint: Paint, original: Surface) -> Self {
        Self {
            settings,
            paint,
            original,
            source: None,
            limit: None,
            keep_alpha: false,
            tip: None,
            coverage: HashMap::new(),
            last: None,
            carried: 0.0,
            seed: 0,
            steps: 0,
            dabs: 0,
            waiting: false,
        }
    }

    /// Only paint where `selection` covers (0–65535 per pixel).
    pub fn within(mut self, selection: Tiled<u16>) -> Self {
        self.limit = Some(selection);
        self
    }

    /// Stamp `tip` at each dab: its longer side is the brush's size, and
    /// hardness doesn't apply.
    pub fn with_tip(mut self, tip: Arc<Tip>) -> Self {
        self.tip = Some(tip);
        self
    }

    /// Change only colour, keeping each pixel's alpha, as painting on a
    /// layer with Lock Transparent Pixels does. Erasing then does nothing.
    pub fn keeping_alpha(mut self) -> Self {
        self.keep_alpha = true;
        self
    }

    /// Paint onto `base` (see [`paint_pixel`]). With alpha kept, the colour
    /// changes as if the pixel were opaque, as in Photoshop.
    fn paint_onto(&self, base: Pixel, a: f32, paint: Paint) -> Pixel {
        if !self.keep_alpha {
            return paint_pixel(base, a, paint);
        }
        let mut out = paint_pixel([base[0], base[1], base[2], u16::MAX], a, paint);
        out[3] = base[3];
        out
    }

    /// How much paint a pixel gets where the stroke covers `c` of it. With
    /// Wet Edges that's half where the stroke is solid and more towards its
    /// edge, where coverage runs out, so that paint gathers along the edge
    /// as watercolour does and a stroke crossing itself gets no darker.
    #[inline]
    fn laid(&self, c: f32) -> f32 {
        if !self.settings.dynamics.wet_edges {
            return c;
        }
        let t = ((c - 0.6) / 0.4).clamp(0.0, 1.0);
        c * (1.0 - 0.5 * t * t * (3.0 - 2.0 * t))
    }

    /// Selection coverage (0–1) of pixel `i` in tile (col, row).
    #[inline]
    fn limit_at(&self, col: u32, row: u32, i: usize) -> f32 {
        match &self.limit {
            None => 1.0,
            Some(l) => f32::from(l.tile(col, row).map_or(l.fill(), |t| t[i])) / MAX,
        }
    }

    /// Set the image clone and heal strokes copy from.
    pub fn sampling(mut self, source: Tiled<Pixel>) -> Self {
        self.source = Some(source);
        self
    }

    /// The pixel a clone or heal stroke copies to (x, y), or transparent if
    /// the source point falls outside the image.
    fn copied(&self, x: u32, y: u32) -> Pixel {
        let (Paint::Clone { dx, dy } | Paint::Heal { dx, dy }) = self.paint else {
            return [0; 4];
        };
        let Some(src) = &self.source else {
            return [0; 4];
        };
        let (sx, sy) = (i64::from(x) + i64::from(dx), i64::from(y) + i64::from(dy));
        if sx < 0 || sy < 0 || sx >= i64::from(src.width()) || sy >= i64::from(src.height()) {
            return [0; 4];
        }
        src.get(sx as u32, sy as u32)
    }

    /// The distance between dabs at `pressure`.
    fn spacing(&self, pressure: f32) -> f32 {
        self.settings.dynamics.step(self.diameter(pressure))
    }

    fn diameter(&self, pressure: f32) -> f32 {
        if self.settings.size_pressure {
            let least = self.settings.dynamics.minimum_diameter.clamp(0.0, 1.0);
            self.settings.size * (least + (1.0 - least) * pressure)
        } else {
            self.settings.size
        }
    }

    /// Continue the stroke to (x, y) in image pixels, with a pen's
    /// `pressure` there (0–1; 1 for a mouse), laying evenly spaced dabs
    /// along the way. Returns the tiles whose coverage changed.
    pub fn add_point(&mut self, x: f32, y: f32, pressure: f32) -> Vec<(u32, u32)> {
        let mut touched = Vec::new();
        let pressure = pressure.clamp(0.0, 1.0);
        match self.last {
            None => {
                self.seed = mix(u64::from(x.to_bits()) << 32 | u64::from(y.to_bits()));
                self.waiting = self.settings.dynamics.follows_direction();
                if !self.waiting {
                    self.step(x, y, pressure, 0.0, &mut touched);
                }
            }
            Some((lx, ly, lp)) => {
                let (dx, dy) = (x - lx, y - ly);
                let length = (dx * dx + dy * dy).sqrt();
                // Anticlockwise from east; y runs down the image.
                let direction = (-dy).atan2(dx);
                if self.waiting && length > 0.0 {
                    self.waiting = false;
                    self.step(lx, ly, lp, direction, &mut touched);
                }
                let mut t = self.spacing(lp) - self.carried;
                let mut spaced = self.spacing(lp);
                while t <= length {
                    let f = t / length;
                    let p = lp + (pressure - lp) * f;
                    self.step(lx + dx * f, ly + dy * f, p, direction, &mut touched);
                    spaced = self.spacing(p);
                    t += spaced;
                }
                self.carried = length - (t - spaced);
            }
        }
        self.last = Some((x, y, pressure));
        touched.sort_unstable();
        touched.dedup();
        touched
    }

    /// Lay the dabs of one step along the stroke, at `pressure` and going
    /// in `direction`.
    fn step(&mut self, x: f32, y: f32, pressure: f32, direction: f32, touched: &mut Vec<(u32, u32)>) {
        let at = Step {
            x,
            y,
            diameter: self.diameter(pressure),
            // Coverage builds up to this.
            most: if self.settings.opacity_pressure { pressure } else { 1.0 },
            flow: self.settings.flow * if self.settings.flow_pressure { pressure } else { 1.0 },
            direction,
        };
        let dabs = self.settings.dynamics.dabs(self.seed, self.steps, &mut self.dabs, &at);
        self.steps += 1;
        for dab in &dabs {
            self.dab(dab, touched);
        }
    }

    fn dab(&mut self, dab: &Dab, touched: &mut Vec<(u32, u32)>) {
        let (w, h) = self.original.size();
        let Dab { x: cx, y: cy, radius: r, roundness, most, flow, .. } = *dab;
        let hardness = self.settings.hardness.clamp(0.0, 0.999);
        let (sin, cos) = dab.angle.sin_cos();
        let noise = self.settings.dynamics.noise;
        // A mirrored tip is read backwards.
        let (flip_x, flip_y) = (if dab.flip_x { -1.0 } else { 1.0 }, if dab.flip_y { -1.0 } else { 1.0 });
        // A sampled tip: which of its sizes to read, and how many of its
        // pixels one of the image's is.
        let tip = self.tip.as_deref().map(|tip| {
            let (tw, th) = tip.size();
            let (tw, th, longer) = (tw as f32, th as f32, tw.max(th) as f32);
            (tip, tip.level_for(2.0 * r * roundness.max(0.25)), longer / (2.0 * r), tw, th)
        });
        // How far the dab reaches each way: a round one its radius, a
        // sampled one half its width along its angle and half its height
        // across (and the half pixel of its own it fades out over), which
        // for a tall or wide tip is far less than a square round its
        // longer side.
        let (reach_x, reach_y) = tip.map_or((r, r), |(_, _, scale, tw, th)| {
            let (along, across) = ((tw / 2.0 + 0.5) / scale, (th / 2.0 + 0.5) / scale * roundness);
            (along * cos.abs() + across * sin.abs(), along * sin.abs() + across * cos.abs())
        });
        let x0 = (cx - reach_x).floor().max(0.0) as u32;
        let y0 = (cy - reach_y).floor().max(0.0) as u32;
        let x1 = ((cx + reach_x).ceil() as u32).min(w);
        let y1 = ((cy + reach_y).ceil() as u32).min(h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        // Add the dab to one tile's coverage. Whether any of it fell there.
        let lay = |(col, row): (u32, u32), cov: &mut [f32]| {
            let (tx, ty) = (col * TILE, row * TILE);
            let mut hit = false;
            for py in y0.max(ty)..y1.min(ty + TILE) {
                for px in x0.max(tx)..x1.min(tx + TILE) {
                    // The pixel's centre from the dab's, and how much of
                    // the dab is there.
                    let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                    let mut shape = if let Some((tip, level, scale, tw, th)) = tip {
                        // Along the tip and across it, in its own pixels.
                        let (along, across) = ((dx * cos - dy * sin) * flip_x, (dx * sin + dy * cos) / roundness * flip_y);
                        tip.sample(level, 0.5 + along * scale / tw, 0.5 + across * scale / th)
                    } else if roundness >= 1.0 {
                        falloff((dx.powi(2) + dy.powi(2)).sqrt() / r.max(0.5), hardness)
                    } else {
                        // Along the tip, and across it, where it's narrower.
                        falloff((dx * cos - dy * sin).hypot((dx * sin + dy * cos) / roundness) / r.max(0.5), hardness)
                    };
                    if shape <= 0.0 {
                        continue;
                    }
                    if noise && shape < 1.0 {
                        // Most of the way to all or nothing, as the
                        // pixel's grain falls.
                        let all = if grain(px, py) < shape { 1.0 } else { 0.0 };
                        shape += (all - shape) * 0.7;
                    }
                    let c = &mut cov[((py - ty) * TILE + (px - tx)) as usize];
                    *c += (most - *c).max(0.0) * flow * shape;
                    hit = true;
                }
            }
            hit
        };
        let tiles = (y0 / TILE..=(y1 - 1) / TILE).flat_map(|row| (x0 / TILE..=(x1 - 1) / TILE).map(move |col| (col, row)));
        if (x1 - x0) as usize * (y1 - y0) as usize >= PARALLEL {
            // A big dab: each tile on its own core. Their coverage is
            // taken out of the stroke for the while.
            let mut taken: Vec<_> = tiles.map(|at| (at, self.coverage.remove(&at).unwrap_or_else(|| vec![0.0; TILE_PIXELS]))).collect();
            let hits: Vec<bool> = taken.par_iter_mut().map(|(at, cov)| lay(*at, cov)).collect();
            for ((at, cov), hit) in taken.into_iter().zip(hits) {
                self.coverage.insert(at, cov);
                if hit {
                    touched.push(at);
                }
            }
        } else {
            for at in tiles {
                if lay(at, self.coverage.entry(at).or_insert_with(|| vec![0.0; TILE_PIXELS])) {
                    touched.push(at);
                }
            }
        }
    }

    /// Write the stroke's current result for the given tiles into `surface`.
    pub fn apply(&self, surface: &mut Surface, tiles: &[(u32, u32)]) {
        let opacity = self.settings.opacity;
        // Each tile is worked out on its own, then put in place.
        match (surface, &self.original) {
            (Surface::Pixels(dst), Surface::Pixels(orig)) => {
                let copying = matches!(self.paint, Paint::Clone { .. } | Paint::Heal { .. });
                let (w, h) = (orig.width(), orig.height());
                let painted = |&(col, row): &(u32, u32)| {
                    let cov = self.coverage.get(&(col, row))?;
                    let mut out: Vec<Pixel> = orig
                        .tile(col, row)
                        .map_or_else(|| vec![orig.fill(); TILE_PIXELS], <[Pixel]>::to_vec);
                    let (tx, ty) = (col * TILE, row * TILE);
                    for i in 0..TILE_PIXELS {
                        let a = self.laid(cov[i]) * opacity * self.limit_at(col, row, i);
                        out[i] = if self.paint == Paint::SpotHeal {
                            // Show where the stroke is until it heals on release.
                            self.paint_onto(out[i], a * 0.35, Paint::Color([0, 0, 0, u16::MAX]))
                        } else if copying {
                            let (x, y) = (tx + i as u32 % TILE, ty + i as u32 / TILE);
                            if a <= 0.0 || x >= w || y >= h {
                                out[i]
                            } else {
                                self.paint_onto(out[i], a, Paint::Color(self.copied(x, y)))
                            }
                        } else {
                            self.paint_onto(out[i], a, self.paint)
                        };
                    }
                    Some(((col, row), out))
                };
                for ((col, row), out) in each(tiles, painted) {
                    dst.set_tile(col, row, out);
                }
            }
            (Surface::Mask(dst), Surface::Mask(orig)) => {
                let target = match self.paint {
                    Paint::Mask(v) => f32::from(v),
                    Paint::Color(c) => f32::from(c[1]),
                    // Cloning isn't offered on masks; leave them alone.
                    Paint::Erase
                    | Paint::Clone { .. }
                    | Paint::Heal { .. }
                    | Paint::SpotHeal => MAX,
                    // Worked out for each pixel below.
                    Paint::Tone { .. } | Paint::Sponge { .. } => 0.0,
                };
                let painted = |&(col, row): &(u32, u32)| {
                    let cov = self.coverage.get(&(col, row))?;
                    let mut out: Vec<u16> = orig
                        .tile(col, row)
                        .map_or_else(|| vec![orig.fill(); TILE_PIXELS], <[u16]>::to_vec);
                    for i in 0..TILE_PIXELS {
                        let a = self.laid(cov[i]) * opacity * self.limit_at(col, row, i);
                        let v = f32::from(out[i]);
                        out[i] = match self.paint {
                            Paint::Tone { range, burn, .. } => {
                                (toning::tone_curve(v / MAX, a, range, burn) * MAX).round() as u16
                            }
                            Paint::Sponge { .. } => out[i],
                            _ => (v + (target - v) * a).round() as u16,
                        };
                    }
                    Some(((col, row), out))
                };
                for ((col, row), out) in each(tiles, painted) {
                    dst.set_tile(col, row, out);
                }
            }
            _ => {}
        }
    }
}

impl Stroke {
    /// Complete the stroke. For healing, this replaces the copied patch with
    /// a healed one and returns the tiles that changed; other strokes are
    /// already final.
    ///
    /// Healing keeps the copy's texture but takes its tone and colour from
    /// round the painted area: the copy is lightened or darkened by however
    /// much the destination differs from it there, carried smoothly across
    /// the painted area ([`crate::poisson`]), so its edge meets what's round
    /// it with no step. This is Photoshop's healing brush (Poisson
    /// blending), by ratio, as light does. What's outside a selection is
    /// left out of it, so a selection drawn along an edge keeps that edge's
    /// colour from bleeding in.
    pub fn finish(&mut self, surface: &mut Surface) -> Vec<(u32, u32)> {
        // A click that never moved: the dab that was waiting to know which
        // way the stroke went.
        let mut waited = Vec::new();
        if let (true, Some((x, y, pressure))) = (self.waiting, self.last) {
            self.waiting = false;
            self.step(x, y, pressure, 0.0, &mut waited);
            waited.sort_unstable();
            waited.dedup();
            self.apply(surface, &waited);
        }
        if self.paint == Paint::SpotHeal {
            let Some((dx, dy)) = self.find_source() else {
                // Nowhere to copy from: undo the overlay.
                let tiles: Vec<(u32, u32)> = self.coverage.keys().copied().collect();
                if let (Surface::Pixels(dst), Surface::Pixels(orig)) = (surface, &self.original) {
                    for &(col, row) in &tiles {
                        let base = orig
                            .tile(col, row)
                            .map_or_else(|| vec![orig.fill(); TILE_PIXELS], <[Pixel]>::to_vec);
                        dst.tile_mut(col, row).copy_from_slice(&base);
                    }
                }
                return tiles;
            };
            self.paint = Paint::Heal { dx, dy };
        }
        let Paint::Heal { .. } = self.paint else {
            return waited;
        };
        let (Some(src), Surface::Pixels(orig), Surface::Pixels(dst)) =
            (&self.source, &self.original, surface)
        else {
            return waited;
        };
        let tiles: Vec<(u32, u32)> = self.coverage.keys().copied().collect();
        if tiles.is_empty() {
            return tiles;
        }
        let (w, h) = (orig.width(), orig.height());
        // The painted pixels and a ring of pixels round them.
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for (&(col, row), cov) in &self.coverage {
            for (i, _) in cov.iter().enumerate().filter(|(_, c)| **c > 0.0) {
                let (x, y) = (col * TILE + i as u32 % TILE, row * TILE + i as u32 / TILE);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
        if x0 >= x1 {
            return tiles;
        }
        let (x0, y0, x1, y1) = (x0.saturating_sub(1), y0.saturating_sub(1), (x1 + 1).min(w), (y1 + 1).min(h));
        let (rw, rh) = ((x1 - x0) as usize, (y1 - y0) as usize);

        // How much lighter the destination is than the copy (as a
        // logarithm), known in the ring and worked out for the rest: the
        // painted pixels, and those with nothing to go by.
        let mut unknown = Vec::with_capacity(rw * rh);
        let mut gain = Vec::with_capacity(rw * rh);
        for y in y0..y1 {
            for x in x0..x1 {
                let (col, row, i) = (x / TILE, y / TILE, ((y % TILE) * TILE + x % TILE) as usize);
                let painted = self.coverage.get(&(col, row)).is_some_and(|c| c[i] > 0.0);
                let (d, c) = (src.get(x, y), self.copied(x, y));
                unknown.push(painted || d[3] == 0 || c[3] == 0 || self.limit_at(col, row, i) <= 0.0);
                let (d, c) = (rgb(d), rgb(c));
                gain.push([0, 1, 2].map(|ch| ((d[ch] + DARK) / (c[ch] + DARK)).ln()));
            }
        }
        let gain = crate::poisson::membrane_fill(rw, rh, &gain, &unknown);

        let opacity = self.settings.opacity;
        for &(col, row) in &tiles {
            let cov = &self.coverage[&(col, row)];
            let base: Vec<Pixel> = orig
                .tile(col, row)
                .map_or_else(|| vec![orig.fill(); TILE_PIXELS], <[Pixel]>::to_vec);
            let out = dst.tile_mut(col, row);
            for i in 0..TILE_PIXELS {
                let a = self.laid(cov[i]) * opacity * self.limit_at(col, row, i);
                let (x, y) = (col * TILE + i as u32 % TILE, row * TILE + i as u32 / TILE);
                if a <= 0.0 || x >= w || y >= h {
                    out[i] = base[i];
                    continue;
                }
                let copied = self.copied(x, y);
                if copied[3] == 0 {
                    out[i] = base[i];
                    continue;
                }
                let r = ((y - y0) as usize) * rw + (x - x0) as usize;
                let mut healed = copied;
                for ch in 0..3 {
                    // As light does: a copy from shadow brightened keeps
                    // its texture in proportion.
                    let v = (f32::from(copied[ch]) / MAX + DARK) * gain[r][ch].exp().clamp(0.25, 4.0) - DARK;
                    healed[ch] = (v.clamp(0.0, 1.0) * MAX).round() as u16;
                }
                out[i] = self.paint_onto(base[i], a, Paint::Color(healed));
            }
        }
        tiles
    }
}

impl Stroke {
    /// Choose where a spot-heal stroke copies from: the nearby patch whose
    /// surroundings are shaped most like the painted area's, whose texture
    /// is as fine or coarse as the skin round it, which is even (no spot,
    /// crease, hair or lip edge in it), and which is like its own
    /// surroundings, since healing takes the difference in shade between
    /// the two surroundings and applies it to the patch.
    fn find_source(&self) -> Option<(i32, i32)> {
        let src = self.source.as_ref()?;
        let (w, h) = (src.width() as i64, src.height() as i64);
        // Exact bounds of the painted pixels.
        let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        for (&(col, row), cov) in &self.coverage {
            for (i, &c) in cov.iter().enumerate() {
                if c > 0.01 {
                    let (x, y) = (
                        i64::from(col * TILE) + (i as i64 % 256),
                        i64::from(row * TILE) + (i as i64 / 256),
                    );
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x0 >= x1 {
            return None;
        }
        let margin = ((self.settings.size * 0.5) as i64).max(4);
        let (rx0, ry0) = ((x0 - margin).max(0), (y0 - margin).max(0));
        let (rx1, ry1) = ((x1 + margin).min(w), (y1 + margin).min(h));
        let extent = (x1 - x0).max(y1 - y0) as f32;

        let cov_at = |x: i64, y: i64| -> f32 {
            let (x, y) = (x as u32, y as u32);
            self.coverage
                .get(&(x / TILE, y / TILE))
                .map_or(0.0, |c| c[((y % TILE) * TILE + x % TILE) as usize])
        };
        // The area cut into blocks, each summed up from a few pixels by its
        // average colour and its texture (how much nearby pixels differ),
        // and whether it's painted.
        let block = ((rx1 - rx0).max(ry1 - ry0) / 12).max(2);
        let taps = block.min(4);
        // Texture is measured between pixels this far apart: pore-sized
        // rather than noise.
        let apart = (block / taps).max(1);
        // Whole blocks only, with room for the texture's second pixel.
        let blocks: Vec<(i64, i64)> = (ry0..ry1 - block - apart)
            .step_by(block as usize)
            .flat_map(|y| (rx0..rx1 - block - apart).step_by(block as usize).map(move |x| (x, y)))
            .collect();
        let sample = |bx: i64, by: i64, ox: i64, oy: i64| -> Option<([f32; 3], f32, bool)> {
            let (mut mean, mut texture, mut painted, mut n) = ([0.0f32; 3], 0.0f32, false, 0.0f32);
            for ty in 0..taps {
                for tx in 0..taps {
                    let (x, y) = (bx + tx * block / taps, by + ty * block / taps);
                    painted |= cov_at(x, y) > 0.01;
                    let at = |x: i64, y: i64| src.get((x + ox) as u32, (y + oy) as u32);
                    let p = at(x, y);
                    if p[3] == 0 {
                        return None;
                    }
                    let (p, right, down) = (rgb(p), rgb(at(x + apart, y)), rgb(at(x, y + apart)));
                    for c in 0..3 {
                        mean[c] += p[c];
                        texture += (p[c] - right[c]).powi(2) + (p[c] - down[c]).powi(2);
                    }
                    n += 1.0;
                }
            }
            Some((mean.map(|v| v / n), texture / n, painted))
        };
        let here = blocks.iter().map(|&(x, y)| sample(x, y, 0, 0)).collect::<Option<Vec<_>>>()?;
        // The texture of the skin round the painted area.
        let ring_texture = {
            let ring: Vec<f32> = here.iter().filter(|b| !b.2).map(|b| b.1).collect();
            ring.iter().sum::<f32>() / ring.len().max(1) as f32
        };

        let mut best: Option<(f32, (i32, i32))> = None;
        for ring in [1.0f32, 1.25, 1.6, 2.0, 2.6] {
            let dist = ring * extent;
            for k in 0..24 {
                let angle = k as f32 / 24.0 * std::f32::consts::TAU;
                let (ox, oy) = (
                    (angle.cos() * dist).round() as i64,
                    (angle.sin() * dist).round() as i64,
                );
                if rx0 + ox < 0 || ry0 + oy < 0 || rx1 + ox > w || ry1 + oy > h {
                    continue;
                }
                let there: Option<Vec<_>> = blocks.iter().map(|&(x, y)| sample(x, y, ox, oy)).collect();
                let Some(there) = there else { continue };
                // Surroundings: their difference, on average and in shape.
                let (mut shift, mut shift_sq, mut ring_n) = ([0.0f32; 3], 0.0f32, 0.0f32);
                let (mut source_ring, mut patch, mut patch_sq, mut texture, mut patch_n) =
                    ([0.0f32; 3], [0.0f32; 3], 0.0f32, 0.0f32, 0.0f32);
                for (d, s) in here.iter().zip(&there) {
                    if d.2 {
                        for (p, v) in patch.iter_mut().zip(s.0) {
                            *p += v;
                            patch_sq += v * v;
                        }
                        texture += s.1;
                        patch_n += 1.0;
                    } else {
                        for c in 0..3 {
                            shift[c] += d.0[c] - s.0[c];
                            shift_sq += (d.0[c] - s.0[c]).powi(2);
                            source_ring[c] += s.0[c];
                        }
                        ring_n += 1.0;
                    }
                }
                if ring_n == 0.0 || patch_n == 0.0 {
                    continue;
                }
                let shift_mean: f32 = shift.iter().map(|s| (s / ring_n).powi(2)).sum();
                // Shaped alike; a difference in shade counts for a quarter,
                // since healing corrects it, but skin shouldn't come from hair.
                let shape = shift_sq / ring_n - 0.75 * shift_mean;
                // As fine or coarse as the skin round the painted area.
                let grain = ((texture / patch_n).sqrt() - ring_texture.sqrt()).powi(2);
                // Even: no spot or crease in it.
                let lumps = patch_sq / patch_n - patch.iter().map(|p| (p / patch_n).powi(2)).sum::<f32>();
                // Like its own surroundings.
                let unlike: f32 = (0..3).map(|c| (patch[c] / patch_n - source_ring[c] / ring_n).powi(2)).sum();
                let score = shape + grain + lumps + unlike;
                if best.is_none_or(|(b, _)| score < b) {
                    best = Some((score, (ox as i32, oy as i32)));
                }
            }
        }
        best.map(|(_, offset)| offset)
    }
}

/// A pixel's colour, 0–1.
fn rgb(p: Pixel) -> [f32; 3] {
    [
        f32::from(p[0]) / MAX,
        f32::from(p[1]) / MAX,
        f32::from(p[2]) / MAX,
    ]
}

/// What `f` makes of each of `tiles`: a tile to a core when there are
/// enough of them to be worth starting the others up.
fn each<T: Send>(tiles: &[(u32, u32)], f: impl Fn(&(u32, u32)) -> Option<T> + Sync) -> Vec<T> {
    if tiles.len() < 4 { tiles.iter().filter_map(f).collect() } else { tiles.par_iter().filter_map(&f).collect() }
}

/// Dab shape at distance `d` (fraction of the radius) from the centre:
/// 1 inside the hard core, easing smoothly to 0 at the edge.
fn falloff(d: f32, hardness: f32) -> f32 {
    if d >= 1.0 {
        0.0
    } else if d <= hardness {
        1.0
    } else {
        let t = (1.0 - d) / (1.0 - hardness);
        t * t * (3.0 - 2.0 * t)
    }
}

fn paint_pixel(base: Pixel, a: f32, paint: Paint) -> Pixel {
    if a <= 0.0 {
        return base;
    }
    let base_a = f32::from(base[3]) / MAX;
    match paint {
        Paint::Color(c) => {
            // A colour's own alpha (from cloned pixels) scales coverage.
            let a = a * f32::from(c[3]) / MAX;
            if a <= 0.0 {
                return base;
            }
            let out_a = a + base_a * (1.0 - a);
            let mut out = [0u16; 4];
            for ch in 0..3 {
                let v = (f32::from(c[ch]) * a + f32::from(base[ch]) * base_a * (1.0 - a)) / out_a;
                out[ch] = v.round().clamp(0.0, MAX) as u16;
            }
            out[3] = (out_a * MAX).round() as u16;
            out
        }
        Paint::Erase => {
            let mut out = base;
            out[3] = (base_a * (1.0 - a) * MAX).round() as u16;
            out
        }
        Paint::Tone { range, burn, protect } => {
            toning::on_pixel(base, |c| toning::dodge_burn(c, a, range, burn, protect))
        }
        Paint::Sponge { saturate, vibrance } => {
            toning::on_pixel(base, |c| toning::sponge(c, a, saturate, vibrance))
        }
        Paint::Mask(_) | Paint::Clone { .. } | Paint::Heal { .. } | Paint::SpotHeal => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(w: u32, h: u32) -> Tiled<Pixel> {
        Tiled::from_slice(w, h, [0; 4], &vec![[65535; 4]; (w * h) as usize])
    }

    fn stroke(
        settings: BrushSettings,
        paint: Paint,
        surface: &Surface,
        points: &[(f32, f32)],
    ) -> Surface {
        let mut s = Stroke::new(settings, paint, surface.clone());
        let mut out = surface.clone();
        for &(x, y) in points {
            let tiles = s.add_point(x, y, 1.0);
            s.apply(&mut out, &tiles);
        }
        out
    }

    #[test]
    fn hard_brush_paints_solid_centre_and_nothing_outside() {
        let surface = Surface::Pixels(white(400, 300));
        let settings = BrushSettings {
            size: 40.0,
            hardness: 1.0,
            ..Default::default()
        };
        let Surface::Pixels(out) = stroke(
            settings,
            Paint::Color([0, 0, 0, 65535]),
            &surface,
            &[(100.0, 100.0)],
        ) else {
            unreachable!()
        };
        assert_eq!(out.get(100, 100), [0, 0, 0, 65535]);
        assert_eq!(out.get(100, 125), [65535; 4]);
    }

    #[test]
    fn toning_strokes_lighten_darken_and_desaturate() {
        let grey = |c: Pixel| Tiled::from_slice(100, 100, [0; 4], &vec![c; 100 * 100]);
        let hard = BrushSettings { size: 40.0, hardness: 1.0, opacity: 0.5, ..Default::default() };
        let tone = |burn| Paint::Tone { range: ToneRange::Midtones, burn, protect: true };
        let run = |paint, surface: &Surface, points: &[(f32, f32)]| {
            let Surface::Pixels(out) = stroke(hard, paint, surface, points) else { unreachable!() };
            out
        };
        let mid = Surface::Pixels(grey([32768, 32768, 32768, 40000]));
        let dodged = run(tone(false), &mid, &[(50.0, 50.0)]);
        let burnt = run(tone(true), &mid, &[(50.0, 50.0)]);
        assert!(dodged.get(50, 50)[0] > 36000, "{:?}", dodged.get(50, 50));
        assert!(burnt.get(50, 50)[0] < 29000, "{:?}", burnt.get(50, 50));
        // Alpha is kept, and outside the brush nothing changes.
        assert_eq!(dodged.get(50, 50)[3], 40000);
        assert_eq!(dodged.get(5, 5), [32768, 32768, 32768, 40000]);
        // Going back over the same spot in one stroke doesn't build up.
        let scrubbed = run(tone(false), &mid, &[(50.0, 50.0), (60.0, 50.0), (50.0, 50.0)]);
        assert_eq!(scrubbed.get(50, 50), dodged.get(50, 50));

        let red = Surface::Pixels(grey([50000, 20000, 15000, 65535]));
        let p = run(Paint::Sponge { saturate: false, vibrance: false }, &red, &[(50.0, 50.0)]);
        let p = p.get(50, 50);
        assert!(p[0] - p[2] < 50000 - 15000, "{p:?}");

        // On a mask, Dodge lightens its greys and Sponge leaves them alone.
        let mask = Surface::Mask(Tiled::new(100, 100, 32768));
        let Surface::Mask(m) = stroke(hard, tone(false), &mask, &[(50.0, 50.0)]) else { unreachable!() };
        assert!(m.get(50, 50) > 36000);
        let sponge = Paint::Sponge { saturate: true, vibrance: true };
        let Surface::Mask(m) = stroke(hard, sponge, &mask, &[(50.0, 50.0)]) else { unreachable!() };
        assert_eq!(m.get(50, 50), 32768);
    }

    #[test]
    fn pen_pressure_thins_and_lightens_the_stroke() {
        let surface = Surface::Pixels(white(400, 100));
        let press = |settings: BrushSettings| {
            let mut s = Stroke::new(settings, Paint::Color([0, 0, 0, 65535]), surface.clone());
            let mut out = surface.clone();
            for (x, p) in [(50.0, 1.0), (350.0, 0.25)] {
                let tiles = s.add_point(x, 50.0, p);
                s.apply(&mut out, &tiles);
            }
            let Surface::Pixels(out) = out else { unreachable!() };
            out
        };
        let hard = BrushSettings {
            size: 40.0,
            hardness: 1.0,
            ..Default::default()
        };
        let out = press(hard);
        // Full pressure: wide and black. Light pressure: thin and grey.
        assert_eq!(out.get(50, 65)[0], 0);
        assert_eq!(out.get(345, 65)[0], 65535);
        let grey = out.get(345, 50)[0];
        assert!((40000..55000).contains(&grey), "{grey}");

        let out = press(BrushSettings {
            size_pressure: false,
            opacity_pressure: false,
            ..hard
        });
        assert_eq!(out.get(345, 65)[0], 0);
        assert_eq!(out.get(345, 50)[0], 0);

        // Pressure on flow: light pressure builds up only part way, even
        // with no cap on opacity.
        let out = press(BrushSettings {
            size_pressure: false,
            opacity_pressure: false,
            flow_pressure: true,
            ..hard
        });
        assert_eq!(out.get(60, 50)[0], 0);
        let light = out.get(345, 50)[0];
        assert!((1000..60000).contains(&light), "{light}");
    }

    #[test]
    fn keeping_alpha_changes_only_colour() {
        // Opaque, half-transparent and transparent white columns.
        let px: Vec<Pixel> = (0..30 * 10)
            .map(|i| [65535, 65535, 65535, [65535, 32768, 0][i % 30 / 10]])
            .collect();
        let surface = Surface::Pixels(Tiled::from_slice(30, 10, [0; 4], &px));
        let settings = BrushSettings {
            size: 200.0,
            hardness: 1.0,
            opacity: 0.5,
            ..Default::default()
        };
        let run = |paint| {
            let mut s = Stroke::new(settings, paint, surface.clone()).keeping_alpha();
            let mut out = surface.clone();
            let tiles = s.add_point(15.0, 5.0, 1.0);
            s.apply(&mut out, &tiles);
            let Surface::Pixels(out) = out else { unreachable!() };
            [5, 15, 25].map(|x| out.get(x, 5))
        };
        // Half-way to black everywhere, as if opaque, with alpha unchanged.
        let painted = run(Paint::Color([0, 0, 0, 65535]));
        for (p, alpha) in painted.iter().zip([65535, 32768, 0]) {
            assert!(p[0].abs_diff(32768) <= 1, "{p:?}");
            assert_eq!(p[3], alpha);
        }
        // Erasing does nothing.
        assert_eq!(run(Paint::Erase), [px[5], px[15], px[25]]);
    }

    #[test]
    fn opacity_caps_a_stroke_that_crosses_itself() {
        let surface = Surface::Pixels(white(300, 300));
        let settings = BrushSettings {
            size: 30.0,
            hardness: 1.0,
            opacity: 0.5,
            ..Default::default()
        };
        let path = [(50.0, 50.0), (250.0, 50.0), (50.0, 50.0), (250.0, 50.0)];
        let Surface::Pixels(out) =
            stroke(settings, Paint::Color([0, 0, 0, 65535]), &surface, &path)
        else {
            unreachable!()
        };
        // Half-way to black, however many times the stroke passed.
        assert!(
            out.get(150, 50)[0].abs_diff(32768) <= 2,
            "{:?}",
            out.get(150, 50)
        );
    }

    #[test]
    fn low_flow_builds_up_gradually() {
        let surface = Surface::Pixels(white(200, 200));
        let settings = BrushSettings {
            size: 30.0,
            hardness: 1.0,
            flow: 0.2,
            ..Default::default()
        };
        // Passing back and forth over a spot darkens it further each time.
        let once = [(50.0, 100.0), (150.0, 100.0)];
        let thrice = [(50.0, 100.0), (150.0, 100.0), (50.0, 100.0), (150.0, 100.0)];
        let one = stroke(settings, Paint::Color([0, 0, 0, 65535]), &surface, &once);
        let many = stroke(settings, Paint::Color([0, 0, 0, 65535]), &surface, &thrice);
        let (Surface::Pixels(one), Surface::Pixels(many)) = (one, many) else {
            unreachable!()
        };
        assert!(many.get(100, 100)[0] < one.get(100, 100)[0]);
    }

    #[test]
    fn soft_edges_fade_out() {
        let surface = Surface::Pixels(white(200, 200));
        let settings = BrushSettings {
            size: 60.0,
            hardness: 0.0,
            ..Default::default()
        };
        let Surface::Pixels(out) = stroke(
            settings,
            Paint::Color([0, 0, 0, 65535]),
            &surface,
            &[(100.0, 100.0)],
        ) else {
            unreachable!()
        };
        let centre = out.get(100, 100)[0];
        let mid = out.get(115, 100)[0];
        let edge = out.get(128, 100)[0];
        assert!(centre < mid && mid < edge && edge < 65535);
    }

    #[test]
    fn eraser_makes_pixels_transparent_and_mask_paint_hides() {
        let pixels = Surface::Pixels(white(100, 100));
        let settings = BrushSettings {
            size: 20.0,
            hardness: 1.0,
            ..Default::default()
        };
        let Surface::Pixels(erased) = stroke(settings, Paint::Erase, &pixels, &[(50.0, 50.0)])
        else {
            unreachable!()
        };
        assert_eq!(erased.get(50, 50)[3], 0);

        let mask = Surface::Mask(Tiled::new(100, 100, u16::MAX));
        let Surface::Mask(painted) = stroke(settings, Paint::Mask(0), &mask, &[(50.0, 50.0)])
        else {
            unreachable!()
        };
        assert_eq!(painted.get(50, 50), 0);
        assert_eq!(painted.get(5, 5), u16::MAX);
    }

    /// A smooth gradient with a dark blemish at (150, 100).
    fn skin(w: u32, h: u32) -> Tiled<Pixel> {
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let tone = 30000 + (x * 60) as u16 + (y * 20) as u16;
                let blemish = (x as i32 - 150).pow(2) + (y as i32 - 100).pow(2) < 100;
                if blemish {
                    [8000, 6000, 6000, 65535]
                } else {
                    [tone, tone - 4000, tone - 8000, 65535]
                }
            })
            .collect();
        Tiled::from_slice(w, h, [0; 4], &px)
    }

    #[test]
    fn clone_copies_from_the_offset() {
        let img = skin(300, 200);
        let surface = Surface::Pixels(img.clone());
        let settings = BrushSettings {
            size: 30.0,
            hardness: 1.0,
            ..Default::default()
        };
        let mut s = Stroke::new(settings, Paint::Clone { dx: 0, dy: 50 }, surface.clone())
            .sampling(img.clone());
        let mut out = surface;
        let tiles = s.add_point(150.0, 100.0, 1.0);
        s.apply(&mut out, &tiles);
        let Surface::Pixels(out) = out else {
            unreachable!()
        };
        assert_eq!(out.get(150, 100), img.get(150, 150));
    }

    #[test]
    fn healing_removes_the_blemish_and_matches_the_surroundings() {
        let img = skin(300, 200);
        let surface = Surface::Pixels(img.clone());
        let settings = BrushSettings {
            size: 30.0,
            hardness: 0.5,
            ..Default::default()
        };
        // Source 60 px to the left: same texture, but a darker tone there.
        let mut s = Stroke::new(settings, Paint::Heal { dx: -60, dy: 0 }, surface.clone())
            .sampling(img.clone());
        let mut out = surface;
        let tiles = s.add_point(150.0, 100.0, 1.0);
        s.apply(&mut out, &tiles);
        let tiles = s.finish(&mut out);
        assert!(!tiles.is_empty());
        let Surface::Pixels(out) = out else {
            unreachable!()
        };
        let healed = out.get(150, 100)[0];
        // What unblemished skin at that spot would be.
        let expected = 30000 + 150 * 60 + 100 * 20;
        assert!(
            healed.abs_diff(expected) < 800,
            "healed {healed}, expected about {expected}"
        );
        // A plain clone would have brought the darker tone from the left.
        let cloned = img.get(90, 100)[0];
        assert!(healed.abs_diff(expected) < cloned.abs_diff(expected) / 3);
    }

    #[test]
    fn healing_meets_what_is_round_it_at_a_hard_edge() {
        // Plain skin on the left to copy from, and uneven light on the
        // right, where the dab lands.
        let (w, h) = (300, 200);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let tone = if x < 100.0 { 40000.0 } else { 36000.0 + 12000.0 * (x / 15.0).sin() * (y / 20.0).cos() };
                [tone as u16, tone as u16 - 4000, tone as u16 - 8000, 65535]
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);
        let surface = Surface::Pixels(img.clone());
        let settings = BrushSettings { size: 60.0, hardness: 1.0, ..Default::default() };
        let mut s = Stroke::new(settings, Paint::Heal { dx: -140, dy: 0 }, surface.clone()).sampling(img.clone());
        let mut out = surface;
        let tiles = s.add_point(200.0, 100.0, 1.0);
        s.apply(&mut out, &tiles);
        s.finish(&mut out);
        let Surface::Pixels(out) = out else { unreachable!() };
        // No step at the dab's edge: just inside it, the skin's nearly as it was.
        let mut worst = 0;
        for i in 0..w * h {
            let (x, y) = (i % w, i / w);
            let d = (x as f32 + 0.5 - 200.0).hypot(y as f32 + 0.5 - 100.0);
            if (29.0..30.0).contains(&d) {
                for c in 0..3 {
                    worst = worst.max(out.get(x, y)[c].abs_diff(img.get(x, y)[c]));
                }
            }
        }
        assert!(worst < 1500, "{worst}");
    }

    #[test]
    fn a_selection_keeps_what_is_outside_it_out_of_a_heal() {
        // Skin with something dark beside it, which the dab overlaps and
        // the selection stops short of.
        let (w, h) = (300, 200);
        let px: Vec<Pixel> = (0..w * h).map(|i| if i % w >= 215 { [5000, 5000, 5000, 65535] } else { [40000, 36000, 32000, 65535] }).collect();
        let img = Tiled::from_slice(w, h, [0; 4], &px);
        let surface = Surface::Pixels(img.clone());
        let settings = BrushSettings { size: 60.0, hardness: 0.5, ..Default::default() };
        let selection = crate::selection::Selection::rectangle(w, h, (0.0, 0.0), (212.0, 200.0));
        let mut s = Stroke::new(settings, Paint::Heal { dx: -100, dy: 0 }, surface.clone())
            .sampling(img.clone())
            .within(selection.coverage);
        let mut out = surface;
        let tiles = s.add_point(190.0, 100.0, 1.0);
        s.apply(&mut out, &tiles);
        s.finish(&mut out);
        let Surface::Pixels(out) = out else { unreachable!() };
        // The skin beside the dark isn't darkened by it.
        for x in 165..212 {
            assert!(out.get(x, 100)[0].abs_diff(40000) < 500, "{x}: {}", out.get(x, 100)[0]);
        }
        assert_eq!(out.get(216, 100), img.get(216, 100));
    }

    #[test]
    fn selection_limits_where_strokes_paint() {
        let surface = Surface::Pixels(white(200, 100));
        let settings = BrushSettings {
            size: 60.0,
            hardness: 1.0,
            ..Default::default()
        };
        let selection =
            crate::selection::Selection::rectangle(200, 100, (0.0, 0.0), (100.0, 100.0));
        let mut s = Stroke::new(settings, Paint::Color([0, 0, 0, 65535]), surface.clone())
            .within(selection.coverage);
        let mut out = surface;
        let tiles = s.add_point(100.0, 50.0, 1.0);
        s.apply(&mut out, &tiles);
        let Surface::Pixels(out) = out else {
            unreachable!()
        };
        assert_eq!(out.get(90, 50)[0], 0);
        assert_eq!(out.get(110, 50)[0], 65535);
    }

    #[test]
    fn spot_healing_finds_a_clean_source_by_itself() {
        // Skin with a blemish, and a hard dark bar nearby that a careless
        // source choice would copy in.
        let (w, h) = (400, 240);
        let mut img = skin(w, h).to_vec();
        for y in 0..h {
            for x in 100..112 {
                img[(y * w + x) as usize] = [3000, 3000, 3000, 65535];
            }
        }
        let img = Tiled::from_slice(w, h, [0; 4], &img);
        let surface = Surface::Pixels(img.clone());
        let settings = BrushSettings {
            size: 30.0,
            hardness: 0.5,
            ..Default::default()
        };
        let mut s = Stroke::new(settings, Paint::SpotHeal, surface.clone()).sampling(img.clone());
        let mut out = surface;
        let tiles = s.add_point(150.0, 100.0, 1.0);
        s.apply(&mut out, &tiles);
        let tiles = s.finish(&mut out);
        assert!(!tiles.is_empty());
        let Surface::Pixels(out) = out else {
            unreachable!()
        };
        let healed = out.get(150, 100)[0];
        let expected = 30000 + 150 * 60 + 100 * 20;
        assert!(
            healed.abs_diff(expected) < 1000,
            "healed {healed}, expected about {expected}"
        );
        // No dark bar pulled into the patch.
        for x in 140..160 {
            assert!(out.get(x, 100)[0] > 20000, "dark pixel at {x}");
        }
    }

    #[test]
    fn spot_healing_copies_texture_like_the_skin_round_it() {
        // Pored skin (a dot every 6 px) with a spot, and a smooth patch just
        // to its right that a choice by likeness pixel by pixel would take.
        let (w, h) = (600, 300);
        let img: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let (dx, dy) = (x as i32 - 200, y as i32 - 150);
                let v = if dx * dx + dy * dy < 100 {
                    20000
                } else if (250..330).contains(&x) {
                    40000
                } else if x % 6 < 2 && y % 6 < 2 {
                    30000
                } else {
                    42000
                };
                [v, v - 6000, v - 10000, 65535]
            })
            .collect();
        let img = Tiled::from_slice(w, h, [0; 4], &img);
        let surface = Surface::Pixels(img.clone());
        let settings = BrushSettings {
            size: 40.0,
            hardness: 0.8,
            ..Default::default()
        };
        let mut s = Stroke::new(settings, Paint::SpotHeal, surface.clone()).sampling(img);
        let mut out = surface;
        let tiles = s.add_point(200.0, 150.0, 1.0);
        s.apply(&mut out, &tiles);
        s.finish(&mut out);
        let Surface::Pixels(out) = out else {
            unreachable!()
        };
        // Pores in the healed patch, not the smooth patch's evenness.
        let (lo, hi) = (140..160)
            .flat_map(|y| (190..210).map(move |x| (x, y)))
            .map(|(x, y)| out.get(x, y)[0])
            .fold((u16::MAX, 0), |(lo, hi), v| (lo.min(v), hi.max(v)));
        assert!(hi - lo > 6000, "{lo}..{hi}");
        assert!(lo > 25000, "the spot's still there: {lo}");
    }

    #[test]
    fn dabs_are_evenly_spaced_across_segments() {
        let surface = Surface::Pixels(white(600, 100));
        let settings = BrushSettings {
            size: 10.0,
            hardness: 1.0,
            ..Default::default()
        };
        // A line drawn in many small moves covers every pixel along it.
        let points: Vec<(f32, f32)> = (0..500).map(|i| (50.0 + i as f32, 50.0)).collect();
        let Surface::Pixels(out) =
            stroke(settings, Paint::Color([0, 0, 0, 65535]), &surface, &points)
        else {
            unreachable!()
        };
        assert!((60..540).all(|x| out.get(x, 50)[0] == 0));
    }

    /// A black stroke through `points` on white, and its red channel.
    fn stroked(settings: BrushSettings, points: &[(f32, f32)], finish: bool) -> Tiled<Pixel> {
        let mut out = Surface::Pixels(white(300, 200));
        let mut s = Stroke::new(settings, Paint::Color([0, 0, 0, 65535]), out.clone());
        for &(x, y) in points {
            let tiles = s.add_point(x, y, 1.0);
            s.apply(&mut out, &tiles);
        }
        if finish {
            s.finish(&mut out);
        }
        let Surface::Pixels(out) = out else { unreachable!() };
        out
    }

    fn hard(size: f32, dynamics: Dynamics) -> BrushSettings {
        BrushSettings { size, hardness: 1.0, dynamics, ..Default::default() }
    }

    #[test]
    fn a_flat_tip_paints_an_ellipse_at_its_angle() {
        // Half as high as it's wide: 40 px across, 20 up and down.
        let flat = Dynamics { roundness: 0.5, ..Default::default() };
        let out = stroked(hard(40.0, flat), &[(100.0, 100.0)], true);
        let painted = |x, y| out.get(x, y)[0] == 0;
        assert!(painted(117, 100) && painted(83, 100) && painted(100, 108) && painted(100, 91));
        assert!(!painted(100, 112) && !painted(100, 87) && !painted(122, 100));
        // Turned a quarter, it stands up.
        let out = stroked(hard(40.0, Dynamics { angle: 90.0, ..flat }), &[(100.0, 100.0)], true);
        assert!(out.get(100, 117)[0] == 0 && out.get(108, 100)[0] == 0 && out.get(112, 100)[0] == 65535);
        // Turned an eighth, anticlockwise: its ends are up to the right
        // and down to the left.
        let out = stroked(hard(40.0, Dynamics { angle: 45.0, ..flat }), &[(100.0, 100.0)], true);
        assert!(out.get(112, 88)[0] == 0 && out.get(88, 112)[0] == 0);
        assert!(out.get(112, 112)[0] == 65535 && out.get(88, 88)[0] == 65535);
    }

    #[test]
    fn wide_spacing_leaves_gaps_between_dabs() {
        // 10 px dabs every 30 px.
        let apart = Dynamics { spacing: 3.0, ..Default::default() };
        let out = stroked(hard(10.0, apart), &[(50.0, 100.0), (200.0, 100.0)], true);
        let painted: Vec<bool> = (40..210).map(|x| out.get(x, 100)[0] == 0).collect();
        let dabs = painted.windows(2).filter(|w| !w[0] && w[1]).count();
        assert_eq!(dabs, 6, "at 50, 80, 110, 140, 170 and 200");
        assert!(out.get(50, 100)[0] == 0 && out.get(65, 100)[0] == 65535 && out.get(80, 100)[0] == 0);
    }

    #[test]
    fn the_first_dab_waits_for_the_strokes_direction() {
        // A flat tip that follows the stroke.
        let following = Dynamics { roundness: 0.25, angle_follows: true, spacing: 10.0, ..Default::default() };
        // Nothing until the pointer moves, and then the dab lies along
        // the way it went: down.
        let out = stroked(hard(40.0, following), &[(100.0, 100.0)], false);
        assert_eq!(out.get(100, 100)[0], 65535);
        let out = stroked(hard(40.0, following), &[(100.0, 100.0), (100.0, 110.0)], false);
        assert!(out.get(100, 117)[0] == 0 && out.get(100, 83)[0] == 0 && out.get(110, 100)[0] == 65535);
        // A click that never moves gets its dab when it's let go.
        let out = stroked(hard(40.0, following), &[(100.0, 100.0), (100.0, 100.0)], true);
        assert!(out.get(117, 100)[0] == 0 && out.get(100, 110)[0] == 65535);
    }

    #[test]
    fn a_scattered_stroke_is_the_same_however_its_points_arrive() {
        let scattered = Dynamics {
            spacing: 0.5,
            scatter: 3.0,
            count: 3,
            count_jitter: 0.5,
            size_jitter: 0.8,
            angle_jitter: 1.0,
            roundness_jitter: 0.5,
            opacity_jitter: 0.5,
            flow_jitter: 0.5,
            ..Default::default()
        };
        let whole = stroked(hard(12.0, scattered), &[(40.0, 100.0), (240.0, 100.0)], true);
        let parts = stroked(hard(12.0, scattered), &[(40.0, 100.0), (90.0, 100.0), (171.0, 100.0), (240.0, 100.0)], true);
        assert!(whole.to_vec() == parts.to_vec());
        // Dabs off the line, well beyond the brush's own 6 px.
        let far = (60..220).any(|x| (60..82).chain(118..140).any(|y| whole.get(x, y)[0] < 65535));
        assert!(far);
        // Another stroke elsewhere scatters differently.
        let other = stroked(hard(12.0, scattered), &[(40.0, 101.0), (240.0, 101.0)], true);
        assert!((0..200).any(|y| (0..300).any(|x| whole.get(x, y) != other.get(x, (y + 1).min(199)))));
    }

    #[test]
    fn minimum_diameter_keeps_a_light_touch_from_vanishing() {
        let settings = BrushSettings { size: 40.0, hardness: 1.0, ..Default::default() };
        let stroke = |least: f32| {
            let dynamics = Dynamics { minimum_diameter: least, ..Default::default() };
            Stroke::new(BrushSettings { dynamics, ..settings }, Paint::Erase, Surface::Pixels(white(8, 8)))
        };
        assert_eq!((stroke(0.0).diameter(0.0), stroke(0.0).diameter(0.5)), (0.0, 20.0));
        assert_eq!((stroke(0.5).diameter(0.0), stroke(0.5).diameter(0.5), stroke(0.5).diameter(1.0)), (20.0, 30.0, 40.0));
    }

    #[test]
    fn a_sampled_tip_is_stamped_the_right_way_up_at_the_brushs_size_and_angle() {
        // Twice as wide as it's high, with paint in its right half only.
        let tip = Arc::new(Tip::new(40, 20, (0..800).map(|i| if i % 40 >= 20 { 65535 } else { 0 }).collect()).unwrap());
        let stamp = |size: f32, dynamics: Dynamics| {
            let settings = BrushSettings { size, hardness: 0.0, dynamics, ..Default::default() };
            let mut out = Surface::Pixels(white(300, 200));
            let mut s = Stroke::new(settings, Paint::Color([0, 0, 0, 65535]), out.clone()).with_tip(tip.clone());
            let tiles = s.add_point(100.0, 100.0, 1.0);
            s.apply(&mut out, &tiles);
            let Surface::Pixels(out) = out else { unreachable!() };
            move |x: u32, y: u32| out.get(x, y)[0]
        };
        // At twice its size: 80 px wide, 40 high, the right half black to
        // its edges whatever the hardness.
        let at = stamp(80.0, Dynamics::default());
        assert!(at(105, 100) == 0 && at(138, 82) == 0 && at(138, 118) == 0 && at(102, 118) == 0);
        assert!(at(95, 100) == 65535 && at(62, 100) == 65535 && at(120, 78) == 65535 && at(142, 100) == 65535);
        // Small, it's read from a smaller copy of itself: still the right half.
        let at = stamp(8.0, Dynamics::default());
        assert!(at(102, 100) == 0 && at(97, 100) == 65535 && at(102, 103) == 65535);
        // Turned a quarter anticlockwise, its right half points up.
        let at = stamp(80.0, Dynamics { angle: 90.0, ..Default::default() });
        assert!(at(100, 80) == 0 && at(118, 62) == 0 && at(82, 62) == 0 && at(100, 120) == 65535 && at(125, 80) == 65535);
        // At an eighth, its far corners reach beyond the brush's radius.
        let at = stamp(80.0, Dynamics { angle: 45.0, ..Default::default() });
        assert!(at(138, 85) == 0 && at(114, 61) == 0 && at(88, 112) == 65535);
        // Half as round, it's half as high.
        let at = stamp(80.0, Dynamics { roundness: 0.5, ..Default::default() });
        assert!(at(120, 108) == 0 && at(120, 112) == 65535 && at(120, 92) == 0 && at(120, 88) == 65535);
    }

    #[test]
    fn a_flipped_tip_is_stamped_mirrored() {
        // Paint in the tip's top right quarter only.
        let tip = Arc::new(Tip::new(40, 40, (0..1600).map(|i| if i % 40 >= 20 && i / 40 < 20 { 65535 } else { 0 }).collect()).unwrap());
        let quarters = |dynamics: Dynamics| {
            let settings = BrushSettings { size: 80.0, dynamics, ..Default::default() };
            let mut out = Surface::Pixels(white(300, 200));
            let mut s = Stroke::new(settings, Paint::Color([0, 0, 0, 65535]), out.clone()).with_tip(tip.clone());
            let tiles = s.add_point(100.0, 100.0, 1.0);
            s.apply(&mut out, &tiles);
            let Surface::Pixels(out) = out else { unreachable!() };
            // Top left, top right, bottom left, bottom right.
            [(80, 80), (120, 80), (80, 120), (120, 120)].map(|(x, y)| out.get(x, y)[0] == 0)
        };
        let d = Dynamics::default();
        assert_eq!(quarters(d), [false, true, false, false]);
        assert_eq!(quarters(Dynamics { flip_x: true, ..d }), [true, false, false, false]);
        assert_eq!(quarters(Dynamics { flip_y: true, ..d }), [false, false, false, true]);
        assert_eq!(quarters(Dynamics { flip_x: true, flip_y: true, ..d }), [false, false, true, false]);
    }

    #[test]
    fn noise_breaks_up_a_dabs_soft_edge_and_wet_edges_thin_its_middle() {
        let dab = |hardness: f32, dynamics: Dynamics| stroked(BrushSettings { size: 120.0, hardness, dynamics, ..Default::default() }, &[(150.0, 100.0)], true);
        let d = Dynamics::default();
        // How much neighbours differ along a line out through the soft edge.
        let rough = |out: &Tiled<Pixel>| (171..208).map(|x| u32::from(out.get(x, 100)[0].abs_diff(out.get(x + 1, 100)[0]))).sum::<u32>() / 37;
        let (smooth, noisy) = (dab(0.3, d), dab(0.3, Dynamics { noise: true, ..d }));
        assert!(rough(&smooth) < 2500 && rough(&noisy) > 10_000, "{} {}", rough(&smooth), rough(&noisy));
        // The solid middle and what's outside are as they were, and it's
        // the same grain each time.
        assert_eq!((noisy.get(150, 100), noisy.get(160, 105), noisy.get(215, 100)), (smooth.get(150, 100), smooth.get(160, 105), [65535; 4]));
        assert!(noisy.to_vec() == dab(0.3, Dynamics { noise: true, ..d }).to_vec());

        // Wet Edges: half the paint where the stroke is solid, however
        // many dabs land there, and more of it in a band along its edge.
        let wet = stroked(BrushSettings { size: 120.0, hardness: 0.3, dynamics: Dynamics { wet_edges: true, ..d }, ..Default::default() }, &[(100.0, 100.0), (200.0, 100.0)], true);
        let grey = |y: u32| wet.get(150, y)[0];
        assert!(grey(100).abs_diff(32768) <= 1 && grey(110).abs_diff(32768) <= 1, "{} {}", grey(100), grey(110));
        let darkest = (100..160).map(grey).min().unwrap();
        assert!((22_000..24_500).contains(&darkest), "{darkest}");
        assert_eq!(grey(161), 65535);
    }
}
