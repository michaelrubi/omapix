//! Photoshop's Brush Settings: the shape of a brush's tip, and how its dabs
//! vary along a stroke (Shape Dynamics, Scattering, Transfer, Noise and
//! Wet Edges).
//! [`crate::brush`] lays the dabs this works out.
//!
//! Whatever varies at random comes from the stroke's seed and the dab's
//! number alone, so a stroke is the same however its points arrive.
//!
//! Ported from PhotoCraft's `crates/paint/src/dynamics.rs` and `rng.rs`
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

/// The most dabs Scattering lays at one step.
pub const MAX_COUNT: u32 = 16;

/// A brush's tip and how it varies. The default is a plain round brush
/// with a dab every tenth of its diameter.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Dynamics {
    /// Brush Tip Shape: the distance between dabs, as a fraction of the
    /// diameter.
    pub spacing: f32,
    /// The tip's angle in degrees, anticlockwise.
    pub angle: f32,
    /// The tip's height as a fraction of its width (1 is round).
    pub roundness: f32,
    /// A sampled tip mirrored left to right, and top to bottom.
    pub flip_x: bool,
    pub flip_y: bool,
    /// Shape Dynamics: how much smaller a dab can be at random (0–1).
    pub size_jitter: f32,
    /// The smallest a dab gets, from jitter or a pen's pressure, as a
    /// fraction of the brush's size.
    pub minimum_diameter: f32,
    /// How far a dab's angle can turn at random (1 is any angle).
    pub angle_jitter: f32,
    /// The angle follows the stroke (Angle Jitter's Control: Direction).
    pub angle_follows: bool,
    /// How much flatter a dab can be at random (0–1).
    pub roundness_jitter: f32,
    /// The flattest that makes it, as a fraction of the tip's roundness.
    pub minimum_roundness: f32,
    /// Each dab mirrored or not at random, left to right and top to bottom.
    pub flip_x_jitter: bool,
    pub flip_y_jitter: bool,
    /// Scattering: how far dabs stray from the stroke, in radii (Photoshop
    /// goes to 10, its 1000 %).
    pub scatter: f32,
    /// Stray along the stroke as well as across it.
    pub both_axes: bool,
    /// Dabs at each step, up to [`MAX_COUNT`].
    pub count: u32,
    /// How many fewer there can be at random (0–1).
    pub count_jitter: f32,
    /// Transfer: how much lower a dab's opacity can be at random (0–1).
    pub opacity_jitter: f32,
    /// How much lower a dab's flow can be at random (0–1).
    pub flow_jitter: f32,
    /// Noise: the soft parts of each dab broken up into grain.
    pub noise: bool,
    /// Wet Edges: paint gathers along the edge of the stroke, as
    /// watercolour's does.
    pub wet_edges: bool,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self {
            spacing: 0.1,
            angle: 0.0,
            roundness: 1.0,
            flip_x: false,
            flip_y: false,
            size_jitter: 0.0,
            minimum_diameter: 0.0,
            angle_jitter: 0.0,
            angle_follows: false,
            roundness_jitter: 0.0,
            minimum_roundness: 0.25,
            flip_x_jitter: false,
            flip_y_jitter: false,
            scatter: 0.0,
            both_axes: false,
            count: 1,
            count_jitter: 0.0,
            opacity_jitter: 0.0,
            flow_jitter: 0.0,
            noise: false,
            wet_edges: false,
        }
    }
}

/// A step along a stroke, before anything varies: where it is, the brush's
/// diameter there, how far coverage can build up, each dab's flow, and the
/// way the stroke is going (radians, anticlockwise).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Step {
    pub x: f32,
    pub y: f32,
    pub diameter: f32,
    pub most: f32,
    pub flow: f32,
    pub direction: f32,
}

/// One dab: its centre, radius, angle (radians, anticlockwise) and
/// roundness, whether its tip is mirrored, how far coverage can build up
/// under it, and its flow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Dab {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub angle: f32,
    pub roundness: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    pub most: f32,
    pub flow: f32,
}

