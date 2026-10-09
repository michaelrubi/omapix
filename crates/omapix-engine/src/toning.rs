//! Photoshop's toning tools: Dodge and Burn lighten and darken one tonal
//! range, Sponge saturates or desaturates. Each takes a colour and a
//! strength (0–1); the brush in [`crate::brush`] supplies the strength from
//! its coverage and Exposure (or Flow).
//!
//! Ported from PhotoCraft's `crates/algo/src/retouch.rs`
//! (<https://github.com/storytold/photocraft>, commit `ec477ca`), under its
//! MIT licence:
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

use crate::Pixel;

const MAX: f32 = u16::MAX as f32;

/// The tonal range Dodge and Burn work on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ToneRange {
    Shadows,
    #[default]
    Midtones,
    Highlights,
}

impl ToneRange {
    pub const ALL: [ToneRange; 3] = [ToneRange::Shadows, ToneRange::Midtones, ToneRange::Highlights];

    pub fn label(self) -> &'static str {
        match self {
            ToneRange::Shadows => "Shadows",
            ToneRange::Midtones => "Midtones",
            ToneRange::Highlights => "Highlights",
        }
    }
}

/// Rec. 601 luma.
#[inline]
fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// Dodge (or with `burn`, Burn) one value `v` (0–1) with strength `e` (0–1).
pub fn tone_curve(v: f32, e: f32, range: ToneRange, burn: bool) -> f32 {
    let v = v.clamp(0.0, 1.0);
    let e = e.clamp(0.0, 1.0);
    match (range, burn) {
        // A gamma bend weighted to peak at mid-grey: black and white stay put.
        (ToneRange::Midtones, _) => {
            let w = 4.0 * v * (1.0 - v);
            let gamma = if burn { 1.0 + e } else { 1.0 / (1.0 + e) };
            v + w * (v.powf(gamma) - v)
        }
        // The chosen end moves most: dodging shadows lifts black, burning
        // highlights pulls white down.
        (ToneRange::Shadows, false) => v + e * (1.0 - v).powi(3),
        (ToneRange::Shadows, true) => v - e * (1.0 - v).powi(2) * v,
        (ToneRange::Highlights, false) => v + e * v * v * (1.0 - v),
        (ToneRange::Highlights, true) => v - e * v.powi(3),
    }
}

/// Bring a colour back into 0–1 by mixing it toward its grey `l`, rather
/// than clipping each channel, which would shift its hue.
fn compress_gamut(c: [f32; 3], l: f32) -> [f32; 3] {
    let l = l.clamp(0.0, 1.0);
    let (lo, hi) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    let mut t = 1.0f32;
    if hi > 1.0 && hi - l > 1e-9 {
        t = t.min((1.0 - l) / (hi - l));
    }
    if lo < 0.0 && l - lo > 1e-9 {
        t = t.min(l / (l - lo));
    }
    c.map(|v| l + (v - l) * t)
}

/// Dodge or Burn a colour. With `protect`, Photoshop's Protect Tones, the
/// curve moves the colour's luma and the colour is scaled with it, keeping
/// its hue and saturation; without, each channel goes through the curve.
pub fn dodge_burn(c: [f32; 3], e: f32, range: ToneRange, burn: bool, protect: bool) -> [f32; 3] {
    if e <= 0.0 {
        return c;
    }
    if !protect {
        return c.map(|v| tone_curve(v, e, range, burn));
    }
    let l = luma(c);
    let nl = tone_curve(l, e, range, burn);
    let out = if l > 1e-6 { c.map(|v| v * nl / l) } else { [nl; 3] };
    compress_gamut(out, nl)
}

/// HSV saturation (chroma over the brightest channel).
fn saturation(c: [f32; 3]) -> f32 {
    let (lo, hi) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    if hi <= 1e-6 { 0.0 } else { ((hi - lo) / hi).clamp(0.0, 1.0) }
}

