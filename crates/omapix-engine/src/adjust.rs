//! Adjustment layers: colour and tone changes applied to everything below,
//! editable at any time, like Photoshop's.
//!
//! Each adjustment is a function from a colour to a colour (0–1 values in
//! the document's space). [`Adjustment::prepare`] turns the settings into a
//! fast form (lookup tables where possible) once per composite.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Adjustment {
    Curves(Curves),
    Levels(Levels),
    HueSaturation(HueSaturation),
    ColorBalance(ColorBalance),
    SelectiveColor(SelectiveColor),
    ChannelMixer(ChannelMixer),
    ColorLookup(ColorLookup),
}

impl Adjustment {
    pub fn name(&self) -> &'static str {
        match self {
            Adjustment::Curves(_) => "Curves",
            Adjustment::Levels(_) => "Levels",
            Adjustment::HueSaturation(_) => "Hue/Saturation",
            Adjustment::ColorBalance(_) => "Color Balance",
            Adjustment::SelectiveColor(_) => "Selective Color",
            Adjustment::ChannelMixer(_) => "Channel Mixer",
            Adjustment::ColorLookup(_) => "Color Lookup",
        }
    }

    pub fn prepare(&self) -> Prepared {
        match self {
            Adjustment::Curves(c) => {
                let master = Lut::from_fn(|x| c.master.eval(x));
                let channel = |curve: &Curve| Lut::from_fn(|x| master.get(curve.eval(x)));
                Prepared::Luts([channel(&c.red), channel(&c.green), channel(&c.blue)])
            }
            Adjustment::Levels(l) => {
                let lut = Lut::from_fn(|x| l.eval(x));
                Prepared::Luts([lut.clone(), lut.clone(), lut])
            }
            Adjustment::HueSaturation(h) => Prepared::HueSaturation(h.clone()),
            Adjustment::ColorBalance(b) => Prepared::ColorBalance(b.clone()),
            Adjustment::SelectiveColor(s) => Prepared::SelectiveColor(s.prepare()),
            Adjustment::ChannelMixer(m) => Prepared::ChannelMixer(m.prepare()),
            Adjustment::ColorLookup(lut) => Prepared::ColorLookup(lut.prepare()),
        }
    }

    /// Serialise for storing in a file.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("adjustments always serialise")
    }

    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}

/// An adjustment ready to apply to pixels.
pub enum Prepared {
    Luts([Lut; 3]),
    HueSaturation(HueSaturation),
    ColorBalance(ColorBalance),
    SelectiveColor(PreparedSelectiveColor),
    ChannelMixer(PreparedChannelMixer),
    ColorLookup(PreparedColorLookup),
}

impl Prepared {
    /// This adjustment sampled on an `n` × `n` × `n` grid over RGB 0–1, red
    /// varying fastest, for a GPU to interpolate.
    pub fn lut(&self, n: usize) -> Vec<[f32; 3]> {
        let step = 1.0 / (n - 1) as f32;
        (0..n * n * n)
            .map(|i| {
                let at = |k: usize| k as f32 * step;
                self.apply([at(i % n), at(i / n % n), at(i / (n * n))])
            })
            .collect()
    }

    #[inline]
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        match self {
            Prepared::Luts([r, g, b]) => [r.get(c[0]), g.get(c[1]), b.get(c[2])],
            Prepared::HueSaturation(h) => h.apply(c),
            Prepared::ColorBalance(b) => b.apply(c),
            Prepared::SelectiveColor(s) => s.apply(c),
            Prepared::ChannelMixer(m) => m.apply(c),
            Prepared::ColorLookup(lut) => lut.apply(c),
        }
    }
}

/// A 0–1 → 0–1 function sampled at 4096 points, linearly interpolated,
/// which is smooth enough for 16-bit images.
#[derive(Clone)]
pub struct Lut(Vec<f32>);

const LUT_SIZE: usize = 4096;

impl Lut {
    fn from_fn(f: impl Fn(f32) -> f32) -> Self {
        Self(
            (0..LUT_SIZE)
                .map(|i| f(i as f32 / (LUT_SIZE - 1) as f32).clamp(0.0, 1.0))
                .collect(),
        )
    }

    #[inline]
    fn get(&self, x: f32) -> f32 {
        let pos = x.clamp(0.0, 1.0) * (LUT_SIZE - 1) as f32;
        let i = (pos as usize).min(LUT_SIZE - 2);
        let t = pos - i as f32;
        self.0[i] + (self.0[i + 1] - self.0[i]) * t
    }
}

/// A tone curve through control points, as in Photoshop's Curves dialog.
/// Points are (input, output), 0–1, sorted by input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    pub points: Vec<(f32, f32)>,
}

impl Default for Curve {
    fn default() -> Self {
        Self {
            points: vec![(0.0, 0.0), (1.0, 1.0)],
        }
    }
}

impl Curve {
    pub fn is_identity(&self) -> bool {
        self.points.iter().all(|&(x, y)| (x - y).abs() < 1e-6)
    }

