//! Free Transform's box (Ctrl+T): which handle the pointer is on, and what
//! dragging it does, as in Photoshop. Corner handles scale in proportion
//! (Shift for free), side handles stretch one way, Alt scales about the
//! centre, dragging inside moves, and dragging outside rotates (Shift in
//! steps of 15°).

use egui::{CursorIcon, Pos2, pos2};
use omapix_engine::transform::Affine;

/// What a drag on the box does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    Move,
    Rotate,
    /// A side (one of `x`, `y` is 0) or corner handle: -1 is the left or
    /// top edge, 1 the right or bottom.
    Scale { x: i8, y: i8 },
}

/// A drag in progress: the handle, where it started (image pixels) and the
/// transform then.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    handle: Handle,
    from: Pos2,
    start: Affine,
}

/// A point of the untransformed box, `(u, v)` from -1 (left, top) to 1.
fn local(bounds: [f64; 4], u: f64, v: f64) -> (f64, f64) {
    let [x0, y0, x1, y1] = bounds;
    (x0 + (u + 1.0) / 2.0 * (x1 - x0), y0 + (v + 1.0) / 2.0 * (y1 - y0))
}

fn to_pos((x, y): (f64, f64)) -> Pos2 {
    pos2(x as f32, y as f32)
}

/// The box's corners on the image, clockwise from the top left.
pub fn corners(bounds: [f64; 4], t: &Affine) -> [Pos2; 4] {
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(u, v)| to_pos(t.apply(local(bounds, u, v))))
}

/// The eight handles, and where they are on the image.
fn handles(bounds: [f64; 4], t: &Affine) -> impl Iterator<Item = (Handle, Pos2)> + '_ {
    let steps = [-1i8, 0, 1];
    steps.into_iter().flat_map(move |y| {
        steps.into_iter().filter(move |&x| (x, y) != (0, 0)).map(move |x| {
            let p = t.apply(local(bounds, f64::from(x), f64::from(y)));
            (Handle::Scale { x, y }, to_pos(p))
        })
    })
}

/// The handle at `p`, within `reach` image pixels of it, or else moving
/// (inside the box) or rotating (outside).
pub fn hit(bounds: [f64; 4], t: &Affine, p: Pos2, reach: f32) -> Handle {
    if let Some((handle, _)) = handles(bounds, t).find(|(_, at)| at.distance(p) <= reach) {
        return handle;
    }
    let c = corners(bounds, t);
    // Inside if on the same side of every edge.
    let sides = (0..4).map(|i| {
        let (e, q) = (c[(i + 1) % 4] - c[i], p - c[i]);
        e.x * q.y - e.y * q.x
    });
    let signs: Vec<f32> = sides.map(f32::signum).collect();
    if signs.iter().all(|&s| s >= 0.0) || signs.iter().all(|&s| s <= 0.0) {
        Handle::Move
    } else {
        Handle::Rotate
    }
}

/// The cursor for a handle, turned with the box.
pub fn cursor(bounds: [f64; 4], t: &Affine, handle: Handle) -> CursorIcon {
    let Handle::Scale { x, y } = handle else {
        return match handle {
            Handle::Move => CursorIcon::Move,
            _ => CursorIcon::Crosshair,
        };
    };
    let centre = t.apply(local(bounds, 0.0, 0.0));
    let at = t.apply(local(bounds, f64::from(x), f64::from(y)));
    let angle = (at.1 - centre.1).atan2(at.0 - centre.0).to_degrees().rem_euclid(180.0);
    // The nearest of the four directions a resize cursor points.
    match ((angle + 22.5) / 45.0) as u32 % 4 {
        0 => CursorIcon::ResizeHorizontal,
        1 => CursorIcon::ResizeNwSe,
        2 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeNeSw,
    }
}

impl Drag {
    pub fn new(handle: Handle, from: Pos2, start: Affine) -> Self {
        Self { handle, from, start }
    }

    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// The transform with the pointer dragged to `p`.
    pub fn to(&self, bounds: [f64; 4], p: Pos2, shift: bool, alt: bool) -> Affine {
        let start = &self.start;
        match self.handle {
            Handle::Move => {
                let d = p - self.from;
                let d = if shift { crate::app::constrain_45(d) } else { d };
                start.then(&Affine::translate(d.x.into(), d.y.into()))
            }
            Handle::Rotate => {
                let c = start.apply(local(bounds, 0.0, 0.0));
                let angle = |p: Pos2| (f64::from(p.y) - c.1).atan2(f64::from(p.x) - c.0);
                let mut turn = angle(p) - angle(self.from);
                if shift {
                    let step = 15f64.to_radians();
                    let (_, _, now) = start.decompose();
                    turn = ((now + turn) / step).round() * step - now;
                }
                start.then(&Affine::rotate_about(turn, c))
            }
            Handle::Scale { x, y } => {
                let Some(inverse) = start.inverse() else {
                    return *start;
                };
                // In the box's own frame: the pointer, the handle, and what
                // stays put (the opposite handle, or the centre with Alt).
                let (u, v) = (f64::from(x), f64::from(y));
                let q = inverse.apply((f64::from(p.x), f64::from(p.y)));
                let h = local(bounds, u, v);
                let a = if alt { local(bounds, 0.0, 0.0) } else { local(bounds, -u, -v) };
                let ratio = |q: f64, h: f64, a: f64| if h == a { 1.0 } else { (q - a) / (h - a) };
                let (mut sx, mut sy) = (ratio(q.0, h.0, a.0), ratio(q.1, h.1, a.1));
                if x == 0 {
                    sx = 1.0;
                } else if y == 0 {
                    sy = 1.0;
                } else if !shift {
                    // Corners keep the proportions: the pointer's distance
                    // along the diagonal.
                    let (dx, dy) = (h.0 - a.0, h.1 - a.1);
                    let s = ((q.0 - a.0) * dx + (q.1 - a.1) * dy) / (dx * dx + dy * dy);
                    (sx, sy) = (s, s);
                }
                Affine::scale_about(sx, sy, a).then(start)
            }
        }
    }
}

