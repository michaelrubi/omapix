//! Brushes from Photoshop's brush files (`.abr`). [`crate::abr`] reads a
//! file; this makes of each brush in it what Omapix's brushes have: a
//! size, a round tip's hardness or a sampled tip, and the Brush Settings
//! in [`crate::dynamics`]. What a brush has beyond those is left out, and
//! named, so that it can be said.
//!
//! Ported from PhotoCraft's `crates/io/src/abr_map.rs`
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

use std::collections::BTreeSet;
use std::sync::Arc;

use rayon::prelude::*;

use crate::abr::{AbrFile, AbrSample, LegacyTip};
use crate::brush::BrushSettings;
use crate::descriptor::{Descriptor, Value};
use crate::dynamics::{Dynamics, MAX_COUNT};
use crate::tip::Tip;

/// The spacing of a brush that doesn't say, as in Photoshop.
const SPACING: f32 = 0.25;

/// A brush as a file has it.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    /// Diameter in image pixels.
    pub size: f32,
    /// A round tip's hardness. A sampled tip has none.
    pub hardness: Option<f32>,
    /// A pen's pressure sets the size (Size Jitter's Control).
    pub size_pressure: bool,
    pub dynamics: Dynamics,
    pub tip: Option<Arc<Tip>>,
}

impl Preset {
    /// Make `brush` this one. Its opacity and flow, and whether a pen's
    /// pressure sets them, stay as they are: they're the tool's.
    pub fn apply(&self, brush: &mut BrushSettings) {
        brush.size = self.size;
        brush.hardness = self.hardness.unwrap_or(brush.hardness);
        brush.size_pressure = self.size_pressure;
        brush.dynamics = self.dynamics;
        brush.tip = self.tip.as_ref().map(|tip| tip.id());
    }
}

fn number(d: &Descriptor, key: &str) -> Option<f32> {
    d.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as f32)
}

/// A percentage, as a fraction.
fn fraction(d: &Descriptor, key: &str) -> Option<f32> {
    number(d, key).map(|v| v / 100.0)
}

fn on(d: &Descriptor, key: &str) -> bool {
    matches!(d.get(key), Some(Value::Boolean(true)))
}

fn object<'a>(d: &'a Descriptor, key: &str) -> Option<&'a Descriptor> {
    d.get(key).and_then(Value::as_descriptor)
}

/// How much something varies at random (0–1, or to 10 for Scatter), and
/// what else sets it: 2 is a pen's pressure, 6 the stroke's direction.
fn varies(d: &Descriptor, key: &str) -> (f32, i64) {
    let Some(v) = object(d, key) else { return (0.0, 0) };
    (fraction(v, "jitter").unwrap_or(0.0).max(0.0), number(v, "bVTy").unwrap_or(0.0) as i64)
}

/// A sampled tip's pixels as a tip.
fn tip(sample: &AbrSample) -> Option<Arc<Tip>> {
    Tip::new(sample.width, sample.height, sample.to_u16()).map(Arc::new)
}

/// How big a tip paints when nothing says: its longer side.
fn longer(tip: &Tip) -> f32 {
    tip.size().0.max(tip.size().1) as f32
}