    /// Evaluate with a monotone cubic spline (Fritsch–Carlson), which passes
    /// through every point without the overshoot of a natural spline.
    /// Outside the first and last points the curve is flat.
    pub fn eval(&self, x: f32) -> f32 {
        let p = &self.points;
        match p.len() {
            0 => return x,
            1 => return p[0].1,
            _ => {}
        }
        if x <= p[0].0 {
            return p[0].1;
        }
        if x >= p[p.len() - 1].0 {
            return p[p.len() - 1].1;
        }
        let n = p.len();
        let slopes: Vec<f32> = (0..n - 1)
            .map(|i| {
                let dx = (p[i + 1].0 - p[i].0).max(1e-6);
                (p[i + 1].1 - p[i].1) / dx
            })
            .collect();
        let mut tangents = vec![0.0f32; n];
        tangents[0] = slopes[0];
        tangents[n - 1] = slopes[n - 2];
        for i in 1..n - 1 {
            tangents[i] = if slopes[i - 1] * slopes[i] <= 0.0 {
                0.0
            } else {
                (slopes[i - 1] + slopes[i]) / 2.0
            };
        }
        for i in 0..n - 1 {
            if slopes[i] == 0.0 {
                tangents[i] = 0.0;
                tangents[i + 1] = 0.0;
                continue;
            }
            let a = tangents[i] / slopes[i];
            let b = tangents[i + 1] / slopes[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                tangents[i] = t * a * slopes[i];
                tangents[i + 1] = t * b * slopes[i];
            }
        }
        let i = p
            .windows(2)
            .position(|w| x >= w[0].0 && x <= w[1].0)
            .unwrap_or(n - 2);
        let (x0, y0) = p[i];
        let (x1, y1) = p[i + 1];
        let h = (x1 - x0).max(1e-6);
        let t = (x - x0) / h;
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * y0 + h10 * h * tangents[i] + h01 * y1 + h11 * h * tangents[i + 1]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Curves {
    /// The RGB curve, applied after the per-channel curves.
    pub master: Curve,
    pub red: Curve,
    pub green: Curve,
    pub blue: Curve,
}

/// An eyedropper point to set from an image sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eyedropper {
    Black,
    Gray,
    White,
}

impl Eyedropper {
    pub fn label(self) -> &'static str {
        match self {
            Eyedropper::Black => "Set Black Point",
            Eyedropper::Gray => "Set Gray Point",
            Eyedropper::White => "Set White Point",
        }
    }
}

/// Luminance of an RGB colour (0.3 R + 0.59 G + 0.11 B), matching the histogram.
pub fn luminance([r, g, b]: [f32; 3]) -> f32 {
    0.3 * r + 0.59 * g + 0.11 * b
}

fn set_curve_black(curve: &mut Curve, in_val: f32) {
    let (_, y_last) = curve.points.last().copied().unwrap_or((1.0, 1.0));
    let x_last = curve.points.last().map_or(1.0, |p| p.0);
    let x = in_val.clamp(0.0, (x_last - 0.001).max(0.0));
    let mut remaining: Vec<(f32, f32)> = curve
        .points
        .iter()
        .copied()
        .filter(|&(px, _)| px > x && px < x_last)
        .collect();
    let mut new_points = Vec::with_capacity(remaining.len() + 2);
    new_points.push((x, 0.0));
    new_points.append(&mut remaining);
    new_points.push((x_last, y_last));
    curve.points = new_points;
}

fn set_curve_white(curve: &mut Curve, in_val: f32) {
    let (x_first, y_first) = curve.points.first().copied().unwrap_or((0.0, 0.0));
    let x = in_val.clamp((x_first + 0.001).min(1.0), 1.0);
    let mut remaining: Vec<(f32, f32)> = curve
        .points
        .iter()
        .copied()
        .filter(|&(px, _)| px > x_first && px < x)
        .collect();
    let mut new_points = Vec::with_capacity(remaining.len() + 2);
    new_points.push((x_first, y_first));
    new_points.append(&mut remaining);
    new_points.push((x, 1.0));
    curve.points = new_points;
}

fn set_curve_gray(curve: &mut Curve, in_val: f32, target: f32) {
    let (x_first, y_first) = curve.points.first().copied().unwrap_or((0.0, 0.0));
    let (x_last, y_last) = curve.points.last().copied().unwrap_or((1.0, 1.0));
    let x = in_val.clamp(
        (x_first + 0.001).min(1.0),
        (x_last - 0.001).max(x_first + 0.001),
    );
    let y = target.clamp(0.0, 1.0);
    curve.points = vec![(x_first, y_first), (x, y), (x_last, y_last)];
}

impl Curves {
    pub fn set_point(&mut self, eyedropper: Eyedropper, sample: [f32; 3]) {
        match eyedropper {
            Eyedropper::Black => self.set_black_point(sample),
            Eyedropper::Gray => self.set_gray_point(sample),
            Eyedropper::White => self.set_white_point(sample),
        }
    }

    fn set_black_point(&mut self, sample: [f32; 3]) {
        set_curve_black(&mut self.red, sample[0]);
        set_curve_black(&mut self.green, sample[1]);
        set_curve_black(&mut self.blue, sample[2]);
    }

    fn set_white_point(&mut self, sample: [f32; 3]) {
        set_curve_white(&mut self.red, sample[0]);
        set_curve_white(&mut self.green, sample[1]);
        set_curve_white(&mut self.blue, sample[2]);
    }

    fn set_gray_point(&mut self, sample: [f32; 3]) {
        let target = luminance(sample);
        set_curve_gray(&mut self.red, sample[0], target);
        set_curve_gray(&mut self.green, sample[1], target);
        set_curve_gray(&mut self.blue, sample[2], target);
    }
}

/// Photoshop's Levels: input black/white points and midtone gamma, then
/// output range. All 0–1 except gamma (1 = unchanged).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    pub in_black: f32,
    pub in_white: f32,
    pub gamma: f32,
    pub out_black: f32,
    pub out_white: f32,
}

impl Default for Levels {
    fn default() -> Self {
        Self {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        }
    }
}

impl Levels {
    pub fn set_point(&mut self, eyedropper: Eyedropper, sample: [f32; 3]) {
        match eyedropper {
            Eyedropper::Black => self.set_black_point(sample),
            Eyedropper::Gray => self.set_gray_point(sample),
            Eyedropper::White => self.set_white_point(sample),
        }
    }

    fn set_black_point(&mut self, sample: [f32; 3]) {
        let lum = luminance(sample);
        self.in_black = lum.clamp(0.0, (self.in_white - 0.001).max(0.0));
    }

    fn set_white_point(&mut self, sample: [f32; 3]) {
        let lum = luminance(sample);
        self.in_white = lum.clamp((self.in_black + 0.001).min(1.0), 1.0);
    }

    fn set_gray_point(&mut self, sample: [f32; 3]) {
        let lum = luminance(sample);
        let range = (self.in_white - self.in_black).max(1e-4);
        let v_in = ((lum - self.in_black) / range).clamp(1e-4, 1.0 - 1e-4);
        self.gamma = (v_in.ln() / 0.5f32.ln()).clamp(0.1, 9.99);
    }

