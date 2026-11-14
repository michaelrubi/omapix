//! Random layer stacks for the tests that check one way of compositing
//! against another: the GPU's live view against `composite.rs`
//! (`gpu.rs`), and cached group results against none (`cache.rs`).
//!
//! A stack is a list of [`Spec`]s, bottom first, as `Document::layers` is.
//! Each layer's pixels and mask depend only on its `id`, so a stack's
//! `Debug` output is all it takes to build the same document again, and a
//! stack with layers taken out by [`shrink::shrink`] still draws what's
//! left as it did.

mod cache;
pub mod shrink;

use omapix_engine::adjust::{Adjustment, ChannelMixer, ColorBalance, ColorLookup, Curves, HueSaturation, Levels, SelectiveColor};
use omapix_engine::layer::{BlendIf, BlendIfChannel, Layer, Mask};
use omapix_engine::tiled::Tiled;
use omapix_engine::{BlendMode, ColorProfile, Document, Pixel};

/// The size of every document made here: small, and not a whole tile.
pub const SIZE: (u32, u32) = (48, 32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pixels,
    Curves,
    Group,
    Levels,
    HueSaturation,
    ColorBalance,
    SelectiveColor,
    ChannelMixer,
    ColorLookup,
}

impl Kind {
    /// The adjustment layers, in the order of `Adjustment`.
    pub const ADJUSTMENTS: [Kind; 7] = [
        Kind::Curves,
        Kind::Levels,
        Kind::HueSaturation,
        Kind::ColorBalance,
        Kind::SelectiveColor,
        Kind::ChannelMixer,
        Kind::ColorLookup,
    ];

    pub fn is_adjustment(self) -> bool {
        !matches!(self, Kind::Pixels | Kind::Group)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskSpec {
    None,
    On,
    /// There, but disabled (Shift+click in Photoshop).
    Off,
}

/// One layer of a stack: everything about it that compositing looks at.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub id: u64,
    pub kind: Kind,
    /// The group it's in, which comes after it in the list.
    pub parent: Option<u64>,
    pub clipped: bool,
    pub visible: bool,
    pub blend: BlendMode,
    pub opacity: f32,
    pub mask: MaskSpec,
    pub blend_if: Option<BlendIf>,
}

impl Spec {
    /// A plain, visible layer of `kind`: Normal (a group Pass Through) at
    /// full opacity, for tests to change with struct update syntax.
    pub fn new(id: u64, kind: Kind) -> Self {
        Self {
            id,
            kind,
            parent: None,
            clipped: false,
            visible: true,
            blend: if kind == Kind::Group { BlendMode::PassThrough } else { BlendMode::Normal },
            opacity: 1.0,
            mask: MaskSpec::None,
            blend_if: None,
        }
    }
}

/// xorshift64*: small, and the same on every machine.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Seeds are small numbers; spread them out, and never zero.
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0 up to, not including, `n`.
    pub fn below(&mut self, n: u64) -> u64 {
        (self.next_u64() >> 33) % n
    }

    /// True `percent` times in a hundred.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// 0 to 1.
    pub fn unit(&mut self) -> f32 {
        self.below(1 << 24) as f32 / (1 << 24) as f32
    }

    pub fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len() as u64) as usize]
    }
}

/// Every blend mode a layer can have.
fn modes() -> Vec<BlendMode> {
    BlendMode::MENU.iter().flat_map(|g| g.iter()).copied().collect()
}

fn blend_if(rng: &mut Rng) -> BlendIf {
    let range = |rng: &mut Rng| {
        if rng.chance(30) {
            return [0.0, 0.0, 1.0, 1.0];
        }
        let mut r = [rng.unit(), rng.unit(), rng.unit(), rng.unit()];
        r.sort_by(f32::total_cmp);
        // Unsplit sliders, as often as not.
        if rng.chance(50) {
            r[1] = r[0];
        }
        if rng.chance(50) {
            r[2] = r[3];
        }
        r
    };
    BlendIf {
        channel: rng.pick(&[BlendIfChannel::Gray, BlendIfChannel::Red, BlendIfChannel::Green, BlendIfChannel::Blue]),
        this: range(rng),
        underlying: range(rng),
    }
}

