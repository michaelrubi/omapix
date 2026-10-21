//! Photoshop's Brush Settings › Texture: a pattern worked into each dab
//! (Texture Each Tip) or into the stroke as a whole, so the paint takes the
//! pattern's grain. [`crate::brush`] combines it with the stroke's coverage.
//!
//! The ways of combining it are PhotoCraft's `mask_combine` and
//! `texture_combine` (`crates/paint/src/render.rs`), as they are since
//! [`81bff2aa`](https://github.com/storytold/photocraft/commit/81bff2aaa4267a2da9a4b6c85cdea451339628da),
//! measured against Photoshop; Brightness and Contrast are its
//! `Texture::level` (`crates/paint/src/brush.rs`), and a pattern's grey its
//! `pattern_tile` (`crates/io/src/abr_map.rs`), all at commit `9b964376`
//! (<https://github.com/storytold/photocraft>), under its MIT licence:
//!
//! Copyright (c) 2026 ArtCraft Team and the PhotoCraft contributors
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use crate::dynamics::mix;
use crate::patterns::PsdPattern;

/// The longest side a pattern is kept at; bigger ones are shrunk to it.
pub const MAX_EDGE: usize = 1024;

/// How the pattern meets the brush's paint, as Photoshop's Texture Mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    #[default]
    Multiply,
    Subtract,
    Darken,
    Overlay,
    ColorDodge,
    ColorBurn,
    LinearBurn,
    HardMix,
    LinearHeight,
    Height,
}

impl Mode {
    pub const ALL: [Mode; 10] = [
        Mode::Multiply,
        Mode::Subtract,
        Mode::Darken,
        Mode::Overlay,
        Mode::ColorDodge,
        Mode::ColorBurn,
        Mode::LinearBurn,
        Mode::HardMix,
        Mode::LinearHeight,
        Mode::Height,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Multiply => "Multiply",
            Mode::Subtract => "Subtract",
            Mode::Darken => "Darken",
            Mode::Overlay => "Overlay",
            Mode::ColorDodge => "Color Dodge",
            Mode::ColorBurn => "Color Burn",
            Mode::LinearBurn => "Linear Burn",
            Mode::HardMix => "Hard Mix",
            Mode::LinearHeight => "Linear Height",
            Mode::Height => "Height",
        }
    }

    /// The mode a brush file's `textureBlendMode` names.
    pub fn from_abr(code: &str) -> Option<Self> {
        Some(match code {
            "Mltp" => Mode::Multiply,
            "Sbtr" => Mode::Subtract,
            "Drkn" => Mode::Darken,
            "Ovrl" => Mode::Overlay,
            "CDdg" => Mode::ColorDodge,
            "CBrn" => Mode::ColorBurn,
            "linearBurn" => Mode::LinearBurn,
            "hardMix" => Mode::HardMix,
            "linearHeight" => Mode::LinearHeight,
            "height" | "Hght" => Mode::Height,
            _ => return None,
        })
    }
}

/// A brush's Texture settings.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Texture {
    /// The pattern ([`Pattern::id`]); the stroke is given the pattern
    /// itself.
    pub pattern: u64,
    /// The pattern's size (1 is 100 %).
    pub scale: f32,
    /// -1 to 1, Photoshop's -150 to 150.
    pub brightness: f32,
    /// -1 to 1, Photoshop's -50 to 0 and 0 to 100.
    pub contrast: f32,
    pub invert: bool,
    /// Into each dab, so Depth can vary from dab to dab, rather than into
    /// the stroke as a whole.
    pub each_tip: bool,
    pub mode: Mode,
    /// How strongly the pattern shows, 0–1.
    pub depth: f32,
    /// With each tip: the least Depth Jitter takes it to, as a fraction.
    pub minimum_depth: f32,
    /// With each tip: how much lower each dab's depth can be at random.
    pub depth_jitter: f32,
}

impl Default for Texture {
    fn default() -> Self {
        Self {
            pattern: 0,
            scale: 1.0,
            brightness: 0.0,
            contrast: 0.0,
            invert: false,
            each_tip: false,
            mode: Mode::Multiply,
            depth: 1.0,
            minimum_depth: 0.0,
            depth_jitter: 0.0,
        }
    }
}

