//! Retouch › Whiten Teeth and Whiten Eyes (docs/AI.md, milestone 13): the
//! teeth, or the whites of the eyes, of the faces the face models find in
//! what the active layer and those below it show, found on a thread of
//! their own, and a Hue/Saturation layer masked to them above
//! (`omapix_engine::whiten`). There's no dialog: the layer's opacity is the
//! amount, and its mask can be painted on.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use omapix_engine::Selection;
use omapix_engine::whiten::{self, Whiten};

use crate::editor::{Editor, Target};
use crate::face_selection::find_whites;

/// The layer's opacity to start with, and Auto Retouch's Standard.
pub const AMOUNT: f32 = 50.0;

/// Where the teeth or whites arrive once they're found.
type Found = Receiver<Result<Selection, String>>;

#[derive(Default)]
pub struct Whitening {
    /// What's being found, and the layer it goes above.
    running: Option<(Whiten, u64, Found)>,
}

impl Whitening {
    /// Find `what` in what the active layer and those below it show.
    pub fn start(&mut self, ctx: &egui::Context, editor: &Editor, what: Whiten) -> Result<(), String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(find_whites(&doc.composite_current_and_below(index), &doc.profile, what));
            ctx.request_repaint();
        });
        self.running = Some((what, editor.active, rx));
        Ok(())
    }

    /// Add the layer once they're found, above the layer it was started on
    /// (or the active one, if that's gone), as one undo step, and select it.
    /// Returns what went wrong, if anything.
    pub fn poll(&mut self, editor: &mut Editor) -> Option<String> {
        let (what, layer, rx) = self.running.as_ref()?;
        let (what, layer) = (*what, *layer);
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(format!("{} stopped unexpectedly", what.name())),
        };
        self.running = None;
        let whites = match result {
            Ok(whites) => whites,
            Err(e) => return Some(e),
        };
        let index = editor.doc.index_of(layer).or(editor.active_index())?;
        let added = editor.edit(what.name(), |doc, active| {
            if let Some(id) = whiten::add_layer(doc, index, what, &whites, AMOUNT) {
                *active = id;
            }
        });
        if added {
            // Painting on an adjustment layer paints its mask.
            editor.target = Target::Mask;
        }
        None
    }

    /// What's being found, if anything.
    pub fn busy(&self) -> Option<Whiten> {
        self.running.as_ref().map(|(what, ..)| *what)
    }
}

/// What the status bar says while `what` is found.
pub fn finding(what: Whiten) -> &'static str {
    match what {
        Whiten::Teeth => "Finding teeth…",
        Whiten::Eyes => "Finding eyes…",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::{ColorProfile, Document, Raster};

    #[test]
    fn teeth_found_become_a_masked_layer_above_in_one_undo_step() {
        // A tooth-coloured image, with the teeth found in the middle of it.
        let image = Raster::new(600, 400, vec![[55000, 50000, 41000, 65535]; 600 * 400]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let before = editor.doc.composite();

        let (tx, rx) = channel();
        let mut whitening = Whitening {
            running: Some((Whiten::Teeth, editor.active, rx)),
        };
        assert_eq!(whitening.poll(&mut editor), None);
        assert_eq!(whitening.busy(), Some(Whiten::Teeth));
        tx.send(Ok(Selection::rectangle(600, 400, (300.0, 200.0), (400.0, 240.0)))).unwrap();
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
        whitening.running = Some((Whiten::Eyes, editor.active, rx));
        tx.send(Err("Found no eyes".into())).unwrap();
        assert_eq!(whitening.poll(&mut editor), Some("Found no eyes".into()));
        assert_eq!(editor.doc.layers.len(), 1);
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a folder
    /// of them), the teeth and the eyes of its faces before, after and as
    /// the mask, side by side as `<photo>-teeth.png` and `<photo>-eyes.png`
    /// in `OMAPIX_FACE_OUT`. `AMOUNT` sets the layer's opacity. Needs the
    /// models (scripts/fetch-models.sh) and ImageMagick.
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
            for (what, part) in [(Whiten::Teeth, "teeth"), (Whiten::Eyes, "eyes")] {
                let t = std::time::Instant::now();
                let whites = match find_whites(&before, &doc.profile, what) {
                    Ok(whites) => whites,
                    Err(e) => {
                        eprintln!("{name}: {e}");
                        continue;
                    }
                };
                let [bx, by, bw, bh] = whites.bounds().unwrap();
                eprintln!("{name}: {part} in {:?}, {bw} × {bh} at ({bx}, {by})", t.elapsed());
                let mut doc = doc.clone();
                whiten::add_layer(&mut doc, 0, what, &whites, amount);
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
