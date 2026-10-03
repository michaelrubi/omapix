//! Face-Aware Liquify and Face Symmetry (docs/AI.md, features 7 and 8): a
//! face's points moved by sliders, and the rest of the face carried along
//! with them as a warp ([`crate::warp`]).

use crate::warp::Field;

/// x, y and depth, in image pixels.
type Point = [f32; 3];

/// A feature's points: those on the left of the image, their mirror images
/// on the right in the same order, and those on the face's midline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sides {
    pub left: Vec<Point>,
    pub right: Vec<Point>,
    pub middle: Vec<Point>,
}

impl Sides {
    fn points(&self) -> impl Iterator<Item = &Point> {
        self.left.iter().chain(&self.right).chain(&self.middle)
    }

    fn points_mut(&mut self) -> impl Iterator<Item = &mut Point> {
        self.left.iter_mut().chain(&mut self.right).chain(&mut self.middle)
    }
}

/// A face's points, feature by feature.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Face {
    /// Round each eye's opening.
    pub eyes: Sides,
    pub brows: Sides,
    /// Down the bridge, and round the wings, nostrils and tip.
    pub nose: Sides,
    /// Round the outside of the lips.
    pub lips: Sides,
    /// Round their inner edges, point for point as `lips`.
    pub mouth: Sides,
    /// Round the face, from the top of the forehead to the chin.
    pub outline: Sides,
}

impl Face {
    fn features(&self) -> [&Sides; 6] {
        [&self.eyes, &self.brows, &self.nose, &self.lips, &self.mouth, &self.outline]
    }

    fn points(&self) -> impl Iterator<Item = &Point> {
        self.features().into_iter().flat_map(Sides::points)
    }
}

/// How much alike a face's two sides are made, feature by feature, 0–100:
/// at 100 they mirror each other, each having gone half way.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Symmetry {
    pub eyes: f32,
    pub brows: f32,
    pub nose: f32,
    pub mouth: f32,
    pub jaw: f32,
}

/// How a face is reshaped: its symmetry, then each of Face-Aware Liquify's
/// sliders, from −100 to 100. 0 leaves it as it is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shape {
    pub symmetry: Symmetry,
    pub eye_size: f32,
    pub eye_distance: f32,
    pub nose_length: f32,
    pub nose_width: f32,
    pub smile: f32,
    pub lips: f32,
    pub mouth_width: f32,
    pub forehead: f32,
    pub chin: f32,
    pub jaw: f32,
    pub face_width: f32,
}

/// What each slider does at 100: sizes and widths as a part of the
/// feature's own, the rest as a part of the distance between the eyes.
const EYE_SIZE: f32 = 0.2;
const EYE_DISTANCE: f32 = 0.08;
const NOSE_LENGTH: f32 = 0.1;
const NOSE_WIDTH: f32 = 0.25;
const SMILE: f32 = 0.1;
const LIPS: f32 = 0.35;
const MOUTH_WIDTH: f32 = 0.15;
const FOREHEAD: f32 = 0.15;
const CHIN: f32 = 0.12;
const JAW: f32 = 0.15;
const FACE_WIDTH: f32 = 0.12;

/// How far a face is turned from the camera (the sine of the angle) where
/// its symmetry starts to be held back, and where nothing is done.
const TURNED: [f32; 2] = [0.1, 0.4];

/// How far round a face its warp reaches, and from where it fades out, in
/// multiples of the ellipse round the face's outline. The room is for the
/// outline to move into, and for what's beyond it to stretch over.
const REACH: f32 = 1.6;
const FADE: f32 = 1.3;
/// How many times the distance between the eyes holds the distance between
/// the points the warp's spline is worked out at (at least the field's
/// grid): at 100, a face with every slider at its end is within a pixel of
/// the spline everywhere, and at 50 up to 3 px out beside its eyes.
const LATTICE: f32 = 100.0;
/// Points pinned round the warp's edge.
const PINS: usize = 32;