/// The brush a `brushPreset` describes, with `tips` those of the file's
/// samples. `None` if its tip isn't in the file.
fn preset(d: &Descriptor, name: String, file: &AbrFile, tips: &[Option<Arc<Tip>>], left_out: &mut BTreeSet<&'static str>) -> Option<Preset> {
    let mut out = Preset { name, size: 30.0, hardness: Some(1.0), size_pressure: false, dynamics: Dynamics { spacing: SPACING, ..Default::default() }, tip: None };
    let v = &mut out.dynamics;
    // Whether anything is set by a control Omapix doesn't have: one other
    // than a pen's pressure on size, opacity and flow, and the stroke's
    // direction on the angle.
    let mut others = false;
    let mut other = |control: i64, known: &[i64]| others |= control != 0 && !known.contains(&control);
    if let Some(t) = object(d, "Brsh") {
        if t.class_id.is("sampledBrush") || t.get("sampledData").is_some() {
            let id = t.get("sampledData").and_then(Value::as_str).unwrap_or_default();
            // A file with one tip and a brush that names another: that one.
            let at = file.samples.iter().position(|s| s.id == id).or((file.samples.len() == 1).then_some(0))?;
            out.tip = Some(tips[at].clone()?);
            out.hardness = None;
        } else {
            out.hardness = Some(fraction(t, "Hrdn").unwrap_or(1.0).clamp(0.0, 1.0));
        }
        let own = out.tip.as_deref().map(longer);
        out.size = number(t, "Dmtr").or(own).unwrap_or(out.size).clamp(1.0, 5000.0);
        v.angle = number(t, "Angl").unwrap_or(0.0);
        v.roundness = fraction(t, "Rndn").unwrap_or(1.0).clamp(0.01, 1.0);
        // With Spacing unticked (`Intr`), Photoshop spaces dabs by speed.
        let spaced = !matches!(t.get("Intr"), Some(Value::Boolean(false)));
        v.spacing = fraction(t, "Spcn").filter(|_| spaced).unwrap_or(SPACING).clamp(0.01, 10.0);
        (v.flip_x, v.flip_y) = (on(t, "flipX"), on(t, "flipY"));
    }
    if on(d, "useTipDynamics") {
        let (size, control) = varies(d, "szVr");
        (v.size_jitter, out.size_pressure) = (size.min(1.0), control == 2);
        other(control, &[2]);
        v.minimum_diameter = fraction(d, "minimumDiameter").unwrap_or(0.0).clamp(0.0, 1.0);
        let (angle, control) = varies(d, "angleDynamics");
        (v.angle_jitter, v.angle_follows) = (angle.min(1.0), control == 6);
        other(control, &[6]);
        let (roundness, control) = varies(d, "roundnessDynamics");
        v.roundness_jitter = roundness.min(1.0);
        other(control, &[]);
        v.minimum_roundness = fraction(d, "minimumRoundness").unwrap_or(v.minimum_roundness).clamp(0.0, 1.0);
        (v.flip_x_jitter, v.flip_y_jitter) = (on(d, "flipX"), on(d, "flipY"));
    }
    if on(d, "useScatter") {
        let (scatter, control) = varies(d, "scatterDynamics");
        v.scatter = scatter.min(10.0);
        other(control, &[]);
        v.both_axes = on(d, "bothAxes");
        v.count = number(d, "Cnt ").map_or(1, |n| n.clamp(1.0, MAX_COUNT as f32) as u32);
        let (count, control) = varies(d, "countDynamics");
        v.count_jitter = count.min(1.0);
        other(control, &[]);
    }
    if on(d, "usePaintDynamics") {
        // A pen's pressure on these is the options bar's buttons.
        let ((opacity, a), (flow, b)) = (varies(d, "opVr"), varies(d, "prVr"));
        (v.opacity_jitter, v.flow_jitter) = (opacity.min(1.0), flow.min(1.0));
        other(a, &[2]);
        other(b, &[2]);
    }
    (v.wet_edges, v.noise) = (on(d, "Wtdg"), on(d, "Nose"));
    let dual = object(d, "dualBrush").is_some_and(|dual| on(dual, "useDualBrush"));
    let more = [
        (on(d, "useTexture"), "Texture"),
        (dual, "Dual Brush"),
        (on(d, "useColorDynamics"), "Color Dynamics"),
        (on(d, "useBrushPose"), "Brush Pose"),
    ];
    let more = more.into_iter().chain([(others, "Fade, tilt and other controls")]);
    left_out.extend(more.filter(|(used, _)| *used).map(|(_, what)| what));
    Some(out)
}