impl Texture {
    /// A pattern value (0–1) after Brightness, Contrast and Invert: what the
    /// mode combines with the paint. Brightness shifts the pattern by up to
    /// 150 levels of 255; positive Contrast steepens it round mid-grey after
    /// that (100 is a threshold), negative flattens it before; Invert is
    /// last.
    pub fn level(&self, v: f32) -> f32 {
        let b = if self.brightness.is_finite() {
            self.brightness.clamp(-1.0, 1.0) * (150.0 / 255.0)
        } else {
            0.0
        };
        let c = if self.contrast.is_finite() { self.contrast.clamp(-1.0, 1.0) } else { 0.0 };
        let x = if c >= 0.0 {
            (v + b - 0.5) / (1.0 - c).max(1e-4) + 0.5
        } else {
            (v - 0.5) * (1.0 + c / 2.0) + 0.5 + b
        };
        let x = if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.5 };
        if self.invert { 1.0 - x } else { x }
    }

    /// A dab's depth, with `r` (0 up to 1) its random number.
    pub(crate) fn dab_depth(&self, r: f32) -> f32 {
        let depth = self.depth.clamp(0.0, 1.0);
        if !self.each_tip {
            return depth;
        }
        let least = self.minimum_depth.clamp(0.0, 1.0);
        depth * (1.0 - self.depth_jitter.clamp(0.0, 1.0) * r).max(least)
    }
}

/// A pattern: grey values from 0 to 1, repeated across the image.
#[derive(Debug, PartialEq)]
pub struct Pattern {
    id: u64,
    pub name: String,
    width: usize,
    height: usize,
    values: Vec<f32>,
}