/// A face's own axes, about the point between its eyes: across, from the
/// eye on the left to the other, and down.
struct Frame {
    origin: [f32; 2],
    across: [f32; 2],
    /// The distance between the eyes.
    iod: f32,
}

impl Frame {
    fn of(face: &Face) -> Self {
        let (l, r) = (middle(&face.eyes.left), middle(&face.eyes.right));
        let (dx, dy) = (r[0] - l[0], r[1] - l[1]);
        let iod = dx.hypot(dy).max(1e-3);
        Self { origin: [(l[0] + r[0]) / 2.0, (l[1] + r[1]) / 2.0], across: [dx / iod, dy / iod], iod }
    }

    /// How far across and down (x, y) is.
    fn place(&self, x: f32, y: f32) -> [f32; 2] {
        let (x, y) = (x - self.origin[0], y - self.origin[1]);
        [x * self.across[0] + y * self.across[1], y * self.across[0] - x * self.across[1]]
    }

    /// The point `u` across and `v` down.
    fn at(&self, u: f32, v: f32) -> [f32; 2] {
        let mut p = [self.origin[0], self.origin[1], 0.0];
        self.shift(&mut p, u, v);
        [p[0], p[1]]
    }

    /// Move `p` by `u` across and `v` down.
    fn shift(&self, p: &mut Point, u: f32, v: f32) {
        p[0] += u * self.across[0] - v * self.across[1];
        p[1] += u * self.across[1] + v * self.across[0];
    }

    /// Where each of `points` is, across and down.
    fn places<'a>(&self, points: impl Iterator<Item = &'a Point>) -> Vec<[f32; 2]> {
        points.map(|p| self.place(p[0], p[1])).collect()
    }
}

/// The middle of `points`, in the image.
fn middle(points: &[Point]) -> [f32; 2] {
    let n = points.len().max(1) as f32;
    [0, 1].map(|c| points.iter().map(|p| p[c]).sum::<f32>() / n)
}

/// The least, the middle and the greatest of `places`' across (`c` = 0) or
/// down (1).
fn span(places: &[[f32; 2]], c: usize) -> [f32; 3] {
    let lo = places.iter().map(|p| p[c]).fold(f32::MAX, f32::min);
    let hi = places.iter().map(|p| p[c]).fold(f32::MIN, f32::max);
    [lo, (lo + hi) / 2.0, hi]
}

