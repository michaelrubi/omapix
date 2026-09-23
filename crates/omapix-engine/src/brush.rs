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

use std::collections::HashMap;

use crate::Pixel;
use crate::tiled::{TILE, TILE_PIXELS, Tiled};

const MAX: f32 = u16::MAX as f32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushSettings {
    /// Diameter in image pixels.
    pub size: f32,
    /// 0 = soft edge fading from the centre, 1 = hard edge.
    pub hardness: f32,
    /// Maximum coverage of one stroke, 0–1.
    pub opacity: f32,
    /// Coverage each dab adds, 0–1.
    pub flow: f32,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            size: 100.0,
            hardness: 0.0,
            opacity: 1.0,
            flow: 1.0,
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
    /// Coverage of touched tiles, 0–1 per pixel.
    coverage: HashMap<(u32, u32), Vec<f32>>,
    last: Option<(f32, f32)>,
    /// Distance travelled since the last dab.
    carried: f32,
}

impl Stroke {
    pub fn new(settings: BrushSettings, paint: Paint, original: Surface) -> Self {
        Self {
            settings,
            paint,
            original,
            coverage: HashMap::new(),
            last: None,
            carried: 0.0,
        }
    }

    fn spacing(&self) -> f32 {
        (self.settings.size * 0.1).max(0.5)
    }

    /// Continue the stroke to (x, y) in image pixels, laying evenly spaced
    /// dabs along the way. Returns the tiles whose coverage changed.
    pub fn add_point(&mut self, x: f32, y: f32) -> Vec<(u32, u32)> {
        let mut touched = Vec::new();
        match self.last {
            None => self.dab(x, y, &mut touched),
            Some((lx, ly)) => {
                let (dx, dy) = (x - lx, y - ly);
                let length = (dx * dx + dy * dy).sqrt();
                let spacing = self.spacing();
                let mut t = spacing - self.carried;
                while t <= length {
                    let f = t / length;
                    self.dab(lx + dx * f, ly + dy * f, &mut touched);
                    t += spacing;
                }
                self.carried = length - (t - spacing);
            }
        }
        self.last = Some((x, y));
        touched.sort_unstable();
        touched.dedup();
        touched
    }

    fn dab(&mut self, cx: f32, cy: f32, touched: &mut Vec<(u32, u32)>) {
        let (w, h) = self.original.size();
        let r = self.settings.size / 2.0;
        let hardness = self.settings.hardness.clamp(0.0, 0.999);
        let flow = self.settings.flow;
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
                        *c += (1.0 - *c) * flow * shape;
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
                    let out = dst.tile_mut(col, row);
                    for i in 0..TILE_PIXELS {
                        out[i] = paint_pixel(base[i], cov[i] * opacity, self.paint);
                    }
                }
                (Surface::Mask(dst), Surface::Mask(orig)) => {
                    let target = match self.paint {
                        Paint::Mask(v) => f32::from(v),
                        Paint::Erase => MAX,
                        Paint::Color(c) => f32::from(c[1]),
                    };
                    let base: Vec<u16> = orig
                        .tile(col, row)
                        .map_or_else(|| vec![orig.fill(); TILE_PIXELS], <[u16]>::to_vec);
                    let out = dst.tile_mut(col, row);
                    for i in 0..TILE_PIXELS {
                        let a = cov[i] * opacity;
                        let v = f32::from(base[i]);
                        out[i] = (v + (target - v) * a).round() as u16;
                    }
                }
                _ => {}
            }
        }
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
        Paint::Mask(_) => base,
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
            let tiles = s.add_point(x, y);
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
    fn opacity_caps_a_stroke_that_crosses_itself() {
        let surface = Surface::Pixels(white(300, 300));
        let settings = BrushSettings {
            size: 30.0,
            hardness: 1.0,
            opacity: 0.5,
            flow: 1.0,
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
            opacity: 1.0,
            flow: 0.2,
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