    fn eval(&self, x: f32) -> f32 {
        let range = (self.in_white - self.in_black).max(1e-4);
        let v = ((x - self.in_black) / range).clamp(0.0, 1.0);
        let v = v.powf(1.0 / self.gamma.max(0.01));
        self.out_black + v * (self.out_white - self.out_black)
    }
}

/// Photoshop's Hue/Saturation (master range): hue in degrees (−180–180),
/// saturation and lightness −100–100.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HueSaturation {
    pub hue: f32,
    pub saturation: f32,
    pub lightness: f32,
}

impl HueSaturation {
    fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let (h, s, l) = rgb_to_hsl(c);
        let h = (h + self.hue / 360.0).rem_euclid(1.0);
        let k = self.saturation / 100.0;
        // Positive saturation pushes towards full colour, negative towards grey.
        let s = if k >= 0.0 {
            s + (1.0 - s) * k * s.min(1.0)
        } else {
            s * (1.0 + k)
        };
        let mut rgb = hsl_to_rgb(h, s.clamp(0.0, 1.0), l);
        let light = self.lightness / 100.0;
        for v in &mut rgb {
            *v = if light >= 0.0 {
                *v + (1.0 - *v) * light
            } else {
                *v * (1.0 + light)
            };
        }
        rgb
    }
}

/// Photoshop's Color Balance: per tonal range, shifts along cyan–red,
/// magenta–green and yellow–blue, each −100–100.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorBalance {
    pub shadows: [f32; 3],
    pub midtones: [f32; 3],
    pub highlights: [f32; 3],
    pub preserve_luminosity: bool,
}

impl Default for ColorBalance {
    fn default() -> Self {
        Self {
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
            preserve_luminosity: true,
        }
    }
}

impl ColorBalance {
    /// Weighting of the three tonal ranges follows GIMP's Color Balance
    /// (gimpoperationcolorbalance.c, GPL-3.0-or-later).
    fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let (_, _, lightness) = rgb_to_hsl(c);
        let (a, b, scale) = (0.25, 0.333, 0.7);
        let shadow_w = ((lightness - b) / -a + 0.5).clamp(0.0, 1.0) * scale;
        let mid_w = ((lightness - b) / a + 0.5).clamp(0.0, 1.0)
            * ((lightness + b - 1.0) / -a + 0.5).clamp(0.0, 1.0)
            * scale;
        let high_w = ((lightness + b - 1.0) / a + 0.5).clamp(0.0, 1.0) * scale;
        let mut out = c;
        for ch in 0..3 {
            let shift = self.shadows[ch] * shadow_w
                + self.midtones[ch] * mid_w
                + self.highlights[ch] * high_w;
            out[ch] = (c[ch] + shift / 100.0).clamp(0.0, 1.0);
        }
        if self.preserve_luminosity {
            let (h, s, _) = rgb_to_hsl(out);
            out = hsl_to_rgb(h, s, lightness);
        }
        out
    }
}

fn rgb_to_hsl([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-7 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [l; 3];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}

/// Photoshop's Selective Color: CMYK adjustments per color range (Reds,
/// Yellows, Greens, Cyans, Blues, Magentas, Whites, Neutrals, Blacks).
/// Sliders are −100–100, and method is Relative (default) or Absolute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectiveColor {
    pub ranges: [[f32; 4]; 9],
    pub relative: bool,
}

impl Default for SelectiveColor {
    fn default() -> Self {
        Self {
            ranges: [[0.0; 4]; 9],
            relative: true,
        }
    }
}

impl SelectiveColor {
    pub const REDS: usize = 0;
    pub const YELLOWS: usize = 1;
    pub const GREENS: usize = 2;
    pub const CYANS: usize = 3;
    pub const BLUES: usize = 4;
    pub const MAGENTAS: usize = 5;
    pub const WHITES: usize = 6;
    pub const NEUTRALS: usize = 7;
    pub const BLACKS: usize = 8;
    pub const RANGE_NAMES: [&'static str; 9] = [
        "Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas", "Whites", "Neutrals", "Blacks",
    ];

    pub fn prepare(&self) -> PreparedSelectiveColor {
        let active = self
            .ranges
            .iter()
            .enumerate()
            .filter(|(_, cmyk)| cmyk.iter().any(|&v| v.abs() > 1e-4))
            .map(|(i, &[c, m, y, k])| (i, [c / 100.0, m / 100.0, y / 100.0, k / 100.0]))
            .collect();
        PreparedSelectiveColor {
            active,
            relative: self.relative,
        }
    }
}

#[derive(Clone)]
pub struct PreparedSelectiveColor {
    active: Vec<(usize, [f32; 4])>,
    relative: bool,
}

impl PreparedSelectiveColor {
    pub fn apply(&self, [r, g, b]: [f32; 3]) -> [f32; 3] {
        if self.active.is_empty() {
            return [r, g, b];
        }
        let min = r.min(g).min(b);
        let max = r.max(g).max(b);
        let med = r + g + b - min - max;

        let mut adj_r = 0.0f32;
        let mut adj_g = 0.0f32;
        let mut adj_b = 0.0f32;

        for &(range, [c, m, y, k]) in &self.active {
            let scale = match range {
                SelectiveColor::REDS if r == max => max - med,
                SelectiveColor::YELLOWS if b == min => med - min,
                SelectiveColor::GREENS if g == max => max - med,
                SelectiveColor::CYANS if r == min => med - min,
                SelectiveColor::BLUES if b == max => max - med,
                SelectiveColor::MAGENTAS if g == min => med - min,
                SelectiveColor::WHITES if r > 0.5 && g > 0.5 && b > 0.5 => (min - 0.5) * 2.0,
                SelectiveColor::NEUTRALS
                    if (r > 0.0 || g > 0.0 || b > 0.0) && (r < 1.0 || g < 1.0 || b < 1.0) =>
                {
                    1.0 - ((max - 0.5).abs() + (min - 0.5).abs())
                }
                SelectiveColor::BLACKS if r < 0.5 && g < 0.5 && b < 0.5 => (0.5 - max) * 2.0,
                _ => 0.0,
            };
            if scale > 0.0 {
                adj_r += comp_adjust(scale, r, c, k, self.relative);
                adj_g += comp_adjust(scale, g, m, k, self.relative);
                adj_b += comp_adjust(scale, b, y, k, self.relative);
            }
        }
        [
            (r + adj_r).clamp(0.0, 1.0),
            (g + adj_g).clamp(0.0, 1.0),
            (b + adj_b).clamp(0.0, 1.0),
        ]
    }
}

