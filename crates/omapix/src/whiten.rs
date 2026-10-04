//! Retouch › Reduce Shine, Lighten Under Eyes, Whiten Teeth and Whiten Eyes
//! (docs/AI.md, milestone 13): the shine on the skin, the shadows under the
//! eyes, the teeth or the whites of the eyes, of the faces the face models
//! find in what the active layer and those below it show, found on a thread
//! of their own, and a masked layer for them above (`omapix_engine::shine`,
//! `under_eyes` and `whiten`). There's no dialog: the layer's opacity is
//! the amount, and its mask can be painted on.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use omapix_engine::shine::{self, Matte};
use omapix_engine::whiten::{self, Whiten};
use omapix_engine::{ColorProfile, Raster, Selection, under_eyes};

use crate::editor::{Editor, Target};
use crate::face_selection::{find_skin, find_under_eyes, find_whites};

/// The layer's opacity to start with, and Auto Retouch's Standard.
pub const AMOUNT: f32 = 50.0;

/// What's found and lightened, or toned down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Whites(Whiten),
    UnderEyes,
    Shine,
}

impl Part {
    /// Its command's name, and its undo step's.
    fn label(self) -> &'static str {
        match self {
            Part::Whites(what) => what.name(),
            Part::UnderEyes => "Lighten Under Eyes",
            Part::Shine => shine::NAME,
        }
    }

    /// What the status bar says while it's found.
    pub fn finding(self) -> &'static str {
        match self {
            Part::Whites(Whiten::Teeth) => "Finding teeth…",
            Part::Whites(Whiten::Eyes) => "Finding eyes…",
            Part::UnderEyes => "Finding shadows under the eyes…",
            Part::Shine => "Finding shine…",
        }
    }
}

/// What a part's layer is made from: its mask, or for shine its pixels
/// too.
enum Made {
    Mask(Selection),
    Matte(Matte),
}

/// Where it arrives once it's found.
type Found = Receiver<Result<Made, String>>;

#[derive(Default)]
pub struct Whitening {
    /// What's being found, and the layer it goes above.
    running: Option<(Part, u64, Found)>,
}

impl Whitening {
    /// Find `part` in what the active layer and those below it show.
    pub fn start(&mut self, ctx: &egui::Context, editor: &Editor, part: Part) -> Result<(), String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let image = doc.composite_current_and_below(index);
            let _ = tx.send(match part {
                Part::Whites(what) => find_whites(&image, &doc.profile, what).map(Made::Mask),
                Part::UnderEyes => find_under_eyes(&image, &doc.profile).map(Made::Mask),
                Part::Shine => find_shine(image, &doc.profile).map(Made::Matte),
            });
            ctx.request_repaint();
        });
        self.running = Some((part, editor.active, rx));
        Ok(())
    }

    /// Add the layer once it's found, above the layer it was started on
    /// (or the active one, if that's gone), as one undo step, and select it.
    /// Returns what went wrong, if anything.
    pub fn poll(&mut self, editor: &mut Editor) -> Option<String> {
        let (part, layer, rx) = self.running.as_ref()?;
        let (part, layer) = (*part, *layer);
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(format!("{} stopped unexpectedly", part.label())),
        };
        self.running = None;
        let made = match result {
            Ok(made) => made,
            Err(e) => return Some(e),
        };
        let index = editor.doc.index_of(layer).or(editor.active_index())?;
        let added = editor.edit(part.label(), |doc, active| {
            let id = match (part, &made) {
                (Part::Whites(what), Made::Mask(mask)) => whiten::add_layer(doc, index, what, mask, AMOUNT),
                (_, Made::Mask(mask)) => under_eyes::add_layer(doc, index, mask, AMOUNT),
                (_, Made::Matte(matte)) => shine::add_layer(doc, index, matte, AMOUNT),
            };
            if let Some(id) = id {
                *active = id;
            }
        });
        if added {
            // Painting on the layer paints its mask.
            editor.target = Target::Mask;
        }
        None
    }

    /// What's being found, if anything.
    pub fn busy(&self) -> Option<Part> {
        self.running.as_ref().map(|(part, ..)| *part)
    }
}