/// The status bar's readout: width and height in percent, and the angle.
pub fn readout(t: &Affine) -> String {
    let (sx, sy, angle) = t.decompose();
    format!(
        "Free Transform: W {:.1}%  H {:.1}%  {:.1}° — Enter to apply, Esc to cancel",
        sx * 100.0,
        sy * 100.0,
        angle.to_degrees()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Vec2;

    fn close(a: Pos2, b: Pos2) -> bool {
        (a - b).length() < 1e-3
    }

    const BOX: [f64; 4] = [100.0, 100.0, 300.0, 200.0];

    #[test]
    fn handles_moving_and_rotating_are_found_where_they_are() {
        let t = Affine::IDENTITY;
        assert_eq!(hit(BOX, &t, pos2(302.0, 199.0), 5.0), Handle::Scale { x: 1, y: 1 });
        assert_eq!(hit(BOX, &t, pos2(200.0, 97.0), 5.0), Handle::Scale { x: 0, y: -1 });
        assert_eq!(hit(BOX, &t, pos2(150.0, 150.0), 5.0), Handle::Move);
        assert_eq!(hit(BOX, &t, pos2(50.0, 150.0), 5.0), Handle::Rotate);
        // Turned a quarter, the top right corner is at the bottom right.
        let turned = Affine::rotate_about(std::f64::consts::FRAC_PI_2, (200.0, 150.0));
        let [_, top_right, ..] = corners(BOX, &turned);
        assert!(close(top_right, pos2(250.0, 250.0)));
        assert_eq!(hit(BOX, &turned, pos2(250.0, 249.0), 5.0), Handle::Scale { x: 1, y: -1 });
        assert_eq!(cursor(BOX, &turned, Handle::Scale { x: 1, y: 0 }), CursorIcon::ResizeVertical);
    }

    #[test]
    fn corners_scale_in_proportion_unless_shift() {
        let drag = Drag::new(Handle::Scale { x: 1, y: 1 }, pos2(300.0, 200.0), Affine::IDENTITY);
        // Dragged right only: both sides grow, from the top left.
        let t = drag.to(BOX, pos2(500.0, 200.0), false, false);
        let c = corners(BOX, &t);
        assert!(close(c[0], pos2(100.0, 100.0)));
        let (sx, sy, _) = t.decompose();
        assert!((sx - sy).abs() < 1e-9 && sx > 1.5, "{sx} {sy}");
        // With Shift, just the width.
        let t = drag.to(BOX, pos2(500.0, 200.0), true, false);
        assert!(close(corners(BOX, &t)[2], pos2(500.0, 200.0)));
        // With Alt, about the centre.
        let t = drag.to(BOX, pos2(400.0, 250.0), true, true);
        assert!(close(corners(BOX, &t)[0], pos2(0.0, 50.0)));
    }

    #[test]
    fn sides_stretch_one_way_and_follow_the_turned_box() {
        let turned = Affine::rotate_about(std::f64::consts::FRAC_PI_2, (200.0, 150.0));
        // The right side's handle is now at the bottom, (200, 250).
        let drag = Drag::new(Handle::Scale { x: 1, y: 0 }, pos2(200.0, 250.0), turned);
        let t = drag.to(BOX, pos2(200.0, 350.0), false, false);
        let (sx, sy, _) = t.decompose();
        assert!((sx - 1.5).abs() < 1e-6 && (sy - 1.0).abs() < 1e-6, "{sx} {sy}");
    }

    #[test]
    fn moving_and_rotating_with_shift_snap() {
        let drag = Drag::new(Handle::Move, pos2(150.0, 150.0), Affine::IDENTITY);
        let t = drag.to(BOX, pos2(190.0, 153.0), true, false);
        assert!(close(corners(BOX, &t)[0], pos2(140.0, 100.0)));

        let drag = Drag::new(Handle::Rotate, pos2(400.0, 150.0), Affine::IDENTITY);
        // About 40° clockwise, snapped to 45°.
        let p = pos2(200.0, 150.0) + Vec2::angled(40f32.to_radians()) * 200.0;
        let (_, _, angle) = drag.to(BOX, p, true, false).decompose();
        assert!((angle.to_degrees() - 45.0).abs() < 1e-6, "{angle}");
        assert!(readout(&drag.to(BOX, p, true, false)).contains("45.0°"));
    }
}
