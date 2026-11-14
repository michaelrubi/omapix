//! Random stacks written as .ora and as .psd, read back, and composited
//! against the document they came from.

use std::path::{Path, PathBuf};

use omapix_engine::composite::composite;
use omapix_engine::layer::Layer;
use omapix_engine::{BlendMode, Document, ora, psd};

use super::{Kind, Spec, document, shrink::shrink, specs};

/// Where the files go: beside the build, not in `/tmp`, which is RAM.
fn folder() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fuzz-files");
    std::fs::create_dir_all(&dir).expect("making target/fuzz-files");
    dir
}

/// How many pixels' channels differ by more than `tolerance`, and by how
/// much at worst.
fn difference(a: &Document, b: &Document, tolerance: u16) -> (usize, u16) {
    let (a, b) = (composite(&a.layers, a.width, a.height), composite(&b.layers, b.width, b.height));
    let worst = |(p, q): (&[u16; 4], &[u16; 4])| (0..4).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0);
    let diffs: Vec<u16> = a.pixels().iter().zip(b.pixels()).map(worst).collect();
    (diffs.iter().filter(|&&d| d > tolerance).count(), diffs.into_iter().max().unwrap_or(0))
}

/// Where `l`'s group is in `layers`, so ids given afresh on loading
/// compare.
fn parent_at(layers: &[Layer], l: &Layer) -> Option<usize> {
    l.parent.and_then(|p| layers.iter().position(|o| o.id == p))
}

/// What the .ora round trip of `specs` gets wrong, if anything.
fn ora_fault(specs: &[Spec]) -> Option<String> {
    let doc = document(specs);
    let path = folder().join("stack.ora");
    ora::save(&doc, &path).expect("saving the .ora");
    let back = ora::load(&path).expect("loading the .ora");
    if back.layers.len() != doc.layers.len() {
        return Some(format!("{} layers came back of {}", back.layers.len(), doc.layers.len()));
    }
    for (i, (a, b)) in doc.layers.iter().zip(&back.layers).enumerate() {
        let neutral = |l: &Layer| l.blend_if.filter(|b| !b.is_neutral());
        let mask = |l: &Layer| l.mask.as_ref().map(|m| (m.pixels.to_vec(), m.enabled));
        let fields = [
            ("name", a.name == b.name),
            ("group", a.is_group == b.is_group),
            ("blend", a.blend == b.blend),
            ("opacity", a.opacity == b.opacity),
            ("visible", a.visible == b.visible),
            ("clipped", a.clipped == b.clipped),
            ("mask", mask(a) == mask(b)),
            ("blend if", neutral(a) == neutral(b)),
            ("parent", parent_at(&doc.layers, a) == parent_at(&back.layers, b)),
            ("pixels", a.pixels.to_vec() == b.pixels.to_vec()),
        ];
        if let Some((what, _)) = fields.iter().find(|(_, same)| !same) {
            return Some(format!("layer {} ({}): {what} changed", i, a.name));
        }
    }
    let (over, worst) = difference(&doc, &back, 0);
    (over > 0).then(|| format!("{over} pixels differ, by up to {worst}"))
}

/// `specs` as a .psd holds them: without the adjustment layers (they
/// layers aren't written), and opacity and Blend If in 255ths.
fn as_psd_holds(specs: &[Spec]) -> Vec<Spec> {
    let to_255 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() / 255.0;
    let kept = specs.iter().filter(|s| !s.kind.is_adjustment()).cloned();
    kept.map(|mut s| {
        s.opacity = to_255(s.opacity);
        if let Some(b) = &mut s.blend_if {
            b.this = b.this.map(to_255);
            b.underlying = b.underlying.map(to_255);
        }
        s
    })
    .collect()
}

/// What the .psd round trip of `specs` gets wrong, if anything: more than
/// 2 levels of 65,535 anywhere, since Grain Merge is written as Linear
/// Light.
fn psd_fault(specs: &[Spec]) -> Option<String> {
    let kept = as_psd_holds(specs);
    // Only Curves: a .psd with no layers opens as its flattened image.
    if kept.is_empty() {
        return None;
    }
    let doc = document(&kept);
    let path = folder().join("stack.psd");
    psd::save(&doc, &path).expect("saving the .psd");
    let back = psd::load(&path).expect("loading the .psd");
    // Grain Merge and Grain Extract are written as Linear Light with the
    // values halved about the middle. That draws the same only over an
    // opaque backdrop: over nothing, the halved colours show. So compare
    // with those layers as the file holds them.
    let mut expected = doc.clone();
    for l in &mut expected.layers {
        let halve: fn(u16) -> u16 = match l.blend {
            BlendMode::GrainMerge => |v| 16384 + v / 2,
            BlendMode::GrainExtract => |v| 49151 - v / 2,
            _ => continue,
        };
        l.pixels = l.pixels.map(|p| [halve(p[0]), halve(p[1]), halve(p[2]), p[3]]);
        l.blend = BlendMode::LinearLight;
    }
    let (over, worst) = difference(&expected, &back, 2);
    (over > 0).then(|| format!("{over} pixels differ, by up to {worst}"))
}

/// The first seed `fault` finds wrong, shrunk, as a message.
fn first_fault(format: &str, seeds: u64, fault: impl Fn(&[Spec]) -> Option<String>) -> Option<String> {
    let seed = (0..seeds).find(|&seed| fault(&specs(seed)).is_some())?;
    let least = shrink(specs(seed), |s| fault(s).is_some());
    let lines: Vec<String> = least.iter().map(|s| format!("{s:?}")).collect();
    let why = fault(&least).unwrap_or_default();
    Some(format!("{format}, seed {seed}, shrunk: {why}:\n{}", lines.join("\n")))
}

/// `OMAPIX_FUZZ_SEEDS=1000` tries more: 40 take a few seconds unoptimised.
#[test]
fn random_stacks_round_trip_as_ora_and_psd() {
    let seeds = std::env::var("OMAPIX_FUZZ_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let faults: Vec<String> = [first_fault(".ora", seeds, ora_fault), first_fault(".psd", seeds, psd_fault)]
        .into_iter()
        .flatten()
        .collect();
    assert!(faults.is_empty(), "{}", faults.join("\n\n"));
}

/// The stacks the test above found wrong, shrunk, kept with their fixes.
#[test]
fn stacks_found_by_fuzzing_round_trip() {
    // Opacity was written to 4 decimals in the .ora.
    let faded = Spec {
        opacity: 0.6511642,
        ..Spec::new(1, Kind::Pixels)
    };
    assert_eq!(ora_fault(&[faded]), None);
    // A group's clipping was lost reading a .psd.
    let clipped_group = [
        Spec {
            clipped: true,
            ..Spec::new(2, Kind::Group)
        },
        Spec {
            clipped: true,
            ..Spec::new(6, Kind::Pixels)
        },
    ];
    assert_eq!(psd_fault(&clipped_group), None);
}
