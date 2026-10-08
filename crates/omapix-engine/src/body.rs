//! Body Reshape (docs/AI.md, feature 8): sliders that make a person's
//! waist, hips, shoulders, arms and legs wider or narrower, their legs and
//! neck longer and their head larger, and level their shoulders, as a warp
//! ([`crate::warp`]) about the bones the pose model finds, faded out away
//! from the person so what's behind them bends as little as it can.

use crate::reshape::ramp;
use crate::selection::{edt_rows, transpose_f32};
use crate::warp::Field;

type Point = [f32; 2];

/// Where the pose model puts a joint: x and y in image pixels, and how
/// likely it is to be in view, 0–1.
pub type Joint = [f32; 3];

/// A person's joints. Each pair is the person's own left, then right.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Joints {
    pub ears: [Joint; 2],
    pub shoulders: [Joint; 2],
    pub elbows: [Joint; 2],
    pub wrists: [Joint; 2],
    pub hips: [Joint; 2],
    pub knees: [Joint; 2],
    pub ankles: [Joint; 2],
}

/// How much of each point of a square of the image is the person, 0–1:
/// `size` × `size` values, in rows, over the square of `side` pixels centred
/// on `centre` and turned by `angle` (radians, clockwise).
pub struct Matte<'a> {
    pub centre: Point,
    pub side: f32,
    pub angle: f32,
    pub size: usize,
    pub cover: &'a [f32],
}

/// How a body is reshaped: each slider from −100 to 100, and 0 leaves it
/// as it is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shape {
    /// The shoulders brought level, 0 to 100: at 100 one is as high as the
    /// other.
    pub level: f32,
    pub head: f32,
    pub neck: f32,
    pub shoulders: f32,
    pub waist: f32,
    pub hips: f32,
    pub arms: f32,
    pub legs: f32,
    pub leg_length: f32,
}

/// What each slider does at 100: widths and the head's size as a part of
/// their own, the legs' length as a part of theirs, and the neck's as a
/// part of the way from the shoulders to the ears.
const HEAD: f32 = 0.12;
const NECK: f32 = 0.12;
const SHOULDERS: f32 = 0.12;
const WAIST: f32 = 0.2;
const HIPS: f32 = 0.15;
const ARMS: f32 = 0.25;
const LEGS: f32 = 0.2;
const LEG_LENGTH: f32 = 0.08;

/// Where down the torso each of its sliders starts, does the most and
/// stops: 0 is the shoulders and 1 the hips.
const SHOULDER_LINE: [f32; 3] = [-0.25, 0.0, 0.3];
const WAIST_LINE: [f32; 3] = [0.25, 0.62, 0.95];
const HIP_LINE: [f32; 3] = [0.75, 1.05, 1.4];
/// The torso is measured from above the shoulders to below the hips.
const TORSO: [f32; 2] = [-0.2, 1.4];

/// Shoulders further from level than this (degrees) aren't levelled: the
/// person's lying down, or leaning far.
pub const LEVELLED: f32 = 30.0;
/// Where levelling the shoulders works, in shoulder widths above and below
/// the line through them: all of it from the tops of the shoulders to the
/// line, none from the jaw up, and less and less down to the waist.
const LEVEL: [f32; 3] = [-0.35, -0.1, 1.0];

/// What's beside a part goes with its edge, less the further away it is:
/// to nothing at this many times the part's half-width (the head's radius)
/// from it. So an arm beside a narrower waist comes in with it.
const CARRY: f32 = 3.0;
const HEAD_CARRY: f32 = 1.0;
/// Outside the person a move fades out, over this many times its own
/// length: far enough not to tear, and no further.
const FADE: f32 = 5.0;
/// A joint this likely to be in view is.
const SEEN: f32 = 0.5;

/// From 0 at `from` up to 1 at `peak` and back to 0 at `to`.
fn bump([from, peak, to]: [f32; 3], s: f32) -> f32 {
    ramp(from, peak, s) * (1.0 - ramp(peak, to, s))
}

/// The way from `a` to `b`, and how far it is.
fn towards(a: Point, b: Point) -> (Point, f32) {
    let (x, y) = (b[0] - a[0], b[1] - a[1]);
    let length = x.hypot(y).max(1e-3);
    ([x / length, y / length], length)
}

fn between(a: Joint, b: Joint) -> Point {
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
}

/// How far outside the person each point of the image is.
#[derive(Clone, Debug, PartialEq)]
struct Outside {
    centre: Point,
    sin: f32,
    cos: f32,
    size: usize,
    /// Pixels between the grid's points.
    cell: f32,
    /// Pixels to the person from each point of the grid: 0 on them.
    far: Vec<f32>,
}

impl Outside {
    fn of(matte: &Matte) -> Self {
        let size = matte.size;
        let cell = matte.side / size as f32;
        // The squared distance, in cells, to the nearest cell on the person.
        let on = matte.cover.iter().map(|&v| if v >= 0.5 { 0.0 } else { f32::INFINITY }).collect::<Vec<_>>();
        let mut columns = transpose_f32(&on, size, size);
        edt_rows(&mut columns, size);
        let mut far = transpose_f32(&columns, size, size);
        edt_rows(&mut far, size);
        // A cell's edge is half a cell from its middle. With nobody there,
        // everywhere is far.
        let far = far.into_iter().map(|d| if d.is_finite() { (d.sqrt() - 0.5).max(0.0) * cell } else { matte.side });
        let (sin, cos) = matte.angle.sin_cos();
        Self { centre: matte.centre, sin, cos, size, cell, far: far.collect() }
    }