/// From 0 at `a` to 1 at `b`, smoothly, and those beyond.
fn ramp(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Make `face`'s two sides alike, in depth too: a face turned a little
/// from the camera, whose far side looks narrower, is as symmetrical as it
/// would be facing it.
fn symmetrise(face: &mut Face, amounts: &Symmetry) {
    if *amounts == Symmetry::default() {
        return;
    }
    // Each pair's middle and the way from its left to its right, and the
    // midline's points.
    let pairs = |sides: &Sides| -> Vec<(Point, Point)> {
        let pairs = sides.left.iter().zip(&sides.right);
        let pairs = pairs.map(|(l, r)| ([0, 1, 2].map(|c| (l[c] + r[c]) / 2.0), [0, 1, 2].map(|c| r[c] - l[c])));
        pairs.chain(sides.middle.iter().map(|m| (*m, [0.0; 3]))).collect()
    };
    let pairs: Vec<_> = face.features().into_iter().flat_map(pairs).collect();
    let n = pairs.len().max(1) as f64;
    let centre = [0, 1, 2].map(|c| pairs.iter().map(|(m, _)| f64::from(m[c])).sum::<f64>() / n);
    // The mirror plane that brings each point nearest its pair's: through
    // the middles' centre, facing the way that's most along the pairs and
    // least along the spread of their middles (by least squares, the
    // greatest eigenvector of the matrix below, found by power iteration).
    let (mut matrix, mut spread, mut normal) = ([[0.0f64; 3]; 3], 0.0f64, [0.0f64; 3]);
    for (m, d) in &pairs {
        let (m, d) = ([0, 1, 2].map(|c| f64::from(m[c]) - centre[c]), d.map(f64::from));
        for i in 0..3 {
            for j in 0..3 {
                matrix[i][j] += d[i] * d[j] - 4.0 * m[i] * m[j];
            }
            spread += 4.0 * m[i] * m[i];
            normal[i] += d[i];
        }
    }
    for _ in 0..64 {
        // Shifted by the spread, so the greatest is the largest too.
        let next = [0, 1, 2].map(|i| (0..3).map(|j| matrix[i][j] * normal[j]).sum::<f64>() + spread * normal[i]);
        let length = next.iter().map(|v| v * v).sum::<f64>().sqrt();
        if length == 0.0 {
            return;
        }
        normal = next.map(|v| v / length);
    }
    // How far `p` is from the plane, to the right.
    let beyond = |p: &Point| (0..3).map(|c| (f64::from(p[c]) - centre[c]) * normal[c]).sum::<f64>() as f32;
    let normal = normal.map(|v| v as f32);
    let mirrored = |p: &Point| {
        let d = beyond(p);
        [0, 1, 2].map(|c| p[c] - 2.0 * d * normal[c])
    };
    let Face { eyes, brows, nose, lips, mouth, outline } = face;
    let features = [
        (eyes, amounts.eyes),
        (brows, amounts.brows),
        (nose, amounts.nose),
        (lips, amounts.mouth),
        (mouth, amounts.mouth),
        (outline, amounts.jaw),
    ];
    // The far side of a face turned from the camera is the landmarker's
    // guess, and looks uneven when it isn't: less is done as it turns.
    let facing = ramp(TURNED[1], TURNED[0], normal[2].abs());
    for (sides, amount) in features {
        let k = amount / 100.0 * facing;
        if k == 0.0 {
            continue;
        }
        // Each side goes half way to the other's mirror image, so neither
        // is simply copied.
        let half_way = |from: &[Point], other: &[Point]| -> Vec<Point> {
            let pairs = from.iter().zip(other).map(|(p, o)| (p, mirrored(o)));
            pairs.map(|(p, to)| [0, 1, 2].map(|c| p[c] + k / 2.0 * (to[c] - p[c]))).collect()
        };
        let (to_left, to_right) = (half_way(&sides.left, &sides.right), half_way(&sides.right, &sides.left));
        fit(&mut sides.left, &to_left);
        fit(&mut sides.right, &to_right);
        for m in &mut sides.middle {
            let d = beyond(m);
            for c in 0..3 {
                m[c] -= k * d * normal[c];
            }
        }
    }
}

/// Move `points` as one towards `to`: shifted, turned and scaled the way
/// that brings them nearest (by least squares). Point by point they'd each
/// go their own way, and an outline would come out wavy.
fn fit(points: &mut [Point], to: &[Point]) {
    let (from, towards) = (middle(points), middle(to));
    let (mut along, mut turned, mut spread) = (0.0, 0.0, 0.0);
    for (p, t) in points.iter().zip(to) {
        let (p, t) = ([p[0] - from[0], p[1] - from[1]], [t[0] - towards[0], t[1] - towards[1]]);
        along += p[0] * t[0] + p[1] * t[1];
        turned += p[0] * t[1] - p[1] * t[0];
        spread += p[0] * p[0] + p[1] * p[1];
    }
    let (along, turned) = if spread > 0.0 { (along / spread, turned / spread) } else { (1.0, 0.0) };
    for (p, t) in points.iter_mut().zip(to) {
        let (x, y) = (p[0] - from[0], p[1] - from[1]);
        *p = [towards[0] + along * x - turned * y, towards[1] + turned * x + along * y, t[2]];
    }
}

/// Where `face`'s points go with `shape`.
fn shaped(face: &Face, shape: &Shape) -> Face {
    let mut to = face.clone();
    symmetrise(&mut to, &shape.symmetry);
    let frame = Frame::of(&to);
    let iod = frame.iod;
    let part = |amount: f32| amount / 100.0;

    // Each eye about its own middle, and apart.
    for (eye, out) in [(&mut to.eyes.left, -1.0), (&mut to.eyes.right, 1.0)] {
        let centre = middle(eye);
        for p in eye {
            for c in 0..2 {
                p[c] += (p[c] - centre[c]) * EYE_SIZE * part(shape.eye_size);
            }
            frame.shift(p, out * EYE_DISTANCE * iod * part(shape.eye_distance), 0.0);
        }
    }

    // The nose: nothing at the top of its bridge, and the most at its base.
    let places = frame.places(to.nose.points());
    let ([_, across, _], [top, _, base]) = (span(&places, 0), span(&places, 1));
    for (p, [u, v]) in to.nose.points_mut().zip(places) {
        let w = ((v - top) / (base - top)).clamp(0.0, 1.0);
        let wider = (u - across) * NOSE_WIDTH * part(shape.nose_width);
        frame.shift(p, wider * w, NOSE_LENGTH * iod * part(shape.nose_length) * w);
    }

    // The lips, fuller away from the mouth between them; then both wider,
    // and their corners up.
    for (outer, inner) in to.lips.points_mut().zip(to.mouth.points()) {
        for c in 0..2 {
            outer[c] += (outer[c] - inner[c]) * LIPS * part(shape.lips);
        }
    }
    let [left, across, _] = span(&frame.places(to.lips.points()), 0);
    for p in to.lips.points_mut().chain(to.mouth.points_mut()) {
        let [u, _] = frame.place(p[0], p[1]);
        let t = (u - across) / (across - left);
        frame.shift(p, (u - across) * MOUTH_WIDTH * part(shape.mouth_width), -SMILE * iod * part(shape.smile) * t * t);
    }

    // The outline: its width from the eyes down, the jaw's from the mouth
    // down, and the forehead and chin along the face.
    let [_, mouth, _] = span(&frame.places(to.mouth.points()), 1);
    let places = frame.places(to.outline.points());
    let ([_, across, _], [top, _, chin]) = (span(&places, 0), span(&places, 1));
    for (p, [u, v]) in to.outline.points_mut().zip(places) {
        let wider = FACE_WIDTH * part(shape.face_width) * ramp(top / 2.0, 0.0, v) + JAW * part(shape.jaw) * ramp(0.0, mouth, v);
        let down = CHIN * part(shape.chin) * ramp(mouth, chin, v) - FOREHEAD * part(shape.forehead) * ramp(0.0, top, v);
        frame.shift(p, (u - across) * wider, down * iod);
    }
    to
}

/// Add to `field` the warp that gives `face` its `shape`: its points go
/// where the shape puts them, the rest of the face follows, and it fades
/// to nothing round the face. For a quick look at the display pyramid's
/// `level`, it's worked out 2^`level` times as coarsely: as close in the
/// pixels shown as it is at full size.
pub fn reshape(field: &mut Field, face: &Face, shape: &Shape, level: u32) {
    if *shape == Shape::default() {
        return;
    }
    let to = shaped(face, shape);
    let mut moves: Vec<_> = face.points().zip(to.points()).map(|(a, b)| ([a[0], a[1]], [b[0], b[1]])).collect();
    // Less than a hundredth of a pixel is rounding.
    if moves.iter().all(|(a, b)| (b[0] - a[0]).hypot(b[1] - a[1]) < 0.01) {
        return;
    }
    // An ellipse round the outline, in the face's own axes: pinned along
    // its reach.
    let frame = Frame::of(face);
    let places = frame.places(face.outline.points());
    let ([left, across, _], [top, down, _]) = (span(&places, 0), span(&places, 1));
    let radii = [across - left, down - top];
    let pins = (0..PINS).map(|k| {
        let (sin, cos) = (k as f32 / PINS as f32 * std::f32::consts::TAU).sin_cos();
        frame.at(across + REACH * radii[0] * cos, down + REACH * radii[1] * sin)
    });
    moves.extend(pins.map(|p| (p, p)));
    let pinned = &moves[moves.len() - PINS..];
    let bound = |c: usize, least: bool| {
        let values = pinned.iter().map(|(p, _)| p[c]);
        if least { values.fold(f32::MAX, f32::min).max(0.0) as u32 } else { values.fold(0.0, f32::max).ceil() as u32 }
    };
    let area = [bound(0, true), bound(1, true), bound(0, false), bound(1, false)];
    let every = ((frame.iod / (LATTICE * crate::warp::STEP as f32)) as usize).max(1) << level;
    field.move_points(&moves, area, every, |[x, y]| {
        let [u, v] = frame.place(x, y);
        ramp(REACH, FADE, ((u - across) / radii[0]).hypot((v - down) / radii[1]))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pixel;
    use crate::tiled::Tiled;
    use crate::warp::warped;

    /// `n` points down the left half of an ellipse, from just after its top
    /// to just before its bottom.
    fn half(cx: f32, cy: f32, rx: f32, ry: f32, n: usize) -> Vec<Point> {
        (1..=n)
            .map(|k| {
                let (sin, cos) = (k as f32 / (n + 1) as f32 * std::f32::consts::PI).sin_cos();
                [cx - rx * sin, cy - ry * cos, 0.0]
            })
            .collect()
    }

    /// `left`, its mirror image in x = 300, and `middle`, x = 300 at each y.
    fn sides(left: Vec<Point>, middle: &[f32]) -> Sides {
        let right = left.iter().map(|p| [600.0 - p[0], p[1], p[2]]).collect();
        Sides { left, right, middle: middle.iter().map(|&y| [300.0, y, 0.0]).collect() }
    }

    /// A face looking straight at the camera, the same either side of x =
    /// 300: eyes 100 apart at y = 300, 36 × 16; a nose from there to y =
    /// 365; lips 70 × 28 round a mouth 60 × 10 at y = 400; and an outline
    /// 220 × 300, from y = 170 to 470.
    fn face() -> Face {
        let ring = |cx: f32, cy: f32, rx: f32, ry: f32| -> Vec<Point> {
            (0..8)
                .map(|k| {
                    let (sin, cos) = (k as f32 / 8.0 * std::f32::consts::TAU).sin_cos();
                    [cx + rx * cos, cy + ry * sin, 0.0]
                })
                .collect()
        };
        Face {
            eyes: sides(ring(250.0, 300.0, 18.0, 8.0), &[]),
            brows: sides((0..5).map(|k| [225.0 + 12.0 * k as f32, 275.0, 0.0]).collect(), &[]),
            nose: sides(vec![[288.0, 350.0, 0.0], [285.0, 362.0, 0.0]], &[300.0, 330.0, 355.0, 365.0]),
            lips: sides(half(300.0, 400.0, 35.0, 14.0, 5), &[386.0, 414.0]),
            mouth: sides(half(300.0, 400.0, 30.0, 5.0, 5), &[395.0, 405.0]),
            outline: sides(half(300.0, 320.0, 110.0, 150.0, 9), &[170.0, 470.0]),
        }
    }

    /// How far the furthest of `a`'s points is from where it is in `b`.
    fn apart(a: &Face, b: &Face) -> f32 {
        a.points().zip(b.points()).map(|(a, b)| (a[0] - b[0]).hypot(a[1] - b[1])).fold(0.0, f32::max)
    }

    /// `face` with its cheeks further back than its nose, turned `degrees`
    /// to one side.
    fn turned(mut face: Face, degrees: f32) -> Face {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let Face { eyes, brows, nose, lips, mouth, outline } = &mut face;
        for p in [eyes, brows, nose, lips, mouth, outline].into_iter().flat_map(Sides::points_mut) {
            let (x, z) = (p[0] - 300.0, ((p[0] - 300.0) / 10.0).powi(2));
            (p[0], p[2]) = (300.0 + x * cos - z * sin, x * sin + z * cos);
        }
        face
    }

    const ALL: Symmetry = Symmetry { eyes: 100.0, brows: 100.0, nose: 100.0, mouth: 100.0, jaw: 100.0 };

    #[test]
    fn symmetry_leaves_a_symmetrical_face_as_it_is_even_turned() {
        let symmetry = Shape { symmetry: ALL, ..Default::default() };
        // With cheeks further back than the nose, turned 25° to one side.
        let turned = turned(face(), 25.0);
        let narrower = turned.outline.right[4][0] - 300.0 < 300.0 - turned.outline.left[4][0] - 20.0;
        assert!(narrower, "one side looks narrower");
        for face in [face(), turned] {
            assert!(apart(&shaped(&face, &symmetry), &face) < 0.01);
            let mut field = Field::new(600, 600);
            reshape(&mut field, &face, &symmetry, 0);
            assert_eq!(field.extent(), None);
        }
    }

    #[test]
    fn symmetry_brings_each_side_half_way_to_the_other() {
        // The eye on the left sits 10 px higher.
        let mut uneven = face();
        for p in &mut uneven.eyes.left {
            p[1] -= 10.0;
        }
        let eyes = |amount| Shape { symmetry: Symmetry { eyes: amount, ..Default::default() }, ..Default::default() };
        let height = |eye: &[Point]| middle(eye)[1];
        let even = shaped(&uneven, &eyes(100.0));
        let (l, r) = (height(&even.eyes.left), height(&even.eyes.right));
        assert!((l - r).abs() < 1.0 && (l - 295.0).abs() < 1.0, "{l} {r}");
        let half = shaped(&uneven, &eyes(50.0));
        assert!((height(&half.eyes.left) - 292.5).abs() < 1.0 && (height(&half.eyes.right) - 297.5).abs() < 1.0);
        // Only the eyes: the rest is where it was.
        assert_eq!((&even.brows, &even.nose, &even.lips, &even.outline), (&uneven.brows, &uneven.nose, &uneven.lips, &uneven.outline));
        // Each side moves as one: the eye keeps its shape.
        let width = |eye: &[Point]| eye[0][0] - eye[4][0];
        assert!((width(&even.eyes.left) - width(&uneven.eyes.left)).abs() < 0.1);
        // Turned far from the camera, where its points can't be trusted,
        // it's left as it is; turned a little, less is done.
        let far = turned(uneven.clone(), 35.0);
        assert_eq!(shaped(&far, &eyes(100.0)), far);
        let (a_little, was) = (shaped(&turned(uneven.clone(), 12.0), &eyes(100.0)), turned(uneven.clone(), 12.0));
        let rise = height(&a_little.eyes.right) - height(&was.eyes.right);
        assert!(rise < -1.0 && rise > -4.0, "{rise}");
        // A midline point off the midline goes back onto it.
        let mut bent = face();
        bent.nose.middle[3][0] += 6.0;
        let straight = shaped(&bent, &Shape { symmetry: Symmetry { nose: 100.0, ..Default::default() }, ..Default::default() });
        assert!((straight.nose.middle[3][0] - 300.0).abs() < 0.5, "{:?}", straight.nose.middle[3]);
    }

    #[test]
    fn each_slider_moves_its_own_feature() {
        let face = face();
        let width = |points: &[Point]| span(&points.iter().map(|p| [p[0], p[1]]).collect::<Vec<_>>(), 0);
        let near = |a: f32, b: f32| (a - b).abs() < 0.05;

        let to = shaped(&face, &Shape { eye_size: 100.0, eye_distance: 100.0, ..Default::default() });
        let [l, m, r] = width(&to.eyes.left);
        assert!(near(r - l, 36.0 * 1.2) && near(m, 250.0 - 8.0), "{l} {m} {r}");
        assert!(near(width(&to.eyes.right)[1], 350.0 + 8.0));
        assert_eq!((&to.brows, &to.nose, &to.outline), (&face.brows, &face.nose, &face.outline));

        let to = shaped(&face, &Shape { nose_width: -100.0, nose_length: 100.0, ..Default::default() });
        assert_eq!(to.nose.middle[0], face.nose.middle[0], "the top of the bridge stays");
        assert!(near(to.nose.middle[3][1], 375.0) && near(to.nose.left[1][0], 300.0 - 15.0 * (1.0 - 0.25 * 62.0 / 65.0)));

        let to = shaped(&face, &Shape { smile: 100.0, lips: 100.0, mouth_width: 100.0, ..Default::default() });
        // The corner is the third point down each side.
        let (corner, was) = (to.lips.left[2], face.lips.left[2]);
        assert!(near(corner[1], was[1] - 10.0) && near(300.0 - corner[0], (35.0 + 5.0 * 0.35) * 1.15), "{corner:?}");
        assert!(near(to.lips.middle[0][1], 386.0 - 9.0 * 0.35) && near(to.lips.middle[1][1], 414.0 + 9.0 * 0.35));
        assert_eq!(to.mouth.middle, face.mouth.middle);

        let to = shaped(&face, &Shape { forehead: 100.0, chin: 100.0, ..Default::default() });
        assert!(near(to.outline.middle[0][1], 170.0 - 15.0) && near(to.outline.middle[1][1], 470.0 + 12.0));
        // The widest of the outline is at eye level, the fifth point down.
        assert!(near(to.outline.left[4][1], face.outline.left[4][1]));

        let to = shaped(&face, &Shape { face_width: -100.0, ..Default::default() });
        assert!(near(300.0 - to.outline.left[4][0], 110.0 * 0.88));
        assert_eq!(to.outline.middle, face.outline.middle);
        let to = shaped(&face, &Shape { jaw: -100.0, ..Default::default() });
        assert!(300.0 - to.outline.left[4][0] > 108.0, "not at the cheekbones");
        let (jaw, was) = (to.outline.left[7][0], face.outline.left[7][0]);
        assert!(near(300.0 - jaw, (300.0 - was) * 0.85), "{jaw} {was}");
    }

    #[test]
    fn reshaping_warps_the_face_and_nothing_far_from_it() {
        const RED: Pixel = [65535, 0, 0, 65535];
        const GREY: Pixel = [30000, 30000, 30000, 65535];
        // A red eye, 16 px across, in a grey face.
        let pixels: Vec<Pixel> = (0..600 * 600)
            .map(|i| {
                let (x, y) = ((i % 600) as f32 - 250.0, (i / 600) as f32 - 300.0);
                if x.hypot(y) <= 8.0 { RED } else { GREY }
            })
            .collect();
        let image = Tiled::from_slice(600, 600, [0; 4], &pixels);
        let face = face();
        let mut field = Field::new(600, 600);
        reshape(&mut field, &face, &Shape { eye_size: 100.0, eye_distance: 100.0, ..Default::default() }, 0);
        // It's bigger, and further out; the eyebrow above has stayed.
        let out = warped(&image, &field);
        assert_eq!((out.get(242, 300), out.get(235, 300), out.get(242, 307)), (RED, RED, RED));
        assert_eq!((out.get(258, 300), out.get(242, 313)), (GREY, GREY));
        let brow = field.at(249.0, 275.0);
        assert!(brow[0].abs() < 0.5 && brow[1].abs() < 0.5, "{brow:?}");
        // Within 1.6 of the outline's ellipse: 176 px either side of 300,
        // and 240 above and below 320.
        let [x0, y0, x1, y1] = field.extent().unwrap();
        assert!(x0 >= 120 && y0 >= 76 && x1 <= 481 && y1 <= 561, "{:?}", field.extent());
        // Other faces' warps add to it.
        let before = field.at(232.0, 300.0);
        reshape(&mut field, &face, &Shape { eye_size: 100.0, eye_distance: 100.0, ..Default::default() }, 0);
        assert!((field.at(232.0, 300.0)[0] - 2.0 * before[0]).abs() < 1e-3);
        // For a quick look it's coarser, and much the same.
        let mut quick = Field::new(600, 600);
        reshape(&mut quick, &face, &Shape { eye_size: 100.0, eye_distance: 100.0, ..Default::default() }, 2);
        let (quick, exact) = (quick.at(232.0, 300.0), before);
        assert!(quick != exact && (quick[0] - exact[0]).hypot(quick[1] - exact[1]) < 2.0, "{quick:?} {exact:?}");
    }
}