/// The shine on the skin in `image`, its faces and body, measured by its
/// largest face.
fn find_shine(image: Raster, profile: &ColorProfile) -> Result<Matte, String> {
    let found = find_skin(image, profile)?;
    let matte = Matte::new(&found.image, &found.skin, found.iod).ok_or("Found no skin")?;
    if matte.is_empty() { Err("Found no shine".into()) } else { Ok(matte) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::Document;

    #[test]
    fn teeth_found_become_a_masked_layer_above_in_one_undo_step() {
        // A tooth-coloured image, with the teeth found in the middle of it.
        let image = Raster::new(600, 400, vec![[55000, 50000, 41000, 65535]; 600 * 400]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let before = editor.doc.composite();

        let (tx, rx) = channel();
        let mut whitening = Whitening {
            running: Some((Part::Whites(Whiten::Teeth), editor.active, rx)),
        };
        assert_eq!(whitening.poll(&mut editor), None);
        assert_eq!(whitening.busy(), Some(Part::Whites(Whiten::Teeth)));
        tx.send(Ok(Made::Mask(Selection::rectangle(600, 400, (300.0, 200.0), (400.0, 240.0))))).unwrap();
        assert_eq!(whitening.poll(&mut editor), None);
        assert_eq!(whitening.busy(), None);

        // The layer's selected, its opacity the amount, for painting on its
        // mask.
        let layer = editor.doc.layer(editor.active).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Whiten Teeth", AMOUNT / 100.0));
        assert_eq!(editor.doc.layers.len(), 2);
        assert!(editor.target == Target::Mask);
        let after = editor.doc.composite();
        assert!(after.get(350, 220)[2] > before.get(350, 220)[2] + 2000);
        assert_eq!(after.get(100, 100), before.get(100, 100));
        assert_eq!(editor.undo_label(), Some("Whiten Teeth"));
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);

        // Failures come back as messages, and nothing's added.
        let (tx, rx) = channel();
        whitening.running = Some((Part::Whites(Whiten::Eyes), editor.active, rx));
        tx.send(Err("Found no eyes".into())).unwrap();
        assert_eq!(whitening.poll(&mut editor), Some("Found no eyes".into()));
        assert_eq!(editor.doc.layers.len(), 1);

        // Shadows under the eyes become a brightening Curves layer.
        let (tx, rx) = channel();
        whitening.running = Some((Part::UnderEyes, editor.active, rx));
        tx.send(Ok(Made::Mask(Selection::rectangle(600, 400, (300.0, 200.0), (400.0, 240.0))))).unwrap();
        assert_eq!(whitening.poll(&mut editor), None);
        let layer = editor.doc.layer(editor.active).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Under Eyes", AMOUNT / 100.0));
        assert!(matches!(layer.adjustment, Some(omapix_engine::adjust::Adjustment::Curves(_))));
        assert!(editor.doc.composite().get(350, 220)[0] > before.get(350, 220)[0] + 1000);
        assert_eq!(editor.undo_label(), Some("Lighten Under Eyes"));
    }

    #[test]
    fn shine_found_becomes_a_masked_pixel_layer_above_in_one_undo_step() {
        // Skin-coloured, with a paler hot spot round (300, 200).
        let pixels = (0..600 * 400)
            .map(|i| {
                let hot = 13000.0 * (-(((i % 600) as f32 - 300.0).hypot((i / 600) as f32 - 200.0) / 12.0).powi(2)).exp();
                [42000 + hot as u16, 33000 + (hot * 1.3) as u16, 28000 + (hot * 1.5) as u16, 65535]
            })
            .collect();
        let image = Raster::new(600, 400, pixels);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let matte = Matte::new(&image, &Selection::all(600, 400), 100.0).unwrap();

        let (tx, rx) = channel();
        let mut whitening = Whitening {
            running: Some((Part::Shine, editor.active, rx)),
        };
        assert_eq!(whitening.busy(), Some(Part::Shine));
        tx.send(Ok(Made::Matte(matte))).unwrap();
        assert_eq!(whitening.poll(&mut editor), None);
        let layer = editor.doc.layer(editor.active).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), ("Reduce Shine", AMOUNT / 100.0));
        assert!(layer.adjustment.is_none() && layer.mask.is_some());
        assert_eq!(editor.target, Target::Mask);
        let after = editor.doc.composite();
        assert!(after.get(300, 200)[0] < image.get(300, 200)[0] - 3000);
        assert_eq!(after.get(100, 100), image.get(100, 100));
        assert_eq!(editor.undo_label(), Some("Reduce Shine"));
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a folder
    /// of them), the largest face before, with its shine reduced and as the
    /// mask, side by side as `<photo>-shine.png` in `OMAPIX_FACE_OUT`.
    /// `AMOUNT` sets the layer's opacity. Needs the models
    /// (scripts/fetch-models.sh) and ImageMagick.
    #[test]
    #[ignore]
    fn reduce_shine_in_photos() {
        use omapix_engine::shine::{self, Matte};
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        let amount = std::env::var("AMOUNT").map_or(AMOUNT, |v| v.parse().unwrap());
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(mut doc) = omapix_engine::io::load(&path) else { continue };
            let found = match crate::face_selection::find_skin(doc.composite(), &doc.profile) {
                Ok(found) => found,
                Err(e) => {
                    eprintln!("{name}: {e}");
                    continue;
                }
            };
            let t = std::time::Instant::now();
            let Some(matte) = Matte::new(&found.image, &found.skin, found.iod) else { continue };
            eprintln!("{name}: iod {:.0}, skin {:?}, shine found in {:?}", found.iod, found.skin.bounds(), t.elapsed());
            let Some(id) = shine::add_layer(&mut doc, 0, &matte, amount) else {
                eprintln!("{name}: no shine");
                continue;
            };
            let after = doc.composite();
            let mask = &doc.layer(id).unwrap().mask.as_ref().unwrap().pixels;
            let before = &found.image;
            let (iw, ih) = (before.width() as f32, before.height() as f32);
            let r = found.iod * 1.9;
            let (cx, cy) = (found.nose.x, found.nose.y);
            let [x0, y0, x1, y1] = [(cx - r).max(0.0), (cy - r).max(0.0), (cx + r).min(iw), (cy + r).min(ih)];
            let step = ((x1 - x0) / 620.0).max(1.0);
            let (ow, oh) = (((x1 - x0) / step) as u32, ((y1 - y0) / step) as u32);
            let mut ppm = format!("P6 {} {} 255\n", ow * 3, oh).into_bytes();
            for y in 0..oh {
                let at = |x: u32| ((x0 + x as f32 * step) as u32, (y0 + y as f32 * step) as u32);
                for half in [before, &after] {
                    for x in 0..ow {
                        let (px, py) = at(x);
                        let p = half.get(px, py);
                        ppm.extend([0, 1, 2].map(|c| (p[c] >> 8) as u8));
                    }
                }
                for x in 0..ow {
                    let (px, py) = at(x);
                    ppm.extend([(mask.get(px, py) >> 8) as u8; 3]);
                }
            }
            let ppm_path = format!("{out}/{name}-shine.ppm");
            std::fs::write(&ppm_path, ppm).unwrap();
            std::process::Command::new("magick").args([&ppm_path, &format!("{out}/{name}-shine.png")]).status().unwrap();
            std::fs::remove_file(ppm_path).unwrap();
        }
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a folder
    /// of them), the teeth, the eyes and under the eyes of its faces before,
    /// after and as the mask, side by side as `<photo>-teeth.png`,
    /// `<photo>-eyes.png` and `<photo>-under-eyes.png` in `OMAPIX_FACE_OUT`.
    /// `AMOUNT` sets the layer's opacity. Needs the models
    /// (scripts/fetch-models.sh) and ImageMagick.
    #[test]
    #[ignore]
    fn whiten_in_photos() {
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        let amount = std::env::var("AMOUNT").map_or(AMOUNT, |v| v.parse().unwrap());
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(doc) = omapix_engine::io::load(&path) else { continue };
            let before = doc.composite();
            for (what, part) in [(Some(Whiten::Teeth), "teeth"), (Some(Whiten::Eyes), "eyes"), (None, "under-eyes")] {
                let t = std::time::Instant::now();
                let found = match what {
                    Some(what) => find_whites(&before, &doc.profile, what),
                    None => find_under_eyes(&before, &doc.profile),
                };
                let whites = match found {
                    Ok(whites) => whites,
                    Err(e) => {
                        eprintln!("{name}: {e}");
                        continue;
                    }
                };
                let [bx, by, bw, bh] = whites.bounds().unwrap();
                eprintln!("{name}: {part} in {:?}, {bw} × {bh} at ({bx}, {by})", t.elapsed());
                let mut doc = doc.clone();
                match what {
                    Some(what) => whiten::add_layer(&mut doc, 0, what, &whites, amount),
                    None => under_eyes::add_layer(&mut doc, 0, &whites, amount),
                };
                let after = doc.composite();
                let pad = bh.max(bw / 4);
                let (x0, y0) = (bx.saturating_sub(pad), by.saturating_sub(pad));
                let (x1, y1) = ((bx + bw + pad).min(doc.width), (by + bh + pad).min(doc.height));
                let step = (x1 - x0).div_ceil(700);
                let (ow, oh) = ((x1 - x0) / step, (y1 - y0) / step);
                let mut ppm = format!("P6 {} {} 255\n", ow * 3, oh).into_bytes();
                for y in 0..oh {
                    for half in [&before, &after] {
                        for x in 0..ow {
                            let p = half.get(x0 + x * step, y0 + y * step);
                            ppm.extend([0, 1, 2].map(|c| (p[c] >> 8) as u8));
                        }
                    }
                    for x in 0..ow {
                        ppm.extend([(whites.at(x0 + x * step, y0 + y * step) * 255.0) as u8; 3]);
                    }
                }
                let ppm_path = format!("{out}/{name}-{part}.ppm");
                std::fs::write(&ppm_path, ppm).unwrap();
                let png = format!("{out}/{name}-{part}.png");
                std::process::Command::new("magick").args([&ppm_path, "-filter", "point", "-resize", "1800x>", &png]).status().unwrap();
                std::fs::remove_file(ppm_path).unwrap();
            }
        }
    }
}
