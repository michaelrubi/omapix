//! Layer blend modes.
//!
//! Formulas follow Photoshop where it differs from the W3C compositing spec
//! (notably Soft Light), and GIMP/Krita for Grain Extract and Grain Merge,
//! which Photoshop lacks but frequency separation relies on. All maths is
//! on 0–1 values in the document's own (gamma-encoded or linear) space, as
//! in Photoshop.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    GrainExtract,
    GrainMerge,
    Hue,
    Saturation,
    Color,
    Luminosity,
    /// Groups only: what's in the group blends straight onto the layers
    /// below, as if it weren't grouped (Photoshop's default for groups).
    PassThrough,
}

impl BlendMode {
    /// In Photoshop's menu order, with separators between groups.
    pub const MENU: &[&[BlendMode]] = &[
        &[BlendMode::Normal],
        &[
            BlendMode::Darken,
            BlendMode::Multiply,
            BlendMode::ColorBurn,
            BlendMode::LinearBurn,
        ],
        &[
            BlendMode::Lighten,
            BlendMode::Screen,
            BlendMode::ColorDodge,
            BlendMode::LinearDodge,
        ],
        &[
            BlendMode::Overlay,
            BlendMode::SoftLight,
            BlendMode::HardLight,
            BlendMode::VividLight,
            BlendMode::LinearLight,
            BlendMode::PinLight,
        ],
        &[
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Subtract,
            BlendMode::Divide,
        ],
        &[BlendMode::GrainExtract, BlendMode::GrainMerge],
        &[
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ],
    ];

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Darken => "Darken",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::LinearBurn => "Linear Burn",
            BlendMode::Lighten => "Lighten",
            BlendMode::Screen => "Screen",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::LinearDodge => "Linear Dodge (Add)",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::VividLight => "Vivid Light",
            BlendMode::LinearLight => "Linear Light",
            BlendMode::PinLight => "Pin Light",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Subtract => "Subtract",
            BlendMode::Divide => "Divide",
            BlendMode::GrainExtract => "Grain Extract",
            BlendMode::GrainMerge => "Grain Merge",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
            BlendMode::PassThrough => "Pass Through",
        }
    }

    /// Name in OpenRaster files, matching what Krita writes so layered
    /// files open with the right modes there.
    pub fn ora_name(self) -> &'static str {
        match self {
            BlendMode::Normal => "svg:src-over",
            BlendMode::Darken => "svg:darken",
            BlendMode::Multiply => "svg:multiply",
            BlendMode::ColorBurn => "svg:color-burn",
            BlendMode::LinearBurn => "krita:linear_burn",
            BlendMode::Lighten => "svg:lighten",
            BlendMode::Screen => "svg:screen",
            BlendMode::ColorDodge => "svg:color-dodge",
            BlendMode::LinearDodge => "svg:plus",
            BlendMode::Overlay => "svg:overlay",
            BlendMode::SoftLight => "svg:soft-light",
            BlendMode::HardLight => "svg:hard-light",
            BlendMode::VividLight => "krita:vivid_light",
            BlendMode::LinearLight => "krita:linear light",
            BlendMode::PinLight => "krita:pin_light",
            BlendMode::Difference => "svg:difference",
            BlendMode::Exclusion => "krita:exclusion",
            BlendMode::Subtract => "krita:subtract",
            BlendMode::Divide => "krita:divide",
            BlendMode::GrainExtract => "krita:grain_extract",
            BlendMode::GrainMerge => "krita:grain_merge",
            BlendMode::Hue => "svg:hue",
            BlendMode::Saturation => "svg:saturation",
            BlendMode::Color => "svg:color",
            BlendMode::Luminosity => "svg:luminosity",
            // Stored as a stack with `isolation="auto"`.
            BlendMode::PassThrough => "svg:src-over",
        }
    }

    pub fn from_ora_name(name: &str) -> Option<Self> {
        if name == "svg:add" {
            return Some(BlendMode::LinearDodge);
        }
        Self::MENU
            .iter()
            .flat_map(|g| g.iter())
            .copied()
            .find(|m| m.ora_name() == name)
    }

    /// Blend a source colour onto a backdrop colour, both 0–1.
    #[inline]
    pub fn apply(self, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
        let sep = |f: fn(f32, f32) -> f32| [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])];
        match self {
            BlendMode::Normal | BlendMode::PassThrough => cs,
            BlendMode::Darken => sep(f32::min),
            BlendMode::Multiply => sep(|b, s| b * s),
            BlendMode::ColorBurn => sep(color_burn),
            BlendMode::LinearBurn => sep(|b, s| (b + s - 1.0).max(0.0)),
            BlendMode::Lighten => sep(f32::max),
            BlendMode::Screen => sep(screen),
            BlendMode::ColorDodge => sep(color_dodge),
            BlendMode::LinearDodge => sep(|b, s| (b + s).min(1.0)),
            BlendMode::Overlay => sep(|b, s| hard_light(s, b)),
            BlendMode::SoftLight => sep(soft_light),
            BlendMode::HardLight => sep(hard_light),
            BlendMode::VividLight => sep(|b, s| {
                if s <= 0.5 {
                    color_burn(b, 2.0 * s)
                } else {
                    color_dodge(b, 2.0 * s - 1.0)
                }
            }),
            BlendMode::LinearLight => sep(|b, s| (b + 2.0 * s - 1.0).clamp(0.0, 1.0)),
            BlendMode::PinLight => sep(|b, s| {
                if s <= 0.5 {
                    b.min(2.0 * s)
                } else {
                    b.max(2.0 * s - 1.0)
                }
            }),
            BlendMode::Difference => sep(|b, s| (b - s).abs()),
            BlendMode::Exclusion => sep(|b, s| b + s - 2.0 * b * s),
            BlendMode::Subtract => sep(|b, s| (b - s).max(0.0)),
            BlendMode::Divide => sep(|b, s| {
                if s <= 0.0 {
                    if b <= 0.0 { 0.0 } else { 1.0 }
                } else {
                    (b / s).min(1.0)
                }
            }),
            BlendMode::GrainExtract => sep(|b, s| (b - s + 0.5).clamp(0.0, 1.0)),
            BlendMode::GrainMerge => sep(|b, s| (b + s - 0.5).clamp(0.0, 1.0)),
            BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
            BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
            BlendMode::Color => set_lum(cs, lum(cb)),
            BlendMode::Luminosity => set_lum(cb, lum(cs)),
        }
    }
}

