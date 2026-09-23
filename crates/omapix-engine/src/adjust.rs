//! Adjustment layers: colour and tone changes applied to everything below,
//! editable at any time, like Photoshop's.
//!
//! Each adjustment is a function from a colour to a colour (0–1 values in
//! the document's space). [`Adjustment::prepare`] turns the settings into a
//! fast form (lookup tables where possible) once per composite.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Adjustment {
    Curves(Curves),
    Levels(Levels),
    HueSaturation(HueSaturation),
    ColorBalance(ColorBalance),
}

impl Adjustment {
    pub fn name(&self) -> &'static str {
        match self {
            Adjustment::Curves(_) => "Curves",
            Adjustment::Levels(_) => "Levels",
            Adjustment::HueSaturation(_) => "Hue/Saturation",
            Adjustment::ColorBalance(_) => "Color Balance",
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
}

impl Prepared {
    #[inline]
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        match self {
            Prepared::Luts([r, g, b]) => [r.get(c[0]), g.get(c[1]), b.get(c[2])],
            Prepared::HueSaturation(h) => h.apply(c),
            Prepared::ColorBalance(b) => b.apply(c),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn json_round_trip() {
        let mut c = Curves::default();
        c.red.points.insert(1, (0.3, 0.35));
        let adj = Adjustment::Curves(c);
        assert_eq!(Adjustment::from_json(&adj.to_json()), Some(adj));
    }
}