/// The layers of one group (or of the top level), into `out`. `budget` is
/// how many more layers there may be.
fn siblings(rng: &mut Rng, parent: Option<u64>, depth: usize, budget: &mut usize, next: &mut u64, out: &mut Vec<Spec>) {
    let modes = modes();
    let count = 1 + rng.below(if depth == 0 { 5 } else { 3 }) as usize;
    for place in 0..count {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let id = *next;
        *next += 1;
        let kind = match rng.below(10) {
            0..=1 if depth < 3 && *budget > 0 => Kind::Group,
            2..=3 if place > 0 || depth > 0 => rng.pick(&Kind::ADJUSTMENTS),
            _ => Kind::Pixels,
        };
        if kind == Kind::Group {
            siblings(rng, Some(id), depth + 1, budget, next, out);
        }
        let plain = match kind {
            Kind::Group => BlendMode::PassThrough,
            _ => BlendMode::Normal,
        };
        out.push(Spec {
            id,
            kind,
            parent,
            // Only with a layer below it in its group to clip to.
            clipped: place > 0 && rng.chance(30),
            visible: !rng.chance(12),
            blend: if rng.chance(45) { plain } else { rng.pick(&modes) },
            opacity: if rng.chance(50) { 1.0 } else { 0.1 + 0.85 * rng.unit() },
            mask: match rng.below(10) {
                0..=2 => MaskSpec::On,
                3 => MaskSpec::Off,
                _ => MaskSpec::None,
            },
            blend_if: rng.chance(15).then(|| blend_if(rng)),
        });
    }
}

/// The stack for `seed`: 1 to 10 layers, in groups up to three deep.
pub fn specs(seed: u64) -> Vec<Spec> {
    let mut rng = Rng::new(seed);
    let (mut out, mut budget, mut next) = (Vec::new(), 2 + rng.below(9) as usize, 1);
    siblings(&mut rng, None, 0, &mut budget, &mut next, &mut out);
    out
}

/// Whether `specs` is a stack `Document` can hold: ids all different, and
/// each group straight after what's in it (see `groups.rs`).
pub fn valid(specs: &[Spec]) -> bool {
    let unique = specs.iter().enumerate().all(|(i, s)| specs[..i].iter().all(|o| o.id != s.id));
    let groups = specs
        .iter()
        .all(|s| s.parent.is_none_or(|p| specs.iter().any(|o| o.id == p && o.kind == Kind::Group)));
    // Its group, and the groups that's in, come after it, with nothing
    // from outside them in between.
    let ordered = specs.iter().enumerate().all(|(i, s)| {
        chain(specs, s).iter().all(|&g| {
            let at = specs.iter().position(|o| o.id == g);
            at.is_some_and(|at| at > i && specs[i..at].iter().all(|o| chain(specs, o).contains(&g)))
        })
    });
    unique && groups && ordered
}

/// The groups `s` is in, innermost first (cut short if they go round in a
/// circle, which [`valid`] then refuses).
pub fn chain(specs: &[Spec], s: &Spec) -> Vec<u64> {
    std::iter::successors(s.parent, |&p| specs.iter().find(|o| o.id == p)?.parent)
        .take(specs.len() + 1)
        .collect()
}

/// A number from `id` and `salt` that looks random: for what a layer
/// holds, which must not change as layers round it are taken out.
fn hash(id: u64, salt: u64) -> u64 {
    Rng::new(id.wrapping_mul(1_000_003).wrapping_add(salt)).next_u64()
}

/// Layer `id`'s pixels: colours that slope across it, partly transparent,
/// inside a rectangle of its own, with nothing outside. The bottom of a
/// stack is usually a photo, so every third id fills the canvas, opaque.
pub fn pixels(id: u64) -> Tiled<Pixel> {
    let (w, h) = SIZE;
    let k = |salt: u64, n: u64| (hash(id, salt) >> 20) % n;
    let (x0, y0) = (k(1, u64::from(w) / 2) as u32, k(2, u64::from(h) / 2) as u32);
    let (x1, y1) = (x0 + w / 3 + k(3, u64::from(w) / 3) as u32, y0 + h / 3 + k(4, u64::from(h) / 3) as u32);
    let whole = id % 3 == 1;
    let slope = |salt: u64, x: u32, y: u32| {
        let (a, b, c) = (k(salt, 1300), k(salt + 10, 1900), k(salt + 20, 65536));
        ((c + a * u64::from(x) + b * u64::from(y)) % 65536) as u16
    };
    let data: Vec<Pixel> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let alpha = if whole { 65535 } else { 20000 + slope(8, x, y) / 2 };
            if whole || (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                [slope(5, x, y), slope(6, x, y), slope(7, x, y), alpha]
            } else {
                [0; 4]
            }
        })
        .collect();
    Tiled::from_slice(w, h, [0; 4], &data)
}