fn screen(b: f32, s: f32) -> f32 {
    b + s - b * s
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        screen(b, 2.0 * s - 1.0)
    }
}

fn color_dodge(b: f32, s: f32) -> f32 {
    if b <= 0.0 {
        0.0
    } else if s >= 1.0 {
        1.0
    } else {
        (b / (1.0 - s)).min(1.0)
    }
}

fn color_burn(b: f32, s: f32) -> f32 {
    if b >= 1.0 {
        1.0
    } else if s <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - b) / s).min(1.0)
    }
}

/// Photoshop's Soft Light. 50 % grey leaves the backdrop unchanged, which
/// is what makes grey-filled dodge & burn layers work.
fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        2.0 * b * s + b * b * (1.0 - 2.0 * s)
    } else {
        2.0 * b * (1.0 - s) + b.sqrt() * (2.0 * s - 1.0)
    }
}

// Non-separable modes, from the W3C compositing spec (which follows Photoshop).

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        out = out.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    out
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    if max <= min {
        return [0.0; 3];
    }
    c.map(|v| (v - min) * s / (max - min))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(mode: BlendMode, b: f32, s: f32) -> f32 {
        mode.apply([b; 3], [s; 3])[0]
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn neutral_colours_leave_backdrop_unchanged() {
        for b in [0.0, 0.2, 0.5, 0.8, 1.0] {
            for (mode, neutral) in [
                (BlendMode::Multiply, 1.0),
                (BlendMode::Screen, 0.0),
                (BlendMode::Overlay, 0.5),
                (BlendMode::SoftLight, 0.5),
                (BlendMode::HardLight, 0.5),
                (BlendMode::LinearLight, 0.5),
                (BlendMode::GrainMerge, 0.5),
                (BlendMode::LinearDodge, 0.0),
                (BlendMode::Difference, 0.0),
            ] {
                assert!(close(one(mode, b, neutral), b), "{mode:?} b={b}");
            }
        }
    }

    #[test]
    fn grain_extract_then_merge_reconstructs() {
        // Frequency separation: high = image - low + 0.5, image = low + high - 0.5.
        for (image, low) in [(0.3, 0.35), (0.9, 0.7), (0.05, 0.2)] {
            let high = one(BlendMode::GrainExtract, image, low);
            assert!(close(one(BlendMode::GrainMerge, low, high), image));
        }
    }

    #[test]
    fn known_values() {
        assert!(close(one(BlendMode::Multiply, 0.5, 0.5), 0.25));
        assert!(close(one(BlendMode::Screen, 0.5, 0.5), 0.75));
        assert!(close(one(BlendMode::Overlay, 0.25, 1.0), 0.5));
        assert!(close(one(BlendMode::ColorDodge, 0.5, 0.5), 1.0));
        assert!(close(one(BlendMode::ColorBurn, 0.5, 0.5), 0.0));
        assert!(close(one(BlendMode::SoftLight, 0.25, 1.0), 0.5));
    }

    #[test]
    fn luminosity_keeps_backdrop_colour() {
        let out = BlendMode::Luminosity.apply([0.8, 0.2, 0.2], [0.5, 0.5, 0.5]);
        assert!(close(lum(out), 0.5));
        assert!(out[0] > out[1]);
    }

    #[test]
    fn ora_names_round_trip() {
        for mode in BlendMode::MENU.iter().flat_map(|g| g.iter()) {
            assert_eq!(BlendMode::from_ora_name(mode.ora_name()), Some(*mode));
        }
    }
}