impl Dynamics {
    /// Whether dabs depend on the way the stroke is going.
    pub(crate) fn follows_direction(&self) -> bool {
        self.angle_follows || self.scatter > 0.0 && !self.both_axes
    }

    /// The distance between steps of a brush `diameter` across.
    pub(crate) fn step(&self, diameter: f32) -> f32 {
        (diameter.max(1.0) * self.spacing.max(0.01)).max(0.5)
    }

    /// The dabs of step number `step` of the stroke `seed` began. `next`
    /// numbers the stroke's dabs.
    pub(crate) fn dabs(&self, seed: u64, step: u64, next: &mut u64, at: &Step) -> Vec<Dab> {
        let unit = |v: f32| v.clamp(0.0, 1.0);
        let count = self.count.clamp(1, MAX_COUNT) as f32;
        let fewer = 1.0 - unit(self.count_jitter) * random(seed, step, COUNT);
        let count = ((count * fewer).round() as u32).clamp(1, MAX_COUNT);
        (0..count)
            .map(|_| {
                let i = *next;
                *next += 1;
                let r = |stream: u64| random(seed, i, stream);
                let either_way = |stream: u64| r(stream) * 2.0 - 1.0;

                let diameter = at.diameter * (1.0 - unit(self.size_jitter) * r(SIZE)).max(unit(self.minimum_diameter));
                let radius = (diameter / 2.0).max(0.5);
                let followed = if self.angle_follows { at.direction.to_degrees() } else { 0.0 };
                let angle = self.angle + followed + either_way(ANGLE) * unit(self.angle_jitter) * 180.0;
                let flatter = (1.0 - unit(self.roundness_jitter) * r(ROUNDNESS)).max(unit(self.minimum_roundness));
                let roundness = (self.roundness.clamp(0.01, 1.0) * flatter).clamp(0.01, 1.0);

                let (mut x, mut y) = (at.x, at.y);
                if self.scatter > 0.0 {
                    let far = self.scatter * radius;
                    let (across, along) = (either_way(SCATTER_X) * far, either_way(SCATTER_Y) * far);
                    if self.both_axes {
                        x += across;
                        y += along;
                    } else {
                        // Across the stroke only (y runs down the image).
                        x += at.direction.sin() * across;
                        y += at.direction.cos() * across;
                    }
                }
                Dab {
                    x,
                    y,
                    radius,
                    angle: angle.to_radians(),
                    roundness,
                    // Mirrored, or with jitter as often as not.
                    flip_x: self.flip_x != (self.flip_x_jitter && r(FLIP_X) < 0.5),
                    flip_y: self.flip_y != (self.flip_y_jitter && r(FLIP_Y) < 0.5),
                    most: at.most * (1.0 - unit(self.opacity_jitter) * r(OPACITY)),
                    flow: at.flow * (1.0 - unit(self.flow_jitter) * r(FLOW)),
                }
            })
            .collect()
    }
}

// One stream of random numbers for each thing that varies, so that they
// vary independently.
const SIZE: u64 = 1;
const ANGLE: u64 = 2;
const ROUNDNESS: u64 = 3;
const FLIP_X: u64 = 4;
const FLIP_Y: u64 = 5;
const SCATTER_X: u64 = 6;
const SCATTER_Y: u64 = 7;
const COUNT: u64 = 8;
const OPACITY: u64 = 9;
const FLOW: u64 = 10;

/// SplitMix64's finaliser.
pub(crate) fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A number from 0 up to 1 for the image's pixel at (x, y): the same
/// however often a stroke's dabs cross it, which is what keeps Noise's
/// grain from averaging away.
pub(crate) fn grain(x: u32, y: u32) -> f32 {
    let at = u64::from(x) << 32 | u64::from(y);
    (mix(at ^ 0x006E_6F69_7365u64.wrapping_mul(0xA24B_AED4_963E_E407)) >> 40) as f32 / (1u64 << 24) as f32
}