/// Layer `id`'s mask: a slope from black to white, its own way round.
pub fn mask(id: u64) -> Tiled<u16> {
    let (w, h) = SIZE;
    let (a, b) = ((hash(id, 30) >> 20) % 1300, (hash(id, 31) >> 20) % 2000);
    let data: Vec<u16> = (0..w * h).map(|i| (a * u64::from(i % w) + b * u64::from(i / w)).min(65535) as u16).collect();
    Tiled::from_slice(w, h, 65535, &data)
}

/// An adjustment of `kind` with settings of layer `id`'s own, none of
/// them the default.
fn adjustment(kind: Kind, id: u64) -> Adjustment {
    // 0 to 1, a different one for each `salt`.
    let u = |salt: u64| ((hash(id, salt) >> 20) % 1000) as f32 / 1000.0;
    // −1 to 1.
    let s = |salt: u64| 2.0 * u(salt) - 1.0;
    match kind {
        Kind::Curves => {
            let mut c = Curves::default();
            c.master.points.insert(1, (0.5, 0.25 + 0.5 * u(40)));
            Adjustment::Curves(c)
        }
        Kind::Levels => Adjustment::Levels(Levels {
            in_black: 0.15 * u(41),
            in_white: 0.8 + 0.2 * u(42),
            gamma: 0.6 + 0.9 * u(43),
            out_black: 0.1 * u(44),
            out_white: 0.9 + 0.1 * u(45),
        }),
        Kind::HueSaturation => Adjustment::HueSaturation(HueSaturation {
            hue: 90.0 * s(41),
            saturation: 60.0 * s(42),
            lightness: 25.0 * s(43),
        }),
        Kind::ColorBalance => Adjustment::ColorBalance(ColorBalance {
            shadows: [30.0 * s(41), 30.0 * s(42), 30.0 * s(43)],
            midtones: [40.0 * s(44), 40.0 * s(45), 40.0 * s(46)],
            highlights: [30.0 * s(47), 30.0 * s(48), 30.0 * s(49)],
            preserve_luminosity: id.is_multiple_of(2),
        }),
        Kind::SelectiveColor => {
            let ranges = std::array::from_fn(|r| std::array::from_fn(|c| 50.0 * s(60 + 4 * r as u64 + c as u64)));
            Adjustment::SelectiveColor(SelectiveColor {
                ranges,
                relative: id.is_multiple_of(2),
            })
        }
        Kind::ChannelMixer => Adjustment::ChannelMixer(ChannelMixer {
            red: [100.0 - 40.0 * u(41), 40.0 * s(42), 20.0 * s(43), 10.0 * s(44)],
            green: [30.0 * s(45), 100.0 - 40.0 * u(46), 20.0 * s(47), 10.0 * s(48)],
            blue: [20.0 * s(49), 30.0 * s(50), 100.0 - 40.0 * u(51), 10.0 * s(52)],
            gray: [40.0, 40.0, 20.0, 0.0],
            monochrome: id.is_multiple_of(5),
        }),
        Kind::ColorLookup => {
            // Three points a side, each corner and edge moved a little,
            // red changing fastest as a .cube file has it.
            let n = 3;
            let table = (0..n * n * n)
                .map(|i| {
                    let at = [i % n, i / n % n, i / (n * n)].map(|v| v as f32 / (n - 1) as f32);
                    std::array::from_fn(|c| (at[c] + 0.15 * s(70 + 3 * i as u64 + c as u64)).clamp(0.0, 1.0))
                })
                .collect();
            Adjustment::ColorLookup(ColorLookup {
                title: format!("fuzz {id}"),
                size: n,
                is_3d: true,
                table,
                ..ColorLookup::default()
            })
        }
        Kind::Pixels | Kind::Group => unreachable!("not an adjustment"),
    }
}