    /// How far outside the person `p` is, in pixels: 0 on them.
    fn distance(&self, p: Point) -> f32 {
        let (x, y) = (p[0] - self.centre[0], p[1] - self.centre[1]);
        let middle = (self.size - 1) as f32 / 2.0;
        let (u, v) = ((x * self.cos + y * self.sin) / self.cell + middle, (y * self.cos - x * self.sin) / self.cell + middle);
        // Beyond the grid, as far as its edge is, and on from there.
        let (cu, cv) = (u.clamp(0.0, 2.0 * middle), v.clamp(0.0, 2.0 * middle));
        let (i, j) = ((cu as usize).min(self.size - 2), (cv as usize).min(self.size - 2));
        let (fx, fy) = (cu - i as f32, cv - j as f32);
        let at = |i: usize, j: usize| self.far[j * self.size + i];
        let top = at(i, j) + (at(i + 1, j) - at(i, j)) * fx;
        let bottom = at(i, j + 1) + (at(i + 1, j + 1) - at(i, j + 1)) * fx;
        top + (bottom - top) * fy + (u - cu).hypot(v - cv) * self.cell
    }

    /// How far from `p` the person's edge is going `way`, up to `cap`:
    /// where they're left, having been on them.
    fn edge(&self, p: Point, way: Point, cap: f32) -> Option<f32> {
        let step = self.cell / 2.0;
        let mut on = false;
        let mut t = 0.0;
        while t <= cap {
            let off = self.distance([p[0] + way[0] * t, p[1] + way[1] * t]) > self.cell / 4.0;
            if on && off {
                return Some(t - step / 2.0);
            }
            on |= !off;
            t += step;
        }
        None
    }
}

/// A part of the body about a bone: the torso, or half a limb.
#[derive(Clone, Debug, PartialEq)]
struct Tube {
    from: Point,
    /// The way the bone goes, and how long it is: s is 0 at `from` and 1 at
    /// its other end.
    along: Point,
    length: f32,
    /// The part's half-widths to the left and right of the way it goes, at
    /// points evenly from s = `first` to `last`.
    first: f32,
    last: f32,
    widths: Vec<[f32; 2]>,
}

impl Tube {
    /// The part from `from` to `to`, its half-widths measured in `outside`
    /// at `count` points from s = `first` to `last`: no more than `cap`,
    /// and `usual` where no edge is found. `paired` parts have a bone down
    /// their middle, so they're as wide as their narrower side: the other
    /// may run into the body, or a sleeve.
    fn measured(from: Point, to: Point, [first, last]: [f32; 2], count: usize, outside: &Outside, [cap, usual]: [f32; 2], paired: bool) -> Self {
        let (along, length) = towards(from, to);
        let right = [-along[1], along[0]];
        let mut widths: Vec<[f32; 2]> = (0..count)
            .map(|k| {
                let s = (first + (last - first) * k as f32 / (count - 1) as f32) * length;
                let p = [from[0] + along[0] * s, from[1] + along[1] * s];
                let sides = [outside.edge(p, [-right[0], -right[1]], cap), outside.edge(p, right, cap)];
                match (sides, paired) {
                    ([Some(l), Some(r)], true) => [l.min(r); 2],
                    ([Some(l), Some(r)], false) => [l, r],
                    ([Some(w), None] | [None, Some(w)], _) => [w; 2],
                    ([None, None], _) => [usual; 2],
                }
            })
            .collect();
        // Each the middle of three, so one stray edge doesn't dent it.
        let measured = widths.clone();
        for (k, width) in widths.iter_mut().enumerate().skip(1).take(count.saturating_sub(2)) {
            for (side, width) in width.iter_mut().enumerate() {
                let mut three = [measured[k - 1][side], measured[k][side], measured[k + 1][side]];
                three.sort_by(f32::total_cmp);
                *width = three[1];
            }
        }
        Self { from, along, length, first, last, widths }
    }

    /// How far along the bone `p` is (0 at its start and 1 at its end) and
    /// how far to its right, in pixels.
    fn place(&self, p: Point) -> (f32, f32) {
        let (x, y) = (p[0] - self.from[0], p[1] - self.from[1]);
        ((x * self.along[0] + y * self.along[1]) / self.length, y * self.along[0] - x * self.along[1])
    }

    /// The half-width at `s`, on the right or the left.
    fn width(&self, s: f32, right: bool) -> f32 {
        let last = (self.widths.len() - 1) as f32;
        let k = ((s - self.first) / (self.last - self.first) * last).clamp(0.0, last);
        let (a, b) = (k as usize, (k as usize + 1).min(self.widths.len() - 1));
        let side = usize::from(right);
        self.widths[a][side] + (self.widths[b][side] - self.widths[a][side]) * k.fract()
    }

    /// The half-width half way along.
    fn middle(&self) -> f32 {
        self.width(0.5, true).min(self.width(0.5, false))
    }