#[inline]
fn comp_adjust(scale: f32, value: f32, adjust: f32, k: f32, relative: bool) -> f32 {
    let min = -value;
    let max = 1.0 - value;
    let mut res = (-1.0 - adjust) * k - adjust;
    if relative {
        res *= max;
    }
    res.clamp(min, max) * scale
}

/// Photoshop's Channel Mixer: linear combinations of color channels.
/// Red, Green, Blue, Constant each −200–200%.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelMixer {
    pub red: [f32; 4],
    pub green: [f32; 4],
    pub blue: [f32; 4],
    pub gray: [f32; 4],
    pub monochrome: bool,
}

impl Default for ChannelMixer {
    fn default() -> Self {
        Self {
            red: [100.0, 0.0, 0.0, 0.0],
            green: [0.0, 100.0, 0.0, 0.0],
            blue: [0.0, 0.0, 100.0, 0.0],
            gray: [40.0, 40.0, 20.0, 0.0],
            monochrome: false,
        }
    }
}

impl ChannelMixer {
    pub fn prepare(&self) -> PreparedChannelMixer {
        let div = |a: [f32; 4]| [a[0] / 100.0, a[1] / 100.0, a[2] / 100.0, a[3] / 100.0];
        PreparedChannelMixer {
            red: div(self.red),
            green: div(self.green),
            blue: div(self.blue),
            gray: div(self.gray),
            monochrome: self.monochrome,
        }
    }
}

#[derive(Clone)]
pub struct PreparedChannelMixer {
    red: [f32; 4],
    green: [f32; 4],
    blue: [f32; 4],
    gray: [f32; 4],
    monochrome: bool,
}

impl PreparedChannelMixer {
    pub fn apply(&self, [r, g, b]: [f32; 3]) -> [f32; 3] {
        if self.monochrome {
            let [mr, mg, mb, mc] = self.gray;
            let v = (r * mr + g * mg + b * mb + mc).clamp(0.0, 1.0);
            [v, v, v]
        } else {
            let [rr, rg, rb, rc] = self.red;
            let [gr, gg, gb, gc] = self.green;
            let [br, bg, bb, bc] = self.blue;
            [
                (r * rr + g * rg + b * rb + rc).clamp(0.0, 1.0),
                (r * gr + g * gg + b * gb + gc).clamp(0.0, 1.0),
                (r * br + g * bg + b * bb + bc).clamp(0.0, 1.0),
            ]
        }
    }
}

/// A 1D or 3D colour lookup table (LUT), loaded from a .cube file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorLookup {
    /// LUT name or title from the file or file stem.
    pub title: String,
    /// Input domain min (default [0, 0, 0]).
    pub domain_min: [f32; 3],
    /// Input domain max (default [1, 1, 1]).
    pub domain_max: [f32; 3],
    /// Grid size along each dimension (e.g. 33 for 33×33×33 3D LUT, or 1024 for 1D LUT).
    pub size: usize,
    /// True for 3D LUT, false for 1D LUT.
    pub is_3d: bool,
    /// Flattened table of RGB output values. Empty if no LUT is loaded.
    pub table: Vec<[f32; 3]>,
}

impl Default for ColorLookup {
    fn default() -> Self {
        Self {
            title: String::new(),
            domain_min: [0.0, 0.0, 0.0],
            domain_max: [1.0, 1.0, 1.0],
            size: 0,
            is_3d: true,
            table: Vec::new(),
        }
    }
}

impl ColorLookup {
    pub fn is_empty(&self) -> bool {
        self.size == 0 || self.table.is_empty()
    }

    /// Load a LUT from the contents of a .cube file.
    pub fn load_cube_str(&mut self, content: &str, title_fallback: &str) -> Result<(), String> {
        let parsed = parse_cube(content, title_fallback)?;
        *self = parsed;
        Ok(())
    }

    /// Load a LUT from a .cube file on disk.
    pub fn load_cube_file<P: AsRef<std::path::Path>>(&mut self, path: P) -> Result<(), String> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let fallback = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "LUT".into());
        self.load_cube_str(&content, &fallback)
    }

    /// Parse a LUT from .cube file content.
    pub fn from_cube_str(content: &str, title_fallback: &str) -> Result<Self, String> {
        parse_cube(content, title_fallback)
    }

    /// The LUT as a .cube file (Adobe's format, which `parse_cube` reads).
    pub fn to_cube(&self) -> String {
        let kind = if self.is_3d { "3D" } else { "1D" };
        let [r0, g0, b0] = self.domain_min;
        let [r1, g1, b1] = self.domain_max;
        let mut cube = format!(
            "TITLE \"{}\"\nLUT_{kind}_SIZE {}\nDOMAIN_MIN {r0} {g0} {b0}\nDOMAIN_MAX {r1} {g1} {b1}\n",
            self.title.replace('"', "'"),
            self.size,
        );
        for [r, g, b] in &self.table {
            cube += &format!("{r:.6} {g:.6} {b:.6}\n");
        }
        cube
    }

    pub fn prepare(&self) -> PreparedColorLookup {
        if self.is_empty() {
            return PreparedColorLookup::Identity;
        }
        let is_default_domain = self.domain_min == [0.0; 3] && self.domain_max == [1.0; 3];
        let mut inv_domain = [1.0f32; 3];
        for (i, inv) in inv_domain.iter_mut().enumerate() {
            let range = self.domain_max[i] - self.domain_min[i];
            *inv = if range.abs() > 1e-6 { 1.0 / range } else { 0.0 };
        }
        let table = Arc::new(self.table.clone());
        if self.is_3d {
            PreparedColorLookup::Lut3D {
                size: self.size,
                is_default_domain,
                domain_min: self.domain_min,
                inv_domain,
                table,
            }
        } else {
            PreparedColorLookup::Lut1D {
                size: self.size,
                is_default_domain,
                domain_min: self.domain_min,
                inv_domain,
                table,
            }
        }
    }
}