/// The document `specs` describes. They must be [`valid`].
pub fn document(specs: &[Spec]) -> Document {
    assert!(valid(specs), "not a stack: {specs:#?}");
    let (w, h) = SIZE;
    let layers = specs
        .iter()
        .map(|s| {
            let name = format!("{:?} {}", s.kind, s.id);
            let mut layer = match s.kind {
                Kind::Pixels => Layer::from_pixels(s.id, name, pixels(s.id)),
                Kind::Group => Layer::group(s.id, name, w, h),
                kind => Layer::adjustment(s.id, adjustment(kind, s.id), w, h),
            };
            layer.mask = match s.mask {
                MaskSpec::None => None,
                MaskSpec::On | MaskSpec::Off => Some(Mask {
                    pixels: mask(s.id),
                    enabled: s.mask == MaskSpec::On,
                }),
            };
            (layer.parent, layer.clipped, layer.visible) = (s.parent, s.clipped, s.visible);
            (layer.blend, layer.opacity, layer.blend_if) = (s.blend, s.opacity, s.blend_if);
            layer
        })
        .collect();
    Document::new("fuzz.tif".into(), ColorProfile::srgb(), 16, w, h, layers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_seed_makes_a_stack_a_document_can_hold() {
        let mut kinds = [0; 9];
        let (mut clipped, mut deepest) = (0, 0);
        for seed in 0..2000 {
            let specs = specs(seed);
            assert!(valid(&specs), "seed {seed}: {specs:#?}");
            assert!((1..=10).contains(&specs.len()), "seed {seed}");
            let doc = document(&specs);
            for (i, s) in specs.iter().enumerate() {
                kinds[s.kind as usize] += 1;
                deepest = deepest.max(doc.depth(s.id));
                // What's in a group is its span, and a clipped layer has
                // a layer below it in its group.
                if s.kind == Kind::Group {
                    let inside = specs.iter().filter(|o| doc.is_inside(o.id, s.id)).count();
                    assert_eq!(doc.span(i).len(), inside + 1, "seed {seed}");
                }
                if s.clipped {
                    clipped += 1;
                    assert!(doc.can_clip(i), "seed {seed}: layer {} has nothing to clip to", s.id);
                }
            }
        }
        // Each kind of thing turns up often enough to be worth the run.
        let [pixels, groups] = [Kind::Pixels, Kind::Group].map(|k| kinds[k as usize]);
        assert!(pixels > 500 && groups > 500, "{kinds:?}");
        assert!(Kind::ADJUSTMENTS.iter().all(|&k| kinds[k as usize] > 100), "{kinds:?}");
        assert!(clipped > 500 && deepest == 3, "{clipped} clipped, {deepest} deep");
    }

    #[test]
    fn a_seed_always_makes_the_same_stack_and_a_layer_keeps_its_pixels() {
        assert_eq!(specs(7), specs(7));
        assert_ne!(specs(7), specs(8));
        let (a, b) = (pixels(4), pixels(4));
        assert_eq!(a.to_raster().pixels(), b.to_raster().pixels());
        assert_ne!(a.to_raster().pixels(), pixels(5).to_raster().pixels());
    }

    #[test]
    fn stacks_out_of_order_are_not_valid() {
        let layer = |id, parent| Spec {
            parent,
            ..Spec::new(id, Kind::Pixels)
        };
        let group = |id, parent| Spec {
            parent,
            ..Spec::new(id, Kind::Group)
        };
        assert!(valid(&[layer(1, Some(3)), layer(2, Some(3)), group(3, None), layer(4, None)]));
        // A group before what's in it, a layer from outside in the middle
        // of one, a parent that isn't there or isn't a group, and an id
        // used twice.
        assert!(!valid(&[group(3, None), layer(1, Some(3))]));
        assert!(!valid(&[layer(1, Some(3)), layer(4, None), group(3, None)]));
        assert!(!valid(&[layer(1, Some(9))]));
        assert!(!valid(&[layer(1, None), layer(2, Some(1))]));
        assert!(!valid(&[layer(1, None), layer(1, None)]));
        assert!(!valid(&[group(1, Some(2)), group(2, Some(1))]));
        // Nested: the inner group's layers are inside the outer one too.
        assert!(valid(&[layer(1, Some(2)), group(2, Some(3)), layer(4, Some(3)), group(3, None)]));
    }
}