    /// Where `p` reads from, with the part `wider` (a part of its width)
    /// at each s: scaled across the bone within it, and beyond its edge,
    /// what's there goes with the edge, less the further away it is.
    fn widened(&self, p: Point, wider: impl Fn(f32) -> f32) -> Point {
        let (s, r) = self.place(p);
        let wider = wider(s);
        if wider == 0.0 {
            return [0.0; 2];
        }
        let width = self.width(s, r >= 0.0);
        let edge = width * (1.0 + wider);
        let across = if r.abs() <= edge {
            r / (1.0 + wider) - r
        } else {
            -r.signum() * wider * width * (1.0 - ramp(0.0, CARRY * width, r.abs() - edge))
        };
        [-self.along[1] * across, self.along[0] * across]
    }
}

/// Half a limb, and where along it its slider works: from nothing to all
/// between the first two, and back to nothing between the last two.
#[derive(Clone, Debug, PartialEq)]
struct Limb {
    tube: Tube,
    works: [f32; 4],
}

impl Limb {
    /// From `from` to `to`, if both are in view.
    fn seen(from: Joint, to: Joint, works: [f32; 4], outside: &Outside) -> Option<Self> {
        let (a, b) = ([from[0], from[1]], [to[0], to[1]]);
        let length = towards(a, b).1;
        // Pointing at the camera, there's no telling its width.
        (from[2] > SEEN && to[2] > SEEN && length > 4.0 * outside.cell).then(|| {
            let tube = Tube::measured(a, b, [0.1, 0.9], 5, outside, [0.35 * length, 0.15 * length], true);
            Self { tube, works }
        })
    }

    fn widened(&self, p: Point, wider: f32) -> Point {
        let [a, b, c, d] = self.works;
        self.tube.widened(p, |s| wider * ramp(a, b, s) * (1.0 - ramp(c, d, s)))
    }
}

/// A leg, for its length.
#[derive(Clone, Debug, PartialEq)]
struct Leg {
    hip: Point,
    knee: Point,
    ankle: Point,
    /// The thigh's half-width.
    width: f32,
}

impl Leg {
    /// Where `p` reads from with the leg `longer` (a part of its length),
    /// the hip staying where it is, and how much that counts at `p`: all
    /// of it on the leg, and less the further from it.
    fn lengthened(&self, p: Point, longer: f32) -> (Point, f32) {
        let ((thigh, upper), (calf, lower)) = (towards(self.hip, self.knee), towards(self.knee, self.ankle));
        // Where the knee goes, and how far along each bone `p` is, as they
        // are now.
        let knee = [self.hip[0] + thigh[0] * upper * (1.0 + longer), self.hip[1] + thigh[1] * upper * (1.0 + longer)];
        let on = |from: Point, way: Point, length: f32| {
            let t = ((p[0] - from[0]) * way[0] + (p[1] - from[1]) * way[1]).clamp(0.0, length * (1.0 + longer));
            (t, (p[0] - from[0] - way[0] * t).hypot(p[1] - from[1] - way[1] * t))
        };
        let ((u, from_thigh), (v, from_calf)) = (on(self.hip, thigh, upper), on(knee, calf, lower));
        // On the thigh, back along it; on the calf, back by the knee's
        // move too, and past the ankle the foot goes with it.
        let back = longer / (1.0 + longer);
        let above = [-thigh[0] * u * back, -thigh[1] * u * back];
        let below = [0, 1].map(|c| -thigh[c] * upper * longer - calf[c] * v * back);
        // Whichever bone is nearer, far more.
        let near = |d: f32| 1.0 / (d.powi(4) + self.width.powi(4) * 1e-4);
        let (a, b) = (near(from_thigh), near(from_calf));
        let d = [0, 1].map(|c| (above[c] * a + below[c] * b) / (a + b));
        (d, 1.0 - ramp(self.width, self.width * (1.0 + CARRY), from_thigh.min(from_calf)))
    }
}

/// A person the pose model found: what [`reshape`] needs to know of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Body {
    /// From between the shoulders to between the hips, and a little beyond
    /// each.
    torso: Tube,
    arms: Vec<Limb>,
    legs: Vec<Limb>,
    strides: Vec<Leg>,
    /// The head's middle (between the ears) and its radius, hair and all.
    head: Option<(Point, f32)>,
    /// The shoulders, the one on the left of the picture first.
    shoulders: [Point; 2],
    outside: Outside,
    /// The image's part they may move in, (x0, y0, x1, y1), before it's
    /// cut to the image.
    area: [f32; 4],
}