pub fn parse_cube(content: &str, title_fallback: &str) -> Result<ColorLookup, String> {
    let mut title = None;
    let mut size_1d = None;
    let mut size_3d = None;
    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut table = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }

        if let Some(rest) = line.strip_prefix("TITLE") {
            let t = rest.trim().trim_matches('"').trim();
            if !t.is_empty() {
                title = Some(t.to_string());
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("LUT_3D_SIZE") {
            let s = rest
                .split_whitespace()
                .next()
                .ok_or_else(|| "LUT_3D_SIZE missing value".to_string())?
                .parse::<usize>()
                .map_err(|e| format!("Invalid LUT_3D_SIZE: {e}"))?;
            if !(2..=256).contains(&s) {
                return Err(format!("LUT_3D_SIZE {s} out of supported range (2..=256)"));
            }
            size_3d = Some(s);
            continue;
        }

        if let Some(rest) = line.strip_prefix("LUT_1D_SIZE") {
            let s = rest
                .split_whitespace()
                .next()
                .ok_or_else(|| "LUT_1D_SIZE missing value".to_string())?
                .parse::<usize>()
                .map_err(|e| format!("Invalid LUT_1D_SIZE: {e}"))?;
            if !(2..=65536).contains(&s) {
                return Err(format!("LUT_1D_SIZE {s} out of supported range (2..=65536)"));
            }
            size_1d = Some(s);
            continue;
        }

        if let Some(rest) = line.strip_prefix("DOMAIN_MIN") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 3 {
                let r = parts[0].parse::<f32>().map_err(|e| format!("DOMAIN_MIN r: {e}"))?;
                let g = parts[1].parse::<f32>().map_err(|e| format!("DOMAIN_MIN g: {e}"))?;
                let b = parts[2].parse::<f32>().map_err(|e| format!("DOMAIN_MIN b: {e}"))?;
                domain_min = [r, g, b];
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("DOMAIN_MAX") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 3 {
                let r = parts[0].parse::<f32>().map_err(|e| format!("DOMAIN_MAX r: {e}"))?;
                let g = parts[1].parse::<f32>().map_err(|e| format!("DOMAIN_MAX g: {e}"))?;
                let b = parts[2].parse::<f32>().map_err(|e| format!("DOMAIN_MAX b: {e}"))?;
                domain_max = [r, g, b];
            }
            continue;
        }

        let mut tokens = line.split_whitespace();
        let tok1 = match tokens.next() {
            Some(t) => t,
            None => continue,
        };
        let first_char = tok1.chars().next().unwrap_or(' ');
        if !first_char.is_ascii_digit() && first_char != '-' && first_char != '+' && first_char != '.' {
            continue;
        }

        let r = match tok1.parse::<f32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tok2 = match tokens.next() {
            Some(t) => t,
            None => continue,
        };
        let g = match tok2.parse::<f32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tok3 = match tokens.next() {
            Some(t) => t,
            None => continue,
        };
        let b = match tok3.parse::<f32>() {
            Ok(v) => v,
            Err(_) => continue,
        };

        table.push([r, g, b]);
    }

    let is_3d = match (size_3d, size_1d) {
        (Some(s3), _) => {
            let expected = s3 * s3 * s3;
            if table.len() != expected {
                return Err(format!(
                    "Expected {expected} data points for 3D LUT of size {s3}, found {}",
                    table.len()
                ));
            }
            true
        }
        (None, Some(s1)) => {
            if table.len() != s1 {
                return Err(format!(
                    "Expected {s1} data points for 1D LUT of size {s1}, found {}",
                    table.len()
                ));
            }
            false
        }
        (None, None) => {
            let len = table.len();
            let cbrt = (len as f64).cbrt().round() as usize;
            if cbrt >= 2 && cbrt * cbrt * cbrt == len {
                size_3d = Some(cbrt);
                true
            } else if len >= 2 {
                size_1d = Some(len);
                false
            } else {
                return Err("No LUT size specified in .cube file".to_string());
            }
        }
    };

    let size = if is_3d {
        size_3d.unwrap()
    } else {
        size_1d.unwrap()
    };

    Ok(ColorLookup {
        title: title.unwrap_or_else(|| title_fallback.to_string()),
        domain_min,
        domain_max,
        size,
        is_3d,
        table,
    })
}

#[derive(Clone)]
pub enum PreparedColorLookup {
    Identity,
    Lut1D {
        size: usize,
        is_default_domain: bool,
        domain_min: [f32; 3],
        inv_domain: [f32; 3],
        table: Arc<Vec<[f32; 3]>>,
    },
    Lut3D {
        size: usize,
        is_default_domain: bool,
        domain_min: [f32; 3],
        inv_domain: [f32; 3],
        table: Arc<Vec<[f32; 3]>>,
    },
}