impl Pattern {
    /// A pattern from its grey values, row by row. `None` if there are
    /// none, or not `width` × `height` of them.
    pub fn new(name: String, width: usize, height: usize, mut values: Vec<f32>) -> Option<Self> {
        if width == 0 || height == 0 || values.len() != width * height {
            return None;
        }
        let (mut w, mut h) = (width, height);
        while w.max(h) > MAX_EDGE {
            // Each pixel the mean of the four it covers.
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            values = (0..nw * nh)
                .map(|i| {
                    let (x, y) = (i % nw * 2, i / nw * 2);
                    let four = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)];
                    let there: Vec<f32> = four.iter().filter(|(x, y)| *x < w && *y < h).map(|(x, y)| values[y * w + x]).collect();
                    there.iter().sum::<f32>() / there.len() as f32
                })
                .collect();
            (w, h) = (nw, nh);
        }
        // What tells one pattern from another, in a brush's settings: its
        // size and pixels.
        let id = values.iter().fold(mix((w as u64) << 32 | h as u64), |id, &v| mix(id ^ u64::from(v.to_bits())));
        Some(Self {
            id,
            name,
            width: w,
            height: h,
            values,
        })
    }

    /// A pattern from a brush file: its brightness, with transparency as
    /// white.
    pub fn from_psd(p: &PsdPattern) -> Option<Self> {
        let n = p.width as usize * p.height as usize;
        let plane = |c: usize| -> Option<Vec<f32>> {
            let ch = p.channels.get(c)?;
            let v: Vec<f32> = match p.depth {
                8 => ch.iter().take(n).map(|&b| f32::from(b) / 255.0).collect(),
                16 => ch
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .take(n)
                    .map(|b| f32::from(u16::from_be_bytes(*b)) / 65535.0)
                    .collect(),
                32 => ch.as_chunks::<4>().0.iter().take(n).map(|b| f32::from_be_bytes(*b).clamp(0.0, 1.0)).collect(),
                _ => return None,
            };
            (v.len() == n).then_some(v)
        };
        let grey: Vec<f32> = match p.mode {
            3 => {
                let (r, g, b) = (plane(0)?, plane(1)?, plane(2)?);
                (0..n).map(|i| 0.299 * r[i] + 0.587 * g[i] + 0.114 * b[i]).collect()
            }
            2 => {
                let palette = p.palette.as_deref()?;
                let index = p.channels.first()?;
                (0..n)
                    .map(|i| {
                        let k = usize::from(index.get(i).copied().unwrap_or(0)) * 3;
                        let c = |o: usize| f32::from(palette.get(k + o).copied().unwrap_or(0)) / 255.0;
                        0.299 * c(0) + 0.587 * c(1) + 0.114 * c(2)
                    })
                    .collect()
            }
            // CMYK planes are stored inverted (0 is full ink).
            4 => {
                let (c, k) = (plane(0)?, plane(3)?);
                (0..n).map(|i| (c[i] * k[i]).clamp(0.0, 1.0)).collect()
            }
            _ => plane(0)?,
        };
        Self::new(p.name.clone(), p.width as usize, p.height as usize, grey)
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The pattern levelled by `texture`'s Brightness, Contrast and Invert,
    /// for a stroke.
    pub(crate) fn levelled(&self, texture: &Texture) -> Levelled {
        Levelled {
            width: self.width,
            height: self.height,
            scale: texture.scale.max(0.01),
            values: self.values.iter().map(|&v| texture.level(v)).collect(),
        }
    }

    /// Grey values (0–1), row by row, for a thumbnail.
    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

/// A pattern as a stroke reads it.
pub(crate) struct Levelled {
    width: usize,
    height: usize,
    scale: f32,
    values: Vec<f32>,
}

impl Levelled {
    fn wrapped(&self, x: f32, y: f32) -> f32 {
        let x = (x as i64).rem_euclid(self.width as i64) as usize;
        let y = (y as i64).rem_euclid(self.height as i64) as usize;
        self.values[y * self.width + x]
    }

    /// The pattern at the image's pixel (x, y), repeating, smoothly
    /// between its own pixels when scaled.
    pub(crate) fn at(&self, x: u32, y: u32) -> f32 {
        if self.scale == 1.0 {
            return self.wrapped(x as f32, y as f32);
        }
        let (fx, fy) = ((x as f32 + 0.5) / self.scale - 0.5, (y as f32 + 0.5) / self.scale - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let top = self.wrapped(x0, y0) + (self.wrapped(x0 + 1.0, y0) - self.wrapped(x0, y0)) * tx;
        let bottom = self.wrapped(x0, y0 + 1.0) + (self.wrapped(x0 + 1.0, y0 + 1.0) - self.wrapped(x0, y0 + 1.0)) * tx;
        top + (bottom - top) * ty
    }
}

/// How far a Height dab at full depth and coverage reaches into the
/// pattern, in pattern ranges (as PhotoCraft measured Photoshop).
const HEIGHT_REACH: f32 = 12.0;

/// The paint `v` (0–1) where the pattern is `t`, at `depth`. No paint
/// stays none. Subtract takes `depth × t` away; Color Dodge and Burn move
/// the pattern towards their neutral as depth falls; Height presses the
/// brush into the pattern as into a height field, so low depth paints only
/// its dark hollows, and Linear Height fades with the pattern where it
/// doesn't reach the top.
pub(crate) fn combine(mode: Mode, v: f32, t: f32, d: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let lerp = |a: f32, b: f32| a + (b - a) * d;
    let r = match mode {
        Mode::Multiply => v * (1.0 - d + d * t),
        Mode::Subtract => v - d * t,
        Mode::Darken => v.min(1.0 - d + d * t),
        Mode::Overlay => lerp(v, if v < 0.5 { 2.0 * v * t } else { 1.0 - 2.0 * (1.0 - v) * (1.0 - t) }),
        Mode::ColorDodge => v / (1.0 - d * t).max(1e-3),
        Mode::ColorBurn => 1.0 - (1.0 - v) / (1.0 - d + d * t).max(1e-3),
        Mode::LinearBurn => lerp(v, (v + t - 1.0).max(0.0)),
        Mode::HardMix => lerp(v, if v + t > 1.0 { 1.0 } else { 0.0 }),
        // Photoshop steps the depth in 1/255ths here, which shows at the
        // lowest depths.
        Mode::Height => HEIGHT_REACH * (d * 255.0).round() / 255.0 * v - t,
        Mode::LinearHeight => {
            let reach = HEIGHT_REACH * (d * 255.0).round() / 255.0 * v;
            if reach < 1.0 { reach * (1.0 - t) } else { reach - t }
        }
    };
    if r.is_finite() { r.clamp(0.0, 1.0) } else { 0.0 }
}

/// The paint `v` (0–1) where a Dual Brush's tip lays `t`: PhotoCraft's
/// `mask_combine` at full depth, which differs from Texture's in Subtract,
/// Color Dodge and Burn and the Heights.
pub(crate) fn dual(mode: Mode, v: f32, t: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let r = match mode {
        Mode::Multiply => v * t,
        Mode::Subtract => v - (1.0 - t),
        Mode::Darken => v.min(t),
        Mode::Overlay => {
            if v < 0.5 {
                2.0 * v * t
            } else {
                1.0 - 2.0 * (1.0 - v) * (1.0 - t)
            }
        }
        Mode::ColorDodge => v / (1.0 - t).max(1e-3),
        Mode::ColorBurn => 1.0 - (1.0 - v) / t.max(1e-3),
        Mode::LinearBurn => v + t - 1.0,
        Mode::HardMix => {
            if v + t > 1.0 {
                1.0
            } else {
                0.0
            }
        }
        Mode::LinearHeight => v * ((t - (1.0 - v)) * 4.0 + 0.5).clamp(0.0, 1.0),
        Mode::Height => {
            if t >= 1.0 - v {
                v
            } else {
                0.0
            }
        }
    };
    if r.is_finite() { r.clamp(0.0, 1.0) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_never_add_paint_and_full_depth_follows_the_pattern() {
        for mode in Mode::ALL {
            for t in [0.0, 0.3, 1.0] {
                assert_eq!(combine(mode, 0.0, t, 1.0), 0.0, "{mode:?}");
            }
        }
        // Multiply: a white pattern leaves the paint, black takes it all.
        assert_eq!(combine(Mode::Multiply, 0.8, 1.0, 1.0), 0.8);
        assert_eq!(combine(Mode::Multiply, 0.8, 0.0, 1.0), 0.0);
        assert!((combine(Mode::Multiply, 0.8, 0.0, 0.5) - 0.4).abs() < 1e-6);
        // Subtract: white takes the most away.
        assert!((combine(Mode::Subtract, 0.8, 0.5, 1.0) - 0.3).abs() < 1e-6);
        assert_eq!(combine(Mode::Subtract, 0.8, 1.0, 1.0), 0.0);
    }

    #[test]
    fn a_dual_tip_masks_the_paint_as_its_mode_says() {
        for mode in Mode::ALL {
            assert_eq!(dual(mode, 0.0, 1.0), 0.0, "{mode:?}");
        }
        assert_eq!((dual(Mode::Multiply, 0.8, 0.5), dual(Mode::Darken, 0.8, 0.5)), (0.4, 0.5));
        // Subtract takes away what the dual tip doesn't cover.
        assert!((dual(Mode::Subtract, 0.8, 0.5) - 0.3).abs() < 1e-6);
        assert_eq!((dual(Mode::Height, 0.6, 0.5), dual(Mode::Height, 0.6, 0.3)), (0.6, 0.0));
    }

    #[test]
    fn height_at_low_depth_paints_only_the_patterns_hollows() {
        // At 5 % the brush reaches 0.6 into the pattern: dark parts paint,
        // light ones don't.
        assert!(combine(Mode::Height, 1.0, 0.1, 0.05) > 0.4);
        assert_eq!(combine(Mode::Height, 1.0, 0.9, 0.05), 0.0);
        // From about 17 % the whole pattern is under the paint.
        assert_eq!(combine(Mode::Height, 1.0, 1.0, 0.2), 1.0);
        // Linear Height fades with the pattern where it can't reach the top.
        let (dark, light) = (combine(Mode::LinearHeight, 1.0, 0.2, 0.05), combine(Mode::LinearHeight, 1.0, 0.8, 0.05));
        assert!(dark > light && light > 0.0, "{dark} {light}");
    }

    #[test]
    fn brightness_contrast_and_invert_level_the_pattern() {
        let plain = Texture::default();
        assert_eq!(plain.level(0.3), 0.3);
        let brighter = Texture { brightness: 1.0, ..plain };
        assert!((brighter.level(0.3) - (0.3 + 150.0 / 255.0)).abs() < 1e-6);
        let threshold = Texture { contrast: 1.0, ..plain };
        assert_eq!((threshold.level(0.49), threshold.level(0.51)), (0.0, 1.0));
        assert!((Texture { invert: true, ..plain }.level(0.3) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn depth_jitter_varies_each_dabs_depth_down_to_the_minimum() {
        let t = Texture {
            depth: 0.8,
            depth_jitter: 1.0,
            minimum_depth: 0.25,
            each_tip: true,
            ..Default::default()
        };
        assert_eq!(t.dab_depth(0.0), 0.8);
        assert!((t.dab_depth(0.99) - 0.2).abs() < 1e-6);
        // Into the stroke as a whole, depth doesn't vary.
        assert_eq!(Texture { each_tip: false, ..t }.dab_depth(0.99), 0.8);
    }

    #[test]
    fn a_pattern_repeats_and_is_scaled_and_kept_small() {
        let p = Pattern::new("dots".into(), 2, 2, vec![0.0, 1.0, 1.0, 0.0]).unwrap();
        let l = p.levelled(&Texture::default());
        assert_eq!((l.at(0, 0), l.at(1, 0), l.at(2, 0), l.at(3, 3)), (0.0, 1.0, 0.0, 0.0));
        let twice = p.levelled(&Texture {
            scale: 2.0,
            ..Default::default()
        });
        // Pixels 0 and 1 fall in the pattern's first pixel, blending towards the next.
        assert!(twice.at(0, 0) < 0.5 && twice.at(2, 0) > 0.5);
        let big = Pattern::new("big".into(), 3000, 1, vec![0.5; 3000]).unwrap();
        assert_eq!(big.size(), (750, 1));
        assert!(Pattern::new("none".into(), 2, 2, vec![0.0; 3]).is_none());
        assert_ne!(p.id(), Pattern::new("other".into(), 2, 2, vec![1.0, 0.0, 0.0, 1.0]).unwrap().id());
    }
}
