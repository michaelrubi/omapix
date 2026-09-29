//! Brush strokes, painted the way Photoshop's Brush tool paints.
//!
//! A stroke lays round dabs along the pointer's path. Dabs build up a
//! per-stroke coverage buffer: each dab adds `flow` of its shape, and the
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

use crate::Pixel;
use crate::tiled::{TILE, TILE_PIXELS, Tiled};

const MAX: f32 = u16::MAX as f32;

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
    /// Coverage of touched tiles, 0–1 per pixel.
    coverage: HashMap<(u32, u32), Vec<f32>>,
    /// The last point, and the pen's pressure there.
    last: Option<(f32, f32, f32)>,
    /// Distance travelled since the last dab.
    carried: f32,
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
            coverage: HashMap::new(),
            last: None,
            carried: 0.0,
        }
    }

    /// Only paint where `selection` covers (0–65535 per pixel).
    pub fn within(mut self, selection: Tiled<u16>) -> Self {
        self.limit = Some(selection);
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
        (self.diameter(pressure) * 0.1).max(0.5)
    }

    fn diameter(&self, pressure: f32) -> f32 {
        if self.settings.size_pressure {
            self.settings.size * pressure
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
            None => self.dab(x, y, pressure, &mut touched),
            Some((lx, ly, lp)) => {
                let (dx, dy) = (x - lx, y - ly);
                let length = (dx * dx + dy * dy).sqrt();
                let mut t = self.spacing(lp) - self.carried;
                let mut spaced = self.spacing(lp);
                while t <= length {
                    let f = t / length;
                    let p = lp + (pressure - lp) * f;
                    self.dab(lx + dx * f, ly + dy * f, p, &mut touched);
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

    fn dab(&mut self, cx: f32, cy: f32, pressure: f32, touched: &mut Vec<(u32, u32)>) {
        let (w, h) = self.original.size();
        let r = self.diameter(pressure) / 2.0;
        let hardness = self.settings.hardness.clamp(0.0, 0.999);
        let flow = self.settings.flow * if self.settings.flow_pressure { pressure } else { 1.0 };
        // Coverage builds up to this.
        let most = if self.settings.opacity_pressure { pressure } else { 1.0 };
        let x0 = (cx - r).floor().max(0.0) as u32;
        let y0 = (cy - r).floor().max(0.0) as u32;
        let x1 = ((cx + r).ceil() as u32).min(w);
        let y1 = ((cy + r).ceil() as u32).min(h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for row in y0 / TILE..=(y1 - 1) / TILE {
            for col in x0 / TILE..=(x1 - 1) / TILE {
                let cov = self
                    .coverage
                    .entry((col, row))
                    .or_insert_with(|| vec![0.0; TILE_PIXELS]);
                let (tx, ty) = (col * TILE, row * TILE);
                let mut hit = false;
                for py in y0.max(ty)..y1.min(ty + TILE) {
                    for px in x0.max(tx)..x1.min(tx + TILE) {
                        // Distance from the pixel centre, as a fraction of the radius.
                        let d = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2))
                            .sqrt()
                            / r.max(0.5);
                        let shape = falloff(d, hardness);
                        if shape <= 0.0 {
                            continue;
                        }
                        let c = &mut cov[((py - ty) * TILE + (px - tx)) as usize];
                        *c += (most - *c).max(0.0) * flow * shape;
                        hit = true;
                    }
                }
                if hit {
                    touched.push((col, row));
                }
            }
        }
    }

    /// Write the stroke's current result for the given tiles into `surface`.
    pub fn apply(&self, surface: &mut Surface, tiles: &[(u32, u32)]) {
        let opacity = self.settings.opacity;
        for &(col, row) in tiles {
            let Some(cov) = self.coverage.get(&(col, row)) else {
                continue;
            };
            match (&mut *surface, &self.original) {
                (Surface::Pixels(dst), Surface::Pixels(orig)) => {
                    let base: Vec<Pixel> = orig
                        .tile(col, row)
                        .map_or_else(|| vec![orig.fill(); TILE_PIXELS], <[Pixel]>::to_vec);
                    let copying = matches!(self.paint, Paint::Clone { .. } | Paint::Heal { .. });
                    let (tx, ty) = (col * TILE, row * TILE);
                    let (w, h) = (orig.width(), orig.height());
                    let out = dst.tile_mut(col, row);
                    for i in 0..TILE_PIXELS {
                        let a = cov[i] * opacity * self.limit_at(col, row, i);
                        out[i] = if self.paint == Paint::SpotHeal {
                            // Show where the stroke is until it heals on release.
                            self.paint_onto(base[i], a * 0.35, Paint::Color([0, 0, 0, u16::MAX]))
                        } else if copying {
                            let (x, y) = (tx + i as u32 % TILE, ty + i as u32 / TILE);
                            if a <= 0.0 || x >= w || y >= h {
                                base[i]
                            } else {
                                self.paint_onto(base[i], a, Paint::Color(self.copied(x, y)))
                            }
                        } else {
                            self.paint_onto(base[i], a, self.paint)
                        };
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
                    };
                    let base: Vec<u16> = orig
                        .tile(col, row)
                        .map_or_else(|| vec![orig.fill(); TILE_PIXELS], <[u16]>::to_vec);
                    let out = dst.tile_mut(col, row);
                    for i in 0..TILE_PIXELS {
                        let a = cov[i] * opacity * self.limit_at(col, row, i);
                        let v = f32::from(base[i]);
                        out[i] = (v + (target - v) * a).round() as u16;
                    }
                }
                _ => {}
            }
        }
    }
}

impl Stroke {
    /// Complete the stroke. For healing, this replaces the copied patch with
    /// a healed one and returns the tiles that changed; other strokes are
    /// already final.
    ///
    /// Healing keeps the copy's fine texture but takes its tone and colour
    /// from the destination's surroundings: it adds the difference between
    /// the smoothed surroundings of the destination and of the source. Both
    /// are smoothed with the painted area weighted out, so the blemish being
    /// covered doesn't tint the result. This is a fast approximation of
    /// Photoshop's healing brush (Poisson blending) that works well on skin.
    pub fn finish(&mut self, surface: &mut Surface) -> Vec<(u32, u32)> {
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
            return Vec::new();
        };
        let (Some(src), Surface::Pixels(orig), Surface::Pixels(dst)) =
            (&self.source, &self.original, surface)
        else {
            return Vec::new();
        };
        let tiles: Vec<(u32, u32)> = self.coverage.keys().copied().collect();
        if tiles.is_empty() {
            return tiles;
        }
        let (w, h) = (orig.width(), orig.height());
        let sigma = (self.settings.size * 0.5).max(3.0);
        let margin = (sigma * 3.0).ceil() as u32;
        let x0 = (tiles.iter().map(|t| t.0).min().unwrap() * TILE).saturating_sub(margin);
        let y0 = (tiles.iter().map(|t| t.1).min().unwrap() * TILE).saturating_sub(margin);
        let x1 = ((tiles.iter().map(|t| t.0).max().unwrap() + 1) * TILE + margin).min(w);
        let y1 = ((tiles.iter().map(|t| t.1).max().unwrap() + 1) * TILE + margin).min(h);
        let (rw, rh) = ((x1 - x0) as usize, (y1 - y0) as usize);

        let coverage_at = |x: u32, y: u32| -> f32 {
            self.coverage
                .get(&(x / TILE, y / TILE))
                .map_or(0.0, |c| c[((y % TILE) * TILE + x % TILE) as usize])
        };
        let norm = |p: Pixel| {
            [
                f32::from(p[0]) / MAX,
                f32::from(p[1]) / MAX,
                f32::from(p[2]) / MAX,
            ]
        };

        // Surroundings of destination and source, painted area weighted out.
        let mut dest = Vec::with_capacity(rw * rh);
        let mut copy = Vec::with_capacity(rw * rh);
        for y in y0..y1 {
            for x in x0..x1 {
                let weight = (1.0 - coverage_at(x, y)).powi(2);
                let d = norm(src.get(x, y));
                let c = self.copied(x, y);
                let c = if c[3] == 0 { d } else { norm(c) };
                dest.push([d[0] * weight, d[1] * weight, d[2] * weight, weight]);
                copy.push([c[0] * weight, c[1] * weight, c[2] * weight, weight]);
            }
        }
        let dest = crate::filters::blur_buffer(dest, rw, rh, sigma);
        let copy = crate::filters::blur_buffer(copy, rw, rh, sigma);

        let opacity = self.settings.opacity;
        for &(col, row) in &tiles {
            let cov = &self.coverage[&(col, row)];
            let base: Vec<Pixel> = orig
                .tile(col, row)
                .map_or_else(|| vec![orig.fill(); TILE_PIXELS], <[Pixel]>::to_vec);
            let out = dst.tile_mut(col, row);
            for i in 0..TILE_PIXELS {
                let a = cov[i] * opacity * self.limit_at(col, row, i);
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
                let (d, c) = (dest[r], copy[r]);
                let mut healed = copied;
                if d[3] > 1e-4 && c[3] > 1e-4 {
                    for ch in 0..3 {
                        let shift = d[ch] / d[3] - c[ch] / c[3];
                        let v = f32::from(copied[ch]) / MAX + shift;
                        healed[ch] = (v.clamp(0.0, 1.0) * MAX).round() as u16;
                    }
                }
                out[i] = self.paint_onto(base[i], a, Paint::Color(healed));
            }
        }
        tiles
    }
}

impl Stroke {
    /// Choose where a spot-heal stroke copies from: the nearby offset whose
    /// surroundings best match the painted area's surroundings, preferring
    /// even source patches so edges (hair, lips) and creases aren't pulled
    /// in.
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
        let step = (((rx1 - rx0).max(ry1 - ry0) / 64).max(1)) as usize;

        let rgb = |p: Pixel| {
            [
                f32::from(p[0]) / MAX,
                f32::from(p[1]) / MAX,
                f32::from(p[2]) / MAX,
            ]
        };
        let cov_at = |x: i64, y: i64| -> f32 {
            let (x, y) = (x as u32, y as u32);
            self.coverage
                .get(&(x / TILE, y / TILE))
                .map_or(0.0, |c| c[((y % TILE) * TILE + x % TILE) as usize])
        };

        let mut best: Option<(f32, (i32, i32))> = None;
        for ring in [1.1f32, 1.5, 2.0, 2.8] {
            let dist = ring * extent + margin as f32;
            for k in 0..16 {
                let angle = k as f32 / 16.0 * std::f32::consts::TAU;
                let (ox, oy) = (
                    (angle.cos() * dist).round() as i64,
                    (angle.sin() * dist).round() as i64,
                );
                if rx0 + ox < 0 || ry0 + oy < 0 || rx1 + ox + 1 > w || ry1 + oy + 1 > h {
                    continue;
                }
                let (mut ring_err, mut ring_n, mut edges, mut inner_n) =
                    (0.0f32, 0usize, 0.0f32, 0usize);
                // How the surroundings differ on average. Healing corrects
                // that, so it counts for less than a difference in shape,
                // but it still counts: skin shouldn't come from hair.
                let mut ring_shift = [0.0f32; 3];
                // The copied patch's sum and sum of squares, for its variance.
                let (mut sum, mut squares) = ([0.0f32; 3], 0.0f32);
                let mut usable = true;
                for y in (ry0..ry1).step_by(step) {
                    for x in (rx0..rx1).step_by(step) {
                        let s = src.get((x + ox) as u32, (y + oy) as u32);
                        if cov_at(x, y) < 0.01 {
                            let d = rgb(src.get(x as u32, y as u32));
                            let s = rgb(s);
                            for c in 0..3 {
                                ring_err += (d[c] - s[c]).powi(2);
                                ring_shift[c] += d[c] - s[c];
                            }
                            ring_n += 1;
                        } else {
                            if s[3] == 0 {
                                usable = false;
                            }
                            let (s, right, down) = (
                                rgb(s),
                                rgb(src.get((x + ox + 1) as u32, (y + oy) as u32)),
                                rgb(src.get((x + ox) as u32, (y + oy + 1) as u32)),
                            );
                            edges += (0..3)
                                .map(|c| (s[c] - right[c]).abs() + (s[c] - down[c]).abs())
                                .sum::<f32>();
                            for c in 0..3 {
                                sum[c] += s[c];
                                squares += s[c] * s[c];
                            }
                            inner_n += 1;
                        }
                    }
                }
                if !usable || ring_n == 0 {
                    continue;
                }
                // A patch that varies (a crease, another spot) would be
                // copied in; one with sharp edges (hair, lips) too.
                let n = inner_n.max(1) as f32;
                let variance = squares / n - sum.iter().map(|s| (s / n).powi(2)).sum::<f32>();
                let ring = ring_err / ring_n as f32
                    - 0.75 * ring_shift.iter().map(|s| (s / ring_n as f32).powi(2)).sum::<f32>();
                let score = ring + variance + 0.5 * (edges / n).powi(2);
                if best.is_none_or(|(b, _)| score < b) {
                    best = Some((score, (ox as i32, oy as i32)));
                }
            }
        }
        best.map(|(_, offset)| offset)
    }
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
}