impl Body {
    /// The person with `joints`, whose `matte` says where their edges are:
    /// `None` without both shoulders in view.
    pub fn new(joints: &Joints, matte: &Matte) -> Option<Self> {
        let outside = Outside::of(matte);
        let [left, right] = joints.shoulders;
        if left[2].min(right[2]) <= SEEN {
            return None;
        }
        let (neck, seat) = (between(left, right), between(joints.hips[0], joints.hips[1]));
        let length = towards(neck, seat).1;
        if length < 4.0 * outside.cell {
            return None;
        }
        // No wider than this: beyond it is an arm against the body.
        let cap = (0.45 * length).max(0.75 * (left[0] - right[0]).hypot(left[1] - right[1]));
        let torso = Tube::measured(neck, seat, TORSO, 17, &outside, [cap, 0.3 * length], false);

        // Each limb's slider leaves its top to the shoulders' and hips',
        // and hands over at the elbow or knee.
        const UPPER: [f32; 4] = [0.05, 0.4, 0.9, 1.1];
        const LOWER: [f32; 4] = [-0.1, 0.1, 0.95, 1.2];
        let halves = |tops: [Joint; 2], middles: [Joint; 2], ends: [Joint; 2]| -> Vec<Limb> {
            let both = (0..2).flat_map(|k| [Limb::seen(tops[k], middles[k], UPPER, &outside), Limb::seen(middles[k], ends[k], LOWER, &outside)]);
            both.flatten().collect()
        };
        let arms = halves(joints.shoulders, joints.elbows, joints.wrists);
        let legs = halves(joints.hips, joints.knees, joints.ankles);
        let strides = (0..2).filter_map(|k| {
            let thigh = Limb::seen(joints.hips[k], joints.knees[k], UPPER, &outside)?;
            let calf = Limb::seen(joints.knees[k], joints.ankles[k], LOWER, &outside)?;
            Some(Leg { hip: thigh.tube.from, knee: calf.tube.from, ankle: [joints.ankles[k][0], joints.ankles[k][1]], width: thigh.tube.middle() })
        });
        let strides = strides.collect();

        // The head: as far as the person goes up and to the sides of the
        // middle of the ears.
        let [a, b] = joints.ears;
        let head = (a[2].max(b[2]) > SEEN).then(|| {
            let centre = between(a, b);
            let (up, neck_length) = towards(neck, centre);
            let ways = [[1.0, 0.0], [0.7, 0.7], [0.0, 1.0], [-0.7, 0.7], [-1.0, 0.0]].map(|[x, y]: [f32; 2]| [x * up[1] + y * up[0], -x * up[0] + y * up[1]]);
            let mut found: Vec<f32> = ways.iter().filter_map(|&way| outside.edge(centre, way, 1.2 * neck_length)).collect();
            found.sort_by(f32::total_cmp);
            let radius = found.get(found.len() / 2).copied().unwrap_or(0.6 * neck_length);
            (centre, radius.clamp(0.4 * neck_length, neck_length))
        });

        // They may move a little beyond the matte's square.
        let reach = matte.side * (0.5 * (outside.sin.abs() + outside.cos.abs()) + 0.25);
        let area = [matte.centre[0] - reach, matte.centre[1] - reach, matte.centre[0] + reach, matte.centre[1] + reach];
        let mut shoulders = [[left[0], left[1]], [right[0], right[1]]];
        shoulders.sort_by(|a, b| a[0].total_cmp(&b[0]));
        Some(Self { torso, arms, legs, strides, head, shoulders, outside, area })
    }

    /// How far the shoulders are from level, in degrees. Beyond
    /// [`LEVELLED`] they're left as they are.
    pub fn shoulder_tilt(&self) -> f32 {
        let [a, b] = self.shoulders;
        (b[1] - a[1]).atan2(b[0] - a[0]).to_degrees().abs()
    }

    /// Where `p` reads from with the shoulders a `part` of the way to
    /// level: each goes up or down to meet the other, with what's above
    /// and beside it, and the body below less and less down to the waist.
    /// The head stays.
    fn levelled(&self, p: Point, part: f32) -> Point {
        if self.shoulder_tilt() > LEVELLED {
            return [0.0; 2];
        }
        let [a, b] = self.shoulders;
        let (across, slope) = (b[0] - a[0], (b[1] - a[1]) / (b[0] - a[0]));
        // How far down a point at `y` goes: beyond a shoulder, as far as
        // the shoulder.
        let x = (p[0] - (a[0] + b[0]) / 2.0).clamp(-across / 2.0, across / 2.0);
        let line = (a[1] + b[1]) / 2.0 + slope * x;
        let [none, top, waist] = LEVEL;
        let down = |y: f32| {
            let below = (y - line) / across;
            -slope * x * part * ramp(none, top, below) * (1.0 - ramp(0.0, waist, below))
        };
        // What ends up at `p` came from where going down brings it here.
        let from = p[1] - down(p[1] - down(p[1]));
        [0.0, from - p[1]]
    }

    /// Where `p` reads from with the head a part `larger`, and `lifted` on
    /// a neck that part of its length longer: about the middle of the
    /// shoulders, which stay where they are.
    fn headed(&self, p: Point, larger: f32, lifted: f32) -> Point {
        let Some((centre, radius)) = self.head else {
            return [0.0; 2];
        };
        let base = self.torso.from;
        let (up, length) = towards(base, centre);
        let lift = lifted * length;
        // Where the head is now.
        let to = length * (1.0 + larger) + lift;
        let radius = radius * (1.0 + larger);
        let (x, y) = (p[0] - base[0], p[1] - base[1]);
        let from_head = (x - up[0] * to).hypot(y - up[1] * to);
        // All of it on the head, and none at the shoulders or far from it.
        let weight = (1.0 - ramp(radius, radius * (1.0 + HEAD_CARRY), from_head)) * ramp(0.05 * length, 0.45 * length, x * up[0] + y * up[1]);
        [((x - up[0] * lift) / (1.0 + larger) - x) * weight, ((y - up[1] * lift) / (1.0 + larger) - y) * weight]
    }