impl PreparedColorLookup {
    #[inline]
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Identity => c,
            Self::Lut1D {
                size,
                is_default_domain,
                domain_min,
                inv_domain,
                table,
            } => {
                if *size < 2 || table.len() < *size {
                    return c;
                }
                let norm = if *is_default_domain {
                    c
                } else {
                    [
                        ((c[0] - domain_min[0]) * inv_domain[0]).clamp(0.0, 1.0),
                        ((c[1] - domain_min[1]) * inv_domain[1]).clamp(0.0, 1.0),
                        ((c[2] - domain_min[2]) * inv_domain[2]).clamp(0.0, 1.0),
                    ]
                };
                let n_minus_1 = (*size - 1) as f32;
                let max_idx = *size - 2;
                let mut out = [0.0f32; 3];
                for ch in 0..3 {
                    let pos = norm[ch].clamp(0.0, 1.0) * n_minus_1;
                    let i = (pos as usize).min(max_idx);
                    let t = pos - i as f32;
                    let v0 = table[i][ch];
                    let v1 = table[i + 1][ch];
                    out[ch] = (v0 + (v1 - v0) * t).clamp(0.0, 1.0);
                }
                out
            }
            Self::Lut3D {
                size,
                is_default_domain,
                domain_min,
                inv_domain,
                table,
            } => {
                let s = *size;
                if s < 2 || table.len() < s * s * s {
                    return c;
                }
                let norm = if *is_default_domain {
                    c
                } else {
                    [
                        ((c[0] - domain_min[0]) * inv_domain[0]).clamp(0.0, 1.0),
                        ((c[1] - domain_min[1]) * inv_domain[1]).clamp(0.0, 1.0),
                        ((c[2] - domain_min[2]) * inv_domain[2]).clamp(0.0, 1.0),
                    ]
                };
                let n_minus_1 = (s - 1) as f32;
                let x = norm[0].clamp(0.0, 1.0) * n_minus_1;
                let y = norm[1].clamp(0.0, 1.0) * n_minus_1;
                let z = norm[2].clamp(0.0, 1.0) * n_minus_1;

                let max_idx = s - 2;
                let i0 = (x as usize).min(max_idx);
                let j0 = (y as usize).min(max_idx);
                let k0 = (z as usize).min(max_idx);

                let fx = x - i0 as f32;
                let fy = y - j0 as f32;
                let fz = z - k0 as f32;

                let stride_g = s;
                let stride_b = s * s;

                let b0 = k0 * stride_b;
                let b1 = (k0 + 1) * stride_b;

                let g0_b0 = j0 * stride_g + b0;
                let g1_b0 = (j0 + 1) * stride_g + b0;
                let g0_b1 = j0 * stride_g + b1;
                let g1_b1 = (j0 + 1) * stride_g + b1;

                let i1 = i0 + 1;

                let c000 = table[i0 + g0_b0];
                let c100 = table[i1 + g0_b0];
                let c010 = table[i0 + g1_b0];
                let c110 = table[i1 + g1_b0];
                let c001 = table[i0 + g0_b1];
                let c101 = table[i1 + g0_b1];
                let c011 = table[i0 + g1_b1];
                let c111 = table[i1 + g1_b1];

                let mut out = [0.0f32; 3];
                for ch in 0..3 {
                    let c00 = c000[ch] + (c100[ch] - c000[ch]) * fx;
                    let c10 = c010[ch] + (c110[ch] - c010[ch]) * fx;
                    let c01 = c001[ch] + (c101[ch] - c001[ch]) * fx;
                    let c11 = c011[ch] + (c111[ch] - c011[ch]) * fx;

                    let c0 = c00 + (c10 - c00) * fy;
                    let c1 = c01 + (c11 - c01) * fy;

                    out[ch] = (c0 + (c1 - c0) * fz).clamp(0.0, 1.0);
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lut_written_as_cube_reads_back_the_same() {
        let cube = "TITLE \"Warm\"\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n0.5 0.25 1\n";
        let lut = ColorLookup::from_cube_str(cube, "x").unwrap();
        assert_eq!(ColorLookup::from_cube_str(&lut.to_cube(), "y").unwrap(), lut);
    }

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < tol)
    }

    #[test]
    fn defaults_change_nothing() {
        let samples = [
            [0.0, 0.0, 0.0],
            [0.2, 0.5, 0.9],
            [1.0, 0.3, 0.3],
            [1.0, 1.0, 1.0],
        ];
        for adj in [
            Adjustment::Curves(Curves::default()),
            Adjustment::Levels(Levels::default()),
            Adjustment::HueSaturation(HueSaturation::default()),
            Adjustment::ColorBalance(ColorBalance::default()),
            Adjustment::SelectiveColor(SelectiveColor::default()),
            Adjustment::ChannelMixer(ChannelMixer::default()),
            Adjustment::ColorLookup(ColorLookup::default()),
        ] {
            let p = adj.prepare();
            for c in samples {
                assert!(
                    close(p.apply(c), c, 2e-3),
                    "{} changed {c:?} to {:?}",
                    adj.name(),
                    p.apply(c)
                );
            }
        }
    }

    #[test]
    fn curve_passes_through_points_without_overshoot() {
        let c = Curve {
            points: vec![(0.0, 0.0), (0.25, 0.4), (0.5, 0.45), (1.0, 1.0)],
        };
        assert!((c.eval(0.25) - 0.4).abs() < 1e-5);
        assert!((c.eval(0.5) - 0.45).abs() < 1e-5);
        // Monotone between the flat-ish middle points.
        let mut last = 0.0;
        for i in 0..=100 {
            let v = c.eval(i as f32 / 100.0);
            assert!(v >= last - 1e-6, "not monotone at {i}");
            last = v;
        }
    }

    #[test]
    fn s_curve_adds_contrast_and_channel_curves_tint() {
        let mut curves = Curves::default();
        curves.master.points = vec![(0.0, 0.0), (0.25, 0.18), (0.75, 0.82), (1.0, 1.0)];
        curves.blue.points = vec![(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)];
        let p = Adjustment::Curves(curves).prepare();
        let dark = p.apply([0.25; 3]);
        assert!(dark[0] < 0.2);
        let mid = p.apply([0.5; 3]);
        assert!(
            mid[2] > mid[0] + 0.05,
            "blue curve should warm-cool: {mid:?}"
        );
    }

    #[test]
    fn levels_remap_and_gamma() {
        let l = Levels {
            in_black: 0.1,
            in_white: 0.9,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        };
        let p = Adjustment::Levels(l).prepare();
        assert!(p.apply([0.1; 3])[0].abs() < 1e-3);
        assert!((p.apply([0.5; 3])[0] - 0.5).abs() < 1e-3);
        let bright = Adjustment::Levels(Levels {
            gamma: 2.0,
            ..Levels::default()
        })
        .prepare();
        assert!(bright.apply([0.25; 3])[0] > 0.45);
    }