/// The brushes in `file`, and what some of them have that Omapix doesn't.
pub fn presets(file: &AbrFile) -> (Vec<Preset>, Vec<&'static str>) {
    let tips: Vec<Option<Arc<Tip>>> = file.samples.par_iter().map(tip).collect();
    let plain = |name: String, size: f32, spacing: f32| Preset {
        name,
        size: size.clamp(1.0, 5000.0),
        hardness: None,
        size_pressure: false,
        dynamics: Dynamics { spacing, ..Default::default() },
        tip: None,
    };
    let mut left_out = BTreeSet::new();
    let mut out = Vec::new();
    // The old layout: a list of round and sampled brushes.
    for (i, brush) in file.legacy.iter().enumerate() {
        let spacing = if brush.spacing == 0 { SPACING } else { f32::from(brush.spacing) / 100.0 };
        let name = brush.name.trim();
        match &brush.tip {
            LegacyTip::Computed { diameter, hardness, angle, roundness } => {
                let name = if name.is_empty() { format!("{diameter} px Round") } else { name.into() };
                let mut round = plain(name, f32::from(*diameter), spacing);
                round.hardness = Some((f32::from(*hardness) / 100.0).clamp(0.0, 1.0));
                (round.dynamics.angle, round.dynamics.roundness) = (f32::from(*angle), (f32::from(*roundness) / 100.0).clamp(0.01, 1.0));
                out.push(round);
            }
            LegacyTip::Sampled(sample) => {
                let Some(tip) = tip(sample) else { continue };
                let name = if name.is_empty() { format!("Sampled Brush {}", i + 1) } else { name.into() };
                out.push(Preset { tip: Some(tip.clone()), ..plain(name, longer(&tip), spacing) });
            }
        }
    }
    // The current one: tips, and the brushes that use them.
    for (i, d) in file.presets.iter().enumerate() {
        let name = d.name().map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        out.extend(preset(d, name.unwrap_or_else(|| format!("Brush {}", i + 1)), file, &tips, &mut left_out));
    }
    // Tips alone, as early files of the current layout have: a brush each.
    if file.legacy.is_empty() && file.presets.is_empty() {
        for (i, tip) in tips.iter().flatten().enumerate() {
            out.push(Preset { tip: Some(tip.clone()), ..plain(format!("Sampled Brush {}", i + 1), longer(tip), SPACING) });
        }
    }
    (out, left_out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abr::{LegacyBrush, parse, write_v12, write_v6};
    use crate::descriptor::UnicodeString;

    fn sample(id: &str, width: u32, height: u32) -> AbrSample {
        let data = (0..width * height).map(|i| (i % 251) as u8).collect();
        AbrSample { id: id.into(), width, height, depth: 8, data }
    }

    fn percent(v: f64) -> Value {
        Value::UnitFloat { unit: *b"#Prc", value: v }
    }

    /// How something varies: its jitter, and its control.
    fn vary(jitter: f64, control: i32) -> Value {
        Value::Descriptor(Descriptor::new("brVr").with("bVTy", Value::Integer(control)).with("jitter", percent(jitter)))
    }

    fn named(name: &str) -> Descriptor {
        Descriptor::new("brushPreset").with("Nm  ", Value::Text(UnicodeString::new_nul(name)))
    }

    #[test]
    fn a_brush_with_a_sampled_tip_and_every_setting_omapix_has() {
        let tip = Descriptor::new("sampledBrush")
            .with("Dmtr", Value::UnitFloat { unit: *b"#Pxl", value: 80.0 })
            .with("Angl", Value::UnitFloat { unit: *b"#Ang", value: -30.0 })
            .with("Rndn", percent(60.0))
            .with("Spcn", percent(45.0))
            .with("Intr", Value::Boolean(true))
            .with("flipY", Value::Boolean(true))
            .with("sampledData", Value::Text(UnicodeString::new_nul("pores")));
        let brush = named("Skin Texture")
            .with("Brsh", Value::Descriptor(tip))
            .with("useTipDynamics", Value::Boolean(true))
            .with("szVr", vary(70.0, 2))
            .with("minimumDiameter", percent(20.0))
            .with("angleDynamics", vary(100.0, 6))
            .with("roundnessDynamics", vary(30.0, 0))
            .with("minimumRoundness", percent(40.0))
            .with("flipX", Value::Boolean(true))
            .with("Nose", Value::Boolean(true))
            .with("useScatter", Value::Boolean(true))
            .with("scatterDynamics", vary(350.0, 0))
            .with("bothAxes", Value::Boolean(true))
            .with("Cnt ", Value::Double(3.0))
            .with("countDynamics", vary(50.0, 0))
            .with("usePaintDynamics", Value::Boolean(true))
            .with("opVr", vary(25.0, 2))
            .with("prVr", vary(10.0, 0));
        // Through the file's own layout and back, as one from disk comes.
        let bytes = write_v6(2, &[sample("other", 3, 3), sample("pores", 40, 20)], &[], &[brush], true).unwrap();
        let (brushes, left_out) = presets(&parse(&bytes).unwrap());
        assert!(left_out.is_empty(), "{left_out:?}");
        let [brush] = &brushes[..] else { panic!("{}", brushes.len()) };
        assert_eq!((brush.name.as_str(), brush.size, brush.hardness, brush.size_pressure), ("Skin Texture", 80.0, None, true));
        assert_eq!(brush.tip.as_ref().unwrap().size(), (40, 20));
        let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
        let d = brush.dynamics;
        assert!(close(d.spacing, 0.45) && close(d.angle, -30.0) && close(d.roundness, 0.6));
        assert!(close(d.size_jitter, 0.7) && close(d.minimum_diameter, 0.2) && close(d.angle_jitter, 1.0) && d.angle_follows);
        assert!(close(d.roundness_jitter, 0.3) && close(d.minimum_roundness, 0.4));
        assert!(close(d.scatter, 3.5) && d.both_axes && d.count == 3 && close(d.count_jitter, 0.5));
        assert!(close(d.opacity_jitter, 0.25) && close(d.flow_jitter, 0.1));
        assert_eq!((d.flip_x, d.flip_y, d.flip_x_jitter, d.flip_y_jitter, d.noise, d.wet_edges), (false, true, true, false, true, false));

        // Chosen, it changes the tool's brush but not its opacity or flow.
        let mut settings = BrushSettings { opacity: 0.4, flow: 0.3, hardness: 0.2, opacity_pressure: false, ..Default::default() };
        brush.apply(&mut settings);
        assert_eq!((settings.size, settings.tip, settings.dynamics), (80.0, Some(brush.tip.as_ref().unwrap().id()), d));
        assert_eq!((settings.opacity, settings.flow, settings.hardness, settings.opacity_pressure), (0.4, 0.3, 0.2, false));
    }

    #[test]
    fn what_omapix_does_not_have_is_named_and_a_missing_tip_skips_the_brush() {
        let round = Descriptor::new("computedBrush").with("Dmtr", Value::Double(12.0)).with("Hrdn", percent(35.0)).with("flipX", Value::Boolean(true));
        let textured = named("Chalk")
            .with("Brsh", Value::Descriptor(round))
            .with("useTexture", Value::Boolean(true))
            .with("dualBrush", Value::Descriptor(Descriptor::new("dualBrush").with("useDualBrush", Value::Boolean(true))))
            .with("useColorDynamics", Value::Boolean(false))
            .with("Wtdg", Value::Boolean(true))
            .with("useTipDynamics", Value::Boolean(true))
            .with("szVr", vary(0.0, 1))
            // Unticked: its settings aren't used.
            .with("useScatter", Value::Boolean(false))
            .with("scatterDynamics", vary(500.0, 0));
        let lost = named("Lost").with("Brsh", Value::Descriptor(Descriptor::new("sampledBrush").with("sampledData", Value::Text(UnicodeString::new("gone")))));
        let file = AbrFile { version: 6, subversion: 2, samples: vec![sample("a", 4, 4), sample("b", 4, 4)], presets: vec![textured, lost], ..Default::default() };
        let (brushes, left_out) = presets(&file);
        assert_eq!(left_out, ["Dual Brush", "Fade, tilt and other controls", "Texture"]);
        let [chalk] = &brushes[..] else { panic!("{}", brushes.len()) };
        assert_eq!((chalk.name.as_str(), chalk.size, chalk.hardness, chalk.size_pressure), ("Chalk", 12.0, Some(0.35), false));
        assert!(chalk.tip.is_none());
        assert_eq!(chalk.dynamics, Dynamics { spacing: SPACING, flip_x: true, wet_edges: true, ..Default::default() });
    }

    #[test]
    fn old_brush_files_and_files_of_tips_alone_give_a_brush_each() {
        let round = LegacyBrush { name: String::new(), spacing: 0, anti_alias: true, tip: LegacyTip::Computed { diameter: 19, hardness: 50, angle: 45, roundness: 30 } };
        let leaf = LegacyBrush { name: "Leaf".into(), spacing: 120, anti_alias: true, tip: LegacyTip::Sampled(sample("", 30, 50)) };
        let old = parse(&write_v12(2, &[round, leaf], true).unwrap()).unwrap();
        let (brushes, left_out) = presets(&old);
        assert!(left_out.is_empty());
        let [round, leaf] = &brushes[..] else { panic!("{}", brushes.len()) };
        assert_eq!((round.name.as_str(), round.size, round.hardness), ("19 px Round", 19.0, Some(0.5)));
        assert_eq!((round.dynamics.spacing, round.dynamics.angle, round.dynamics.roundness), (0.25, 45.0, 0.3));
        assert_eq!((leaf.name.as_str(), leaf.size, leaf.hardness, leaf.dynamics.spacing), ("Leaf", 50.0, None, 1.2));
        assert_eq!(leaf.tip.as_ref().unwrap().size(), (30, 50));

        let tips = AbrFile { version: 6, subversion: 1, samples: vec![sample("a", 8, 4), sample("b", 2, 6)], ..Default::default() };
        let names: Vec<_> = presets(&tips).0.iter().map(|b| (b.name.clone(), b.size)).collect();
        assert_eq!(names, [("Sampled Brush 1".to_owned(), 8.0), ("Sampled Brush 2".to_owned(), 6.0)]);
    }
}