    /// Where `p` reads from, for `shape`.
    fn offset(&self, p: Point, shape: &Shape) -> Point {
        let part = |amount: f32| amount / 100.0;
        let mut d = [0.0f32; 2];
        let mut add = |by: Point| d = [d[0] + by[0], d[1] + by[1]];
        if shape.shoulders != 0.0 || shape.waist != 0.0 || shape.hips != 0.0 {
            add(self.torso.widened(p, |s| {
                SHOULDERS * part(shape.shoulders) * bump(SHOULDER_LINE, s) + WAIST * part(shape.waist) * bump(WAIST_LINE, s) + HIPS * part(shape.hips) * bump(HIP_LINE, s)
            }));
        }
        for (limbs, wider) in [(&self.arms, ARMS * part(shape.arms)), (&self.legs, LEGS * part(shape.legs))] {
            if wider != 0.0 {
                limbs.iter().for_each(|limb| add(limb.widened(p, wider)));
            }
        }
        if shape.leg_length != 0.0 {
            // Between two legs, each has its say.
            let (mut sum, mut total) = ([0.0f32; 2], 0.0f32);
            for leg in &self.strides {
                let (by, weight) = leg.lengthened(p, LEG_LENGTH * part(shape.leg_length));
                sum = [sum[0] + by[0] * weight, sum[1] + by[1] * weight];
                total += weight;
            }
            add(sum.map(|v| v / total.max(1.0)));
        }
        if shape.head != 0.0 || shape.neck != 0.0 {
            add(self.headed(p, HEAD * part(shape.head), NECK * part(shape.neck)));
        }
        if shape.level != 0.0 {
            add(self.levelled(p, part(shape.level)));
        }
        // Outside the person (as they were) it fades out, the sooner the
        // less it is.
        let moved = d[0].hypot(d[1]);
        if moved == 0.0 {
            return d;
        }
        let fade = 1.0 - ramp(0.0, FADE * moved + self.outside.cell, self.outside.distance([p[0] + d[0], p[1] + d[1]]));
        [d[0] * fade, d[1] * fade]
    }
}