    #[test]
    fn hue_saturation_desaturates_and_rotates() {
        let grey = Adjustment::HueSaturation(HueSaturation {
            saturation: -100.0,
            ..Default::default()
        })
        .prepare();
        let g = grey.apply([0.8, 0.2, 0.2]);
        assert!((g[0] - g[1]).abs() < 1e-4 && (g[1] - g[2]).abs() < 1e-4);
        let rotate = Adjustment::HueSaturation(HueSaturation {
            hue: 120.0,
            ..Default::default()
        })
        .prepare();
        let r = rotate.apply([0.8, 0.2, 0.2]);
        assert!(
            r[1] > r[0] && r[1] > r[2],
            "red rotated 120° should be green: {r:?}"
        );
    }

    #[test]
    fn color_balance_warms_midtones_keeping_lightness() {
        let b = ColorBalance {
            midtones: [30.0, 0.0, -30.0],
            ..Default::default()
        };
        let p = Adjustment::ColorBalance(b).prepare();
        let out = p.apply([0.5, 0.5, 0.5]);
        assert!(out[0] > out[2]);
        let (_, _, l) = rgb_to_hsl(out);
        assert!((l - 0.5).abs() < 1e-3);
    }

    #[test]
    fn selective_color_adjusts_targeted_range() {
        // Red pixel: [0.8, 0.2, 0.1]. Adding 50% cyan to Reds should decrease red.
        let mut sc = SelectiveColor::default();
        sc.ranges[SelectiveColor::REDS][0] = 50.0;
        let p = Adjustment::SelectiveColor(sc.clone()).prepare();
        let out = p.apply([0.8, 0.2, 0.1]);
        assert!(out[0] < 0.8, "red component should decrease: {out:?}");
        assert_eq!(out[1], 0.2);
        assert_eq!(out[2], 0.1);

        // In relative mode, 100% red [1.0, 0.0, 0.0] has cyan = 0, so adding cyan adds 0.
        let out_rel = p.apply([1.0, 0.0, 0.0]);
        assert_eq!(out_rel, [1.0, 0.0, 0.0]);

        // In absolute mode, adding cyan to [1.0, 0.0, 0.0] reduces red.
        sc.relative = false;
        let p_abs = Adjustment::SelectiveColor(sc).prepare();
        let out_abs = p_abs.apply([1.0, 0.0, 0.0]);
        assert!(out_abs[0] < 1.0, "absolute mode should reduce pure red: {out_abs:?}");

        // Neutral pixel: [0.5, 0.5, 0.5]. Adding black to Neutrals darkens all channels.
        let mut sc_neutral = SelectiveColor::default();
        sc_neutral.ranges[SelectiveColor::NEUTRALS][3] = 40.0;
        let p_neu = Adjustment::SelectiveColor(sc_neutral).prepare();
        let out_neu = p_neu.apply([0.5, 0.5, 0.5]);
        assert!(out_neu[0] < 0.5 && out_neu[1] < 0.5 && out_neu[2] < 0.5);
    }

    #[test]
    fn channel_mixer_mixes_channels_and_monochrome() {
        // Swap red and green channels
        let mixer = ChannelMixer {
            red: [0.0, 100.0, 0.0, 0.0],
            green: [100.0, 0.0, 0.0, 0.0],
            blue: [0.0, 0.0, 100.0, 0.0],
            ..Default::default()
        };
        let p = Adjustment::ChannelMixer(mixer).prepare();
        let out = p.apply([0.8, 0.3, 0.1]);
        assert!(close(out, [0.3, 0.8, 0.1], 1e-4));

        // Constant offsets channel
        let with_const = ChannelMixer {
            blue: [0.0, 0.0, 100.0, 20.0],
            ..Default::default()
        };
        let p_const = Adjustment::ChannelMixer(with_const).prepare();
        let out_const = p_const.apply([0.5, 0.5, 0.5]);
        assert!(close(out_const, [0.5, 0.5, 0.7], 1e-4));

        // Monochrome mode produces equal R, G, B channels
        let mono = ChannelMixer {
            monochrome: true,
            gray: [40.0, 40.0, 20.0, 0.0],
            ..Default::default()
        };
        let p_mono = Adjustment::ChannelMixer(mono).prepare();
        let out_mono = p_mono.apply([1.0, 0.5, 0.0]);
        // 1.0 * 0.4 + 0.5 * 0.4 + 0.0 * 0.2 = 0.6
        assert!(close(out_mono, [0.6, 0.6, 0.6], 1e-4));
    }

    #[test]
    fn json_round_trip() {
        let mut c = Curves::default();
        c.red.points.insert(1, (0.3, 0.35));
        let adj = Adjustment::Curves(c);
        assert_eq!(Adjustment::from_json(&adj.to_json()), Some(adj));

        let mut sc = SelectiveColor::default();
        sc.ranges[SelectiveColor::REDS] = [10.0, -20.0, 30.0, -40.0];
        sc.relative = false;
        let adj_sc = Adjustment::SelectiveColor(sc);
        assert_eq!(Adjustment::from_json(&adj_sc.to_json()), Some(adj_sc));

        let cm = ChannelMixer {
            monochrome: true,
            gray: [30.0, 50.0, 20.0, 5.0],
            ..Default::default()
        };
        let adj_cm = Adjustment::ChannelMixer(cm);
        assert_eq!(Adjustment::from_json(&adj_cm.to_json()), Some(adj_cm));

        let cube = "TITLE \"Test\"\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let lut = ColorLookup::from_cube_str(cube, "fallback").unwrap();
        let adj_lut = Adjustment::ColorLookup(lut);
        assert_eq!(Adjustment::from_json(&adj_lut.to_json()), Some(adj_lut));
    }