/// Sponge: move a colour away from its grey (`saturate`) or toward it, by
/// `amount` (0–1). `vibrance` saturates colours that already are less, and
/// desaturates near-greys less, so nothing clips or goes grey in one pass.
pub fn sponge(c: [f32; 3], amount: f32, saturate: bool, vibrance: bool) -> [f32; 3] {
    let mut a = amount.clamp(0.0, 1.0);
    if a <= 0.0 {
        return c;
    }
    if vibrance {
        let s = saturation(c);
        a *= if saturate { 1.0 - s } else { 0.25 + 0.75 * s };
    }
    let l = luma(c);
    let k = if saturate { 1.0 + a } else { 1.0 - a };
    compress_gamut(c.map(|v| l + (v - l) * k), l)
}

/// Run `f` on a pixel's colour as 0–1 floats, keeping its alpha.
pub fn on_pixel(p: Pixel, f: impl FnOnce([f32; 3]) -> [f32; 3]) -> Pixel {
    let c = f([p[0], p[1], p[2]].map(|v| f32::from(v) / MAX));
    let [r, g, b] = c.map(|v| (v.clamp(0.0, 1.0) * MAX).round() as u16);
    [r, g, b, p[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_move_their_range_most_and_keep_the_ends() {
        for burn in [false, true] {
            assert_eq!(tone_curve(0.0, 1.0, ToneRange::Midtones, burn), 0.0);
            assert_eq!(tone_curve(1.0, 1.0, ToneRange::Midtones, burn), 1.0);
        }
        assert!(tone_curve(0.5, 0.5, ToneRange::Midtones, false) > 0.5);
        assert!(tone_curve(0.5, 0.5, ToneRange::Midtones, true) < 0.5);
        let dodged = |v: f32, r| tone_curve(v, 0.5, r, false) - v;
        assert!(dodged(0.1, ToneRange::Shadows) > dodged(0.9, ToneRange::Shadows));
        assert!(dodged(0.9, ToneRange::Highlights) > dodged(0.1, ToneRange::Highlights));
        let burnt = |v: f32, r| v - tone_curve(v, 0.5, r, true);
        assert!(burnt(0.9, ToneRange::Highlights) > burnt(0.2, ToneRange::Highlights));
        assert!(burnt(0.3, ToneRange::Shadows) / 0.3 > burnt(0.9, ToneRange::Shadows) / 0.9);
        // No strength, no change.
        assert_eq!(dodge_burn([0.3, 0.4, 0.5], 0.0, ToneRange::Midtones, false, true), [0.3, 0.4, 0.5]);
    }

    #[test]
    fn protect_tones_keeps_hue() {
        let c = [0.6, 0.3, 0.2];
        let p = dodge_burn(c, 0.8, ToneRange::Midtones, false, true);
        assert!(p.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(luma(p) > luma(c));
        assert!((p[0] / p[1] - c[0] / c[1]).abs() < 0.05, "{p:?}");
    }

    #[test]
    fn sponge_changes_saturation_not_luma() {
        let c = [0.8, 0.4, 0.3];
        let d = sponge(c, 0.5, false, false);
        assert!(saturation(d) < saturation(c));
        assert!((luma(d) - luma(c)).abs() < 1e-4);
        let s = sponge(c, 0.5, true, false);
        assert!(saturation(s) > saturation(c));
        assert!(s.iter().all(|v| (0.0..=1.0).contains(v)));
        // Vibrance holds back colours that are already saturated.
        let v = sponge([1.0, 0.1, 0.1], 0.5, true, true);
        let nv = sponge([1.0, 0.1, 0.1], 0.5, true, false);
        assert!((v[1] - 0.1).abs() <= (nv[1] - 0.1).abs());
        assert_eq!(sponge([0.5; 3], 1.0, false, false), [0.5; 3]);
    }

    #[test]
    fn pixels_keep_alpha() {
        let p = on_pixel([30000, 20000, 10000, 1234], |c| dodge_burn(c, 0.5, ToneRange::Midtones, false, true));
        assert_eq!(p[3], 1234);
        assert!(p[0] > 30000);
    }
}