/// A number from 0 up to 1 that depends only on what it's given.
fn random(seed: u64, index: u64, stream: u64) -> f32 {
    let h = mix(seed ^ mix(index.wrapping_mul(0xD1B5_4A32_D192_ED03) ^ stream.wrapping_mul(0x8CB9_2BA7_2F3D_8DD7)));
    (h >> 40) as f32 / (1u64 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: Step = Step { x: 100.0, y: 50.0, diameter: 40.0, most: 1.0, flow: 0.8, direction: 0.0 };

    /// The dabs of 500 steps.
    fn many(d: &Dynamics, at: &Step) -> Vec<Dab> {
        let mut next = 0;
        (0..500).flat_map(|step| d.dabs(7, step, &mut next, at)).collect()
    }

    #[test]
    fn a_plain_brush_lays_one_dab_where_the_step_is() {
        let dabs = many(&Dynamics::default(), &AT);
        let plain = Dab { x: 100.0, y: 50.0, radius: 20.0, angle: 0.0, roundness: 1.0, flip_x: false, flip_y: false, most: 1.0, flow: 0.8 };
        assert_eq!(dabs.len(), 500);
        assert!(dabs.iter().all(|d| *d == plain));
    }

    #[test]
    fn random_numbers_are_spread_evenly_and_depend_only_on_what_they_are_given() {
        assert_eq!(random(7, 3, 1), random(7, 3, 1));
        assert!(random(7, 3, 1) != random(8, 3, 1) && random(7, 3, 1) != random(7, 3, 2));
        let mean = (0..10_000).map(|i| random(1, i, 0)).sum::<f32>() / 10_000.0;
        assert!((mean - 0.5).abs() < 0.02, "{mean}");
        assert!((0..10_000).all(|i| (0.0..1.0).contains(&random(2, i, 5))));
    }

    #[test]
    fn shape_dynamics_vary_each_dab_within_their_limits() {
        let d = Dynamics {
            size_jitter: 1.0,
            minimum_diameter: 0.25,
            angle_jitter: 0.5,
            roundness: 0.8,
            roundness_jitter: 1.0,
            minimum_roundness: 0.5,
            ..Default::default()
        };
        let dabs = many(&d, &AT);
        let range = |of: fn(&Dab) -> f32| dabs.iter().map(of).fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        // Radius from a quarter of 20 to all of it, reaching both ends.
        let (lo, hi) = range(|d| d.radius);
        assert!(lo == 5.0 && hi > 19.5, "{lo} {hi}");
        // Angle a quarter turn either way.
        let (lo, hi) = range(|d| d.angle.to_degrees());
        assert!((-90.0..-80.0).contains(&lo) && hi <= 90.0 && hi > 80.0, "{lo} {hi}");
        // Roundness from half the tip's 80 % to all of it.
        let (lo, hi) = range(|d| d.roundness);
        assert!((lo - 0.4).abs() < 1e-6 && hi > 0.78 && hi <= 0.8, "{lo} {hi}");
        // The same stroke again is the same.
        assert_eq!(dabs, many(&d, &AT));
    }

    #[test]
    fn scattering_strays_across_the_stroke_or_both_ways_and_lays_several_dabs() {
        let across = Dynamics { scatter: 2.0, count: 4, ..Default::default() };
        let dabs = many(&across, &AT);
        assert_eq!(dabs.len(), 2000);
        // Going east, they stray up and down by up to two radii.
        assert!(dabs.iter().all(|d| (d.x - 100.0).abs() < 1e-3 && (d.y - 50.0).abs() <= 40.0));
        assert!(dabs.iter().any(|d| d.y < 15.0) && dabs.iter().any(|d| d.y > 85.0));
        // Going north, left and right.
        let north = Step { direction: std::f32::consts::FRAC_PI_2, ..AT };
        assert!(many(&across, &north).iter().all(|d| (d.y - 50.0).abs() < 1e-3 && (d.x - 100.0).abs() <= 40.0));

        let both = Dynamics { both_axes: true, ..across };
        let dabs = many(&both, &AT);
        assert!(dabs.iter().any(|d| d.x < 65.0) && dabs.iter().any(|d| d.x > 135.0) && dabs.iter().any(|d| d.y < 15.0));
        assert!(!both.follows_direction() && across.follows_direction());

        // Count Jitter lays from one dab to all four.
        let fewer = Dynamics { count_jitter: 1.0, ..across };
        let mut next = 0;
        let counts: Vec<usize> = (0..500).map(|step| fewer.dabs(7, step, &mut next, &AT).len()).collect();
        assert!(counts.iter().all(|n| (1..=4).contains(n)) && counts.contains(&1) && counts.contains(&4));
    }

    #[test]
    fn transfer_lowers_opacity_and_flow_and_direction_turns_the_tip() {
        let d = Dynamics { opacity_jitter: 0.5, flow_jitter: 1.0, angle: 30.0, angle_follows: true, ..Default::default() };
        let north = Step { direction: std::f32::consts::FRAC_PI_2, ..AT };
        let dabs = many(&d, &north);
        assert!(dabs.iter().all(|d| d.most > 0.5 && d.most <= 1.0 && d.flow >= 0.0 && d.flow <= 0.8));
        assert!(dabs.iter().any(|d| d.most < 0.55) && dabs.iter().any(|d| d.flow < 0.05) && dabs.iter().any(|d| d.flow > 0.75));
        assert!(dabs.iter().all(|d| (d.angle.to_degrees() - 120.0).abs() < 1e-3));
        assert!(d.follows_direction());
    }

    #[test]
    fn a_tip_is_mirrored_always_or_as_often_as_not() {
        let always = many(&Dynamics { flip_x: true, ..Default::default() }, &AT);
        assert!(always.iter().all(|d| d.flip_x && !d.flip_y));
        // With jitter, about half of them each way, and not the same half.
        let jitter = Dynamics { flip_x_jitter: true, flip_y_jitter: true, ..Default::default() };
        let dabs = many(&jitter, &AT);
        let (x, y) = (dabs.iter().filter(|d| d.flip_x).count(), dabs.iter().filter(|d| d.flip_y).count());
        assert!((200..300).contains(&x) && (200..300).contains(&y), "{x} {y}");
        assert!(dabs.iter().any(|d| d.flip_x != d.flip_y));
        // A mirrored tip with jitter is still either way.
        let both = many(&Dynamics { flip_x: true, ..jitter }, &AT);
        assert!((200..300).contains(&both.iter().filter(|d| d.flip_x).count()));
        assert!(dabs.iter().zip(&both).all(|(a, b)| a.flip_x != b.flip_x));
    }

    #[test]
    fn grain_is_spread_evenly_and_fixed_to_the_images_pixels() {
        assert_eq!(grain(10, 20), grain(10, 20));
        assert!(grain(10, 20) != grain(11, 20) && grain(10, 20) != grain(20, 10));
        let mean = (0..10_000).map(|i| grain(i % 100, i / 100)).sum::<f32>() / 10_000.0;
        assert!((mean - 0.5).abs() < 0.02, "{mean}");
    }

    #[test]
    fn spacing_sets_the_distance_between_steps() {
        assert_eq!(Dynamics::default().step(100.0), 10.0);
        assert_eq!(Dynamics { spacing: 0.25, ..Default::default() }.step(100.0), 25.0);
        // Never closer than half a pixel.
        assert_eq!(Dynamics::default().step(2.0), 0.5);
        assert_eq!(Dynamics { spacing: 0.0, ..Default::default() }.step(100.0), 1.0);
    }
}