/// Add to `field` the warp that gives `body` its `shape`. For a quick look
/// at the display pyramid's `level`, it's worked out 2^`level` times as
/// coarsely: as close in the pixels shown as it is at full size.
pub fn reshape(field: &mut Field, body: &Body, shape: &Shape, level: u32) {
    if *shape == Shape::default() {
        return;
    }
    let area = [body.area[0].max(0.0) as u32, body.area[1].max(0.0) as u32, body.area[2].max(0.0).ceil() as u32, body.area[3].max(0.0).ceil() as u32];
    field.add(area, 1 << level, |p| body.offset(p, shape), |_| 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: usize = 256;
    /// The matte's square: the whole 1024 × 1024 image.
    const SIDE: f32 = 1024.0;

    /// A person standing in a 1024 × 1024 image, facing the camera, arms out
    /// a little: shoulders at y = 300 and hips at y = 500 either side of x =
    /// 512, a torso 160 wide, limbs 50 wide, legs down to y = 860, and a
    /// head 50 in radius at (512, 220). With `arms_down`, the arms hang
    /// against the body.
    fn joints(arms_down: bool) -> Joints {
        let seen = |x: f32, y: f32| [x, y, 1.0];
        let (elbow, wrist) = if arms_down { (112.0, 114.0) } else { (150.0, 220.0) };
        // The person's left is the image's right.
        let pair = |x: f32, y: f32| [seen(512.0 + x, y), seen(512.0 - x, y)];
        Joints {
            ears: pair(30.0, 220.0),
            shoulders: pair(80.0, 300.0),
            elbows: pair(elbow, 400.0),
            wrists: pair(wrist, 500.0),
            hips: pair(45.0, 500.0),
            knees: pair(50.0, 680.0),
            ankles: pair(55.0, 860.0),
        }
    }

    /// How far `p` is from the line from `a` to `b`.
    fn from_bone(p: Point, a: Joint, b: Joint) -> f32 {
        let (way, length) = towards([a[0], a[1]], [b[0], b[1]]);
        let t = ((p[0] - a[0]) * way[0] + (p[1] - a[1]) * way[1]).clamp(0.0, length);
        (p[0] - a[0] - way[0] * t).hypot(p[1] - a[1] - way[1] * t)
    }

    /// [`joints`]' person, drawn: within 25 of each limb's bones, 80 of the
    /// spine and 50 of the head's middle.
    fn cover(joints: &Joints) -> Vec<f32> {
        let cell = SIDE / SIZE as f32;
        (0..SIZE * SIZE)
            .map(|k| {
                let p = [((k % SIZE) as f32 + 0.5) * cell, ((k / SIZE) as f32 + 0.5) * cell];
                let limb = (0..2).any(|s| {
                    let bones = [
                        (joints.shoulders[s], joints.elbows[s]),
                        (joints.elbows[s], joints.wrists[s]),
                        (joints.hips[s], joints.knees[s]),
                        (joints.knees[s], joints.ankles[s]),
                    ];
                    bones.into_iter().any(|(a, b)| from_bone(p, a, b) <= 25.0)
                });
                let torso = (p[0] - 512.0).abs() <= 80.0 && (300.0..=520.0).contains(&p[1]);
                let neck = (p[0] - 512.0).abs() <= 20.0 && (220.0..=300.0).contains(&p[1]);
                let head = (p[0] - 512.0).hypot(p[1] - 220.0) <= 50.0;
                if limb || torso || neck || head { 1.0 } else { 0.0 }
            })
            .collect()
    }

    fn body(arms_down: bool) -> Body {
        let joints = joints(arms_down);
        let cover = cover(&joints);
        Body::new(&joints, &Matte { centre: [512.0; 2], side: SIDE, angle: 0.0, size: SIZE, cover: &cover }).unwrap()
    }

    /// The field that gives the person `shape`.
    fn field(body: &Body, shape: &Shape) -> Field {
        let mut field = Field::new(1024, 1024);
        reshape(&mut field, body, shape, 0);
        field
    }

    /// Where the image's `p` ends up: the point that reads from it.
    fn goes(field: &Field, p: Point) -> Point {
        let mut to = p;
        for _ in 0..20 {
            let d = field.at(to[0], to[1]);
            to = [p[0] - d[0], p[1] - d[1]];
        }
        to
    }

    #[test]
    fn a_bodys_parts_are_measured_from_its_matte() {
        let body = body(false);
        // The torso is 80 either side of the spine, and a limb 25.
        let waist = [body.torso.width(0.6, false), body.torso.width(0.6, true)];
        assert!(waist.iter().all(|w| (w - 80.0).abs() < 5.0), "{waist:?}");
        assert_eq!((body.arms.len(), body.legs.len(), body.strides.len()), (4, 4, 2));
        for limb in body.arms.iter().chain(&body.legs) {
            assert!((limb.tube.middle() - 25.0).abs() < 5.0, "{}", limb.tube.middle());
        }
        let (centre, radius) = body.head.unwrap();
        assert!((centre[0] - 512.0).abs() < 1e-3 && (radius - 50.0).abs() < 6.0, "{centre:?} {radius}");
        // On the person, and 100 px above their head.
        assert_eq!(body.outside.distance([512.0, 400.0]), 0.0);
        let above = body.outside.distance([512.0, 70.0]);
        assert!((above - 100.0).abs() < 4.0, "{above}");
    }

    #[test]
    fn arms_against_the_body_do_not_widen_it() {
        // The matte's edge is the arms' outer edge, 30 beyond the torso's.
        let body = body(true);
        let waist = body.torso.width(0.6, true);
        assert!(waist < 125.0, "{waist}");
        // An arm's inner side runs into the body: it's as wide as its outer.
        assert!((body.arms[0].tube.middle() - 25.0).abs() < 6.0, "{}", body.arms[0].tube.middle());
    }

    #[test]
    fn no_sliders_leave_no_warp() {
        assert_eq!(field(&body(false), &Shape::default()).extent(), None);
    }

    #[test]
    fn a_narrower_waist_leaves_the_shoulders_hips_and_background_alone() {
        let body = body(false);
        let field = field(&body, &Shape { waist: -100.0, ..Default::default() });
        // The waist's edges come in by a fifth, the same each side, and the
        // spine stays.
        let (left, right) = (goes(&field, [432.0, 424.0]), goes(&field, [592.0, 424.0]));
        assert!((left[0] - 448.0).abs() < 3.0 && (right[0] - 576.0).abs() < 3.0, "{left:?} {right:?}");
        assert!(left[1] == 424.0 && goes(&field, [512.0, 424.0]) == [512.0, 424.0]);
        // Shoulders, hips, head and legs stay.
        for p in [[432.0, 300.0], [592.0, 300.0], [440.0, 520.0], [512.0, 220.0], [462.0, 700.0]] {
            assert_eq!(field.at(p[0], p[1]), [0.0; 2], "{p:?}");
        }
        // The background behind goes with the edge beside it, less the
        // further out, and not at all 5 moves' lengths from the person: here
        // beyond their arm, which is at x = 654 to 704.
        let (near, far) = (field.at(600.0, 424.0), field.at(770.0, 424.0));
        assert!(near[0] > 4.0 && near[0] < 16.0 && far == [0.0; 2], "{near:?} {far:?}");
        let extent = field.extent().unwrap();
        assert!(extent[0] > 230 && extent[2] < 794 && extent[1] > 300 && extent[3] < 520, "{extent:?}");
    }

    #[test]
    fn an_arm_beside_a_narrower_waist_comes_in_with_it() {
        let body = body(true);
        let field = field(&body, &Shape { waist: -100.0, ..Default::default() });
        // The arm hangs at x = 400 to 450 at the waist: both its edges come
        // in, by nearly as much.
        let (outer, inner) = (goes(&field, [402.0, 424.0]), goes(&field, [448.0, 424.0]));
        assert!(outer[0] - 402.0 > 8.0 && inner[0] - 448.0 > 8.0, "{outer:?} {inner:?}");
        assert!(((outer[0] - 402.0) - (inner[0] - 448.0)).abs() < 4.0, "{outer:?} {inner:?}");
    }

    #[test]
    fn hips_and_shoulders_widen_where_they_are() {
        let body = body(false);
        let wider = field(&body, &Shape { hips: 100.0, shoulders: 100.0, ..Default::default() });
        let (hip, shoulder, waist) = (goes(&wider, [434.0, 510.0]), goes(&wider, [434.0, 302.0]), goes(&wider, [434.0, 424.0]));
        assert!(434.0 - hip[0] > 8.0 && 434.0 - shoulder[0] > 6.0, "{hip:?} {shoulder:?}");
        assert!((434.0 - waist[0]).abs() < 1.5, "{waist:?}");
    }

    #[test]
    fn thinner_arms_and_legs_narrow_about_their_bones() {
        let body = body(false);
        let field = field(&body, &Shape { arms: -100.0, legs: -100.0, ..Default::default() });
        // The person's left calf is about x = 564.5 at y = 770, 25 either
        // side: both edges come in by a fifth, and the bone stays.
        let (inner, outer, bone) = (goes(&field, [541.0, 770.0]), goes(&field, [588.0, 770.0]), goes(&field, [564.5, 770.0]));
        assert!((inner[0] - 545.7).abs() < 1.5 && (outer[0] - 583.3).abs() < 1.5, "{inner:?} {outer:?}");
        assert!((bone[0] - 564.5).abs() < 0.5, "{bone:?}");
        // A forearm, by a quarter; the torso and head stay.
        let forearm = [512.0 + 185.0, 450.0];
        let arm = &body.arms[1].tube;
        let (s, r) = arm.place(forearm);
        assert!((0.3..0.7).contains(&s) && r.abs() < 1.0, "{s} {r}");
        let edge = [forearm[0] + arm.along[1] * 22.0, forearm[1] - arm.along[0] * 22.0];
        let to = goes(&field, edge);
        assert!((from_bone(to, joints(false).elbows[0], joints(false).wrists[0]) - 16.5).abs() < 1.5, "{to:?}");
        assert_eq!((field.at(512.0, 400.0), field.at(512.0, 220.0)), ([0.0; 2], [0.0; 2]));
    }

    #[test]
    fn longer_legs_carry_the_feet_down_and_leave_the_hips() {
        let body = body(false);
        let field = field(&body, &Shape { leg_length: 100.0, ..Default::default() });
        // 360 from hip to ankle: the ankle goes 29 down, the knee half that.
        let (ankle, knee) = (goes(&field, [567.0, 860.0]), goes(&field, [562.0, 680.0]));
        assert!((ankle[1] - 860.0 - 28.8).abs() < 2.0 && (knee[1] - 680.0 - 14.4).abs() < 2.0, "{ankle:?} {knee:?}");
        // Both legs alike, and between them.
        let other = goes(&field, [457.0, 860.0]);
        assert!((other[1] - ankle[1]).abs() < 0.5, "{other:?}");
        let gap = goes(&field, [512.0, 800.0]);
        assert!((gap[1] - 800.0 - 22.0).abs() < 3.0, "{gap:?}");
        // The hips and all above them stay, and so does what's well below
        // the feet.
        for p in [[512.0, 500.0], [467.0, 498.0], [512.0, 300.0], [512.0, 1020.0]] {
            let d = field.at(p[0], p[1]);
            assert!(d[0].hypot(d[1]) < 0.3, "{p:?}: {d:?}");
        }
    }

    #[test]
    fn a_larger_head_grows_from_the_shoulders_and_a_longer_neck_lifts_it() {
        let body = body(false);
        let larger = field(&body, &Shape { head: 100.0, ..Default::default() });
        // 80 above the shoulders, 12 % larger: the middle goes up 9.6, the
        // top 15.6 and each side out 6.
        let (middle, top, side) = (goes(&larger, [512.0, 220.0]), goes(&larger, [512.0, 172.0]), goes(&larger, [560.0, 220.0]));
        assert!((220.0 - middle[1] - 9.6).abs() < 1.0 && (172.0 - top[1] - 15.4).abs() < 1.5, "{middle:?} {top:?}");
        assert!((side[0] - 560.0 - 5.8).abs() < 1.0, "{side:?}");
        let lifted = field(&body, &Shape { neck: 100.0, ..Default::default() });
        let (middle, side) = (goes(&lifted, [512.0, 220.0]), goes(&lifted, [560.0, 220.0]));
        assert!((220.0 - middle[1] - 9.6).abs() < 1.0 && (side[0] - 560.0).abs() < 0.5, "{middle:?} {side:?}");
        // Either way the shoulders and all below stay.
        for field in [&larger, &lifted] {
            assert_eq!((field.at(512.0, 304.0), field.at(440.0, 320.0), field.at(512.0, 600.0)), ([0.0; 2], [0.0; 2], [0.0; 2]));
        }
    }

    /// [`body`], one shoulder `drop` px below where it was and the other as
    /// far above: the person's left, on the right of the picture, the lower.
    fn leaning(drop: f32) -> Body {
        let mut joints = joints(false);
        joints.shoulders[0][1] += drop;
        joints.shoulders[1][1] -= drop;
        let mut cover = cover(&joints);
        // Their torso goes up to the higher shoulder.
        let cell = SIDE / SIZE as f32;
        for (k, v) in cover.iter_mut().enumerate() {
            let p = [((k % SIZE) as f32 + 0.5) * cell, ((k / SIZE) as f32 + 0.5) * cell];
            if (p[0] - 512.0).abs() <= 80.0 && (300.0 - drop.abs()..=300.0).contains(&p[1]) {
                *v = 1.0;
            }
        }
        Body::new(&joints, &Matte { centre: [512.0; 2], side: SIDE, angle: 0.0, size: SIZE, cover: &cover }).unwrap()
    }

    #[test]
    fn levelled_shoulders_meet_half_way_and_leave_the_head_and_waist() {
        let body = leaning(10.0);
        assert!((body.shoulder_tilt() - 7.1).abs() < 0.1, "{}", body.shoulder_tilt());
        let level = field(&body, &Shape { level: 100.0, ..Default::default() });
        // The shoulders, at y = 290 and 310, meet at 300, and half way with
        // the slider half way.
        let (high, low) = (goes(&level, [432.0, 290.0]), goes(&level, [592.0, 310.0]));
        assert!((high[1] - 300.0).abs() < 1.0 && (low[1] - 300.0).abs() < 1.0, "{high:?} {low:?}");
        assert_eq!((high[0], low[0]), (432.0, 592.0));
        let half = field(&body, &Shape { level: 50.0, ..Default::default() });
        let low = goes(&half, [592.0, 310.0]);
        assert!((low[1] - 305.0).abs() < 1.0, "{low:?}");
        // The top of an arm goes with its shoulder, and the chest below
        // less: between them nothing's torn.
        let (arm, chest) = (goes(&level, [612.0, 330.0]), goes(&level, [580.0, 380.0]));
        assert!((330.0 - arm[1] - 9.0).abs() < 1.5 && (2.0..8.0).contains(&(380.0 - chest[1])), "{arm:?} {chest:?}");
        // The head, the spine, the waist and all below stay.
        for p in [[512.0, 220.0], [550.0, 230.0], [512.0, 320.0], [440.0, 480.0], [590.0, 480.0], [467.0, 700.0]] {
            assert_eq!(level.at(p[0], p[1]), [0.0; 2], "{p:?}");
        }
        // Level already, there's nothing to do; leaning far, nothing's done.
        let none = [body.clone(), leaning(0.0), leaning(60.0)].map(|body| field(&body, &Shape { level: 100.0, ..Default::default() }).at(592.0, 310.0));
        assert!(none[0][1] > 5.0 && none[1] == [0.0; 2] && none[2] == [0.0; 2], "{none:?}");
        assert!(leaning(60.0).shoulder_tilt() > LEVELLED);
    }

    #[test]
    fn a_body_with_no_legs_in_view_has_no_leg_sliders() {
        let mut joints = joints(false);
        for joint in joints.knees.iter_mut().chain(&mut joints.ankles) {
            joint[2] = 0.1;
        }
        let cover = cover(&joints);
        let matte = Matte { centre: [512.0; 2], side: SIDE, angle: 0.0, size: SIZE, cover: &cover };
        let body = Body::new(&joints, &matte).unwrap();
        assert!(body.legs.is_empty() && body.strides.is_empty() && body.arms.len() == 4);
        assert_eq!(field(&body, &Shape { legs: 100.0, leg_length: 100.0, ..Default::default() }).extent(), None);
        // And without both shoulders there's nobody to shape.
        joints.shoulders[0][2] = 0.1;
        assert!(Body::new(&joints, &matte).is_none());
    }

    #[test]
    fn a_quick_look_is_nearly_the_same() {
        let body = body(false);
        let shape = Shape { waist: -100.0, legs: -60.0, leg_length: 50.0, head: 40.0, ..Default::default() };
        let fine = field(&body, &shape);
        let mut coarse = Field::new(1024, 1024);
        reshape(&mut coarse, &body, &shape, 2);
        for p in [[440.0, 424.0], [590.0, 770.0], [512.0, 180.0], [567.0, 860.0], [620.0, 424.0]] {
            let (a, b) = (fine.at(p[0], p[1]), coarse.at(p[0], p[1]));
            assert!((a[0] - b[0]).hypot(a[1] - b[1]) < 1.5, "{p:?}: {a:?} {b:?}");
        }
    }

    #[test]
    fn a_turned_matte_is_read_where_it_lies() {
        // The same person, their matte's square turned a quarter: its rows
        // run down the image.
        let joints = joints(false);
        let upright = cover(&joints);
        let turned: Vec<f32> = (0..SIZE * SIZE).map(|k| upright[(k % SIZE) * SIZE + SIZE - 1 - k / SIZE]).collect();
        let matte = Matte { centre: [512.0; 2], side: SIDE, angle: std::f32::consts::FRAC_PI_2, size: SIZE, cover: &turned };
        let body = Body::new(&joints, &matte).unwrap();
        let waist = body.torso.width(0.6, true);
        assert!((waist - 80.0).abs() < 5.0, "{waist}");
    }
}