    #[test]
    fn color_lookup_3d_identity_and_grading() {
        // Identity 2x2x2 cube
        let id_cube = r#"
            # Comment
            TITLE "Identity 2x2x2"
            LUT_3D_SIZE 2
            DOMAIN_MIN 0.0 0.0 0.0
            DOMAIN_MAX 1.0 1.0 1.0

            0.0 0.0 0.0
            1.0 0.0 0.0
            0.0 1.0 0.0
            1.0 1.0 0.0
            0.0 0.0 1.0
            1.0 0.0 1.0
            0.0 1.0 1.0
            1.0 1.0 1.0
        "#;
        let lut = ColorLookup::from_cube_str(id_cube, "fallback").unwrap();
        assert_eq!(lut.title, "Identity 2x2x2");
        assert_eq!(lut.size, 2);
        assert!(lut.is_3d);
        let p = Adjustment::ColorLookup(lut).prepare();

        let samples = [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.25, 0.5, 0.75],
            [0.1, 0.9, 0.3],
        ];
        for c in samples {
            assert!(
                close(p.apply(c), c, 1e-5),
                "identity 3D LUT changed {c:?} to {:?}",
                p.apply(c)
            );
        }

        // Invert-red 2x2x2 cube
        let invert_r_cube = r#"
            LUT_3D_SIZE 2
            1.0 0.0 0.0
            0.0 0.0 0.0
            1.0 1.0 0.0
            0.0 1.0 0.0
            1.0 0.0 1.0
            0.0 0.0 1.0
            1.0 1.0 1.0
            0.0 1.0 1.0
        "#;
        let lut_inv = ColorLookup::from_cube_str(invert_r_cube, "my_lut").unwrap();
        assert_eq!(lut_inv.title, "my_lut");
        let p_inv = Adjustment::ColorLookup(lut_inv).prepare();
        let out = p_inv.apply([0.2, 0.4, 0.8]);
        assert!(close(out, [0.8, 0.4, 0.8], 1e-4), "expected [0.8, 0.4, 0.8], got {out:?}");
    }

    #[test]
    fn color_lookup_1d_interpolation() {
        let cube_1d = r#"
            TITLE "Invert 1D"
            LUT_1D_SIZE 2
            1.0 1.0 1.0
            0.0 0.0 0.0
        "#;
        let lut = ColorLookup::from_cube_str(cube_1d, "").unwrap();
        assert!(!lut.is_3d);
        assert_eq!(lut.size, 2);
        let p = Adjustment::ColorLookup(lut).prepare();
        let out = p.apply([0.25, 0.7, 1.0]);
        assert!(close(out, [0.75, 0.3, 0.0], 1e-4));
    }

    #[test]
    fn color_lookup_errors() {
        assert!(ColorLookup::from_cube_str("LUT_3D_SIZE 2\n0 0 0\n", "").is_err());
        assert!(ColorLookup::from_cube_str("LUT_1D_SIZE 3\n0 0 0\n1 1 1\n", "").is_err());
        assert!(ColorLookup::from_cube_str("random text with no numbers", "").is_err());
    }

    #[test]
    fn curves_eyedroppers_set_black_white_gray() {
        let mut curves = Curves::default();
        let black_sample = [0.15, 0.12, 0.18];
        curves.set_black_point(black_sample);
        let p = Adjustment::Curves(curves.clone()).prepare();
        assert!(close(p.apply(black_sample), [0.0, 0.0, 0.0], 2e-3));

        let white_sample = [0.85, 0.90, 0.88];
        curves.set_white_point(white_sample);
        let p = Adjustment::Curves(curves.clone()).prepare();
        assert!(close(p.apply(white_sample), [1.0, 1.0, 1.0], 2e-3));
        assert!(close(p.apply(black_sample), [0.0, 0.0, 0.0], 2e-3));

        let gray_sample = [0.55, 0.48, 0.52];
        curves.set_gray_point(gray_sample);
        let p = Adjustment::Curves(curves.clone()).prepare();
        let res = p.apply(gray_sample);
        let lum = luminance(gray_sample);
        assert!(close(res, [lum, lum, lum], 2e-3));
        assert!(close(p.apply(black_sample), [0.0, 0.0, 0.0], 2e-3));
        assert!(close(p.apply(white_sample), [1.0, 1.0, 1.0], 2e-3));
    }

    #[test]
    fn levels_eyedroppers_set_black_white_gray() {
        let mut levels = Levels::default();
        let black_sample = [0.12, 0.12, 0.12];
        levels.set_black_point(black_sample);
        let p = Adjustment::Levels(levels.clone()).prepare();
        assert!(close(p.apply(black_sample), [0.0, 0.0, 0.0], 2e-3));

        let white_sample = [0.88, 0.88, 0.88];
        levels.set_white_point(white_sample);
        let p = Adjustment::Levels(levels.clone()).prepare();
        assert!(close(p.apply(white_sample), [1.0, 1.0, 1.0], 2e-3));
        assert!(close(p.apply(black_sample), [0.0, 0.0, 0.0], 2e-3));

        let gray_sample = [0.45, 0.45, 0.45];
        levels.set_gray_point(gray_sample);
        let p = Adjustment::Levels(levels.clone()).prepare();
        assert!(close(p.apply(gray_sample), [0.5, 0.5, 0.5], 2e-3));
        assert!(close(p.apply(black_sample), [0.0, 0.0, 0.0], 2e-3));
        assert!(close(p.apply(white_sample), [1.0, 1.0, 1.0], 2e-3));
    }

    #[test]
    fn lut_samples_the_adjustment_on_a_grid() {
        let mut curves = Curves::default();
        curves.master.points.insert(1, (0.5, 0.7));
        curves.red.points.insert(1, (0.25, 0.1));
        let prepared = Adjustment::Curves(curves).prepare();
        let n = 9;
        let lut = prepared.lut(n);
        assert_eq!(lut.len(), n * n * n);
        let at = |r: usize, g: usize, b: usize| lut[r + g * n + b * n * n];
        let step = 1.0 / (n - 1) as f32;
        for (r, g, b) in [(0, 0, 0), (8, 8, 8), (2, 4, 6), (7, 1, 3)] {
            let c = [r as f32 * step, g as f32 * step, b as f32 * step];
            assert!(close(at(r, g, b), prepared.apply(c), 1e-6));
        }
    }
}
