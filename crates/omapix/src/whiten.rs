//! Retouch › Reduce Shine, Lighten Under Eyes, Whiten Teeth and Whiten Eyes
//! (docs/AI.md, milestone 13): the shine on the skin, the shadows under the
//! eyes, the teeth or the whites of the eyes, of the faces the face models
//! find in what the active layer and those below it show, found on a thread
//! of their own, and a masked layer for them above (`omapix_engine::shine`,
//! `under_eyes` and `whiten`). There's no dialog: the layer's opacity is
//! the amount, and its mask can be painted on. Retouch › Makeup (milestone
//! 15) is found the same way, and is a group of painted layers
//! (`omapix_engine::makeup`). Retouch › Regenerate Skin too, with the skin
//! texture FLUX.2 klein draws for the skin as a masked pixel layer
//! (docs/AI.md, "Regenerate Skin").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use omapix_engine::layer::{Layer, Mask};
use omapix_engine::raster::Pixel;
use omapix_engine::shine::{self, Matte};
use omapix_engine::tiled::Tiled;
use omapix_engine::transform::{Resampling, resized};
use omapix_engine::whiten::{self, Whiten};
use omapix_engine::{ColorProfile, DisplayTransform, Raster, Selection, color, filters, makeup, under_eyes};

use crate::editor::{Editor, Target};
use crate::face_selection::{find_makeup, find_skin, find_under_eyes, find_whites};

/// The layer's opacity to start with, and Auto Retouch's Standard.
pub const AMOUNT: f32 = 50.0;

/// What's found and lightened, or toned down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Whites(Whiten),
    UnderEyes,
    Shine,
    Makeup,
    Texture,
}

impl Part {
    /// Its command's name, and its undo step's.
    fn label(self) -> &'static str {
        match self {
            Part::Whites(what) => what.name(),
            Part::UnderEyes => "Lighten Under Eyes",
            Part::Shine => shine::NAME,
            Part::Makeup => "Makeup",
            Part::Texture => TEXTURE,
        }
    }

    /// What the status bar says while it's found.
    pub fn finding(self) -> &'static str {
        match self {
            Part::Whites(Whiten::Teeth) => "Finding teeth…",
            Part::Whites(Whiten::Eyes) => "Finding eyes…",
            Part::UnderEyes => "Finding shadows under the eyes…",
            Part::Shine => "Finding shine…",
            Part::Makeup => "Finding faces…",
            Part::Texture => "Finding skin…",
        }
    }
}

/// What a part's layer is made from: its mask, or for shine its pixels
/// too; for makeup, where each product's layer is painted.
enum Made {
    Mask(Selection),
    Matte(Matte),
    Makeup(Vec<(makeup::Product, Selection)>),
    Texture(Texture),
}

/// Where it arrives once it's found.
type Found = Receiver<Result<Made, String>>;

#[derive(Default)]
pub struct Whitening {
    /// What's being found, and the layer it goes above.
    running: Option<(Part, u64, Found)>,
    /// How far Regenerate Skin has got.
    progress: Arc<Progress>,
}

/// How far Regenerate Skin has got, shared with its thread: tiles done, of
/// how many (0 until the skin's found), and whether it's been told to stop.
#[derive(Default)]
pub struct Progress {
    done: AtomicUsize,
    of: AtomicUsize,
    stop: AtomicBool,
}

impl Whitening {
    /// Find `part` in what the active layer and those below it show.
    pub fn start(&mut self, ctx: &egui::Context, editor: &Editor, part: Part) -> Result<(), String> {
        let index = editor.active_index().ok_or("Select a layer first")?;
        let doc = editor.doc.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        // A new one each time: a run that was stopped may still be finishing
        // a tile, and keeps its own.
        self.progress = Arc::new(Progress::default());
        let progress = Arc::clone(&self.progress);
        std::thread::spawn(move || {
            let image = doc.composite_current_and_below(index);
            let _ = tx.send(match part {
                Part::Whites(what) => find_whites(&image, &doc.profile, what).map(Made::Mask),
                Part::UnderEyes => find_under_eyes(&image, &doc.profile).map(Made::Mask),
                Part::Shine => find_shine(image, &doc.profile).map(Made::Matte),
                Part::Makeup => find_makeup(&image, &doc.profile).map(Made::Makeup),
                Part::Texture => find_texture(image, &doc.profile, |done, of| {
                    progress.done.store(done, Ordering::Relaxed);
                    progress.of.store(of, Ordering::Relaxed);
                    ctx.request_repaint();
                    !progress.stop.load(Ordering::Relaxed)
                })
                .map(Made::Texture),
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
        if part == Part::Texture && self.progress.stop.load(Ordering::Relaxed) {
            self.running = None;
            return None;
        }
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
                (_, Made::Makeup(products)) => makeup::add_layers(doc, index, products),
                (Part::Whites(what), Made::Mask(mask)) => whiten::add_layer(doc, index, what, mask, AMOUNT),
                (_, Made::Mask(mask)) => under_eyes::add_layer(doc, index, mask, AMOUNT),
                (_, Made::Matte(matte)) => shine::add_layer(doc, index, matte, AMOUNT),
                (_, Made::Texture(texture)) => Some(texture.add_layer(doc, index, AMOUNT)),
            };
            if let Some(id) = id {
                *active = id;
            }
        });
        if added {
            // Painting on the layer paints its mask. Makeup's group has
            // none: its layers are painted on.
            editor.target = if part == Part::Makeup { Target::Pixels } else { Target::Mask };
        }
        None
    }

    /// What's being found, if anything.
    pub fn busy(&self) -> Option<Part> {
        self.running.as_ref().map(|(part, ..)| *part)
    }

    /// While Regenerate Skin draws, how many tiles it's done of how many.
    pub fn tiles(&self) -> Option<(usize, usize)> {
        let of = self.progress.of.load(Ordering::Relaxed);
        (self.busy() == Some(Part::Texture) && of > 0).then(|| (self.progress.done.load(Ordering::Relaxed), of))
    }

    /// Stop Regenerate Skin: nothing's added, and from the next
    /// [`Whitening::poll`] Omapix carries on. Its thread finishes the tile
    /// it's on and drops it.
    pub fn stop(&self) {
        if self.busy() == Some(Part::Texture) {
            self.progress.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// The shine on the skin in `image`, its faces and body, measured by its
/// largest face.
fn find_shine(image: Raster, profile: &ColorProfile) -> Result<Matte, String> {
    let found = find_skin(image, profile)?;
    let matte = Matte::new(&found.image, &found.skin, found.iod).ok_or("Found no skin")?;
    if matte.is_empty() { Err("Found no shine".into()) } else { Ok(matte) }
}

/// Regenerate Skin's undo step and layer.
const TEXTURE: &str = "Regenerate Skin";
/// Faces are redrawn at most this many pixels between the eyes: the scale
/// tried on photos, and fewer tiles on large ones.
const TEXTURE_IOD: f32 = 280.0;
/// What's kept of the model's drawing is what a blur of this much (as a
/// share of the distance between the eyes) takes away: pores and fine
/// lines, not the light or the skin's colour, which it changes too.
const TEXTURE_DETAIL: f32 = 3.0 / TEXTURE_IOD;

/// Regenerate Skin's layer: what `bounds` of the image shows, with the
/// skin's texture drawn anew, and the skin as its mask.
pub struct Texture {
    /// Left, top, width and height in the image.
    bounds: [u32; 4],
    pixels: Vec<Pixel>,
    mask: Vec<u16>,
}

impl Texture {
    /// The texture for `skin` in `image`, for eyes `iod` apart, from
    /// `model` (`omapix_ai::flux::add_texture`'s form, less `tell`).
    /// The face is shrunk first if it's larger than [`TEXTURE_IOD`]. Only
    /// how the model changes the image's fine detail is added to it, at
    /// full size, so its colour and light, and detail finer than the model
    /// saw, stay as they were.
    fn new(
        image: &Raster,
        profile: &ColorProfile,
        skin: &Selection,
        iod: f32,
        model: impl FnOnce(&[f32], usize, usize, &[f32]) -> Result<Option<Vec<f32>>, String>,
    ) -> Result<Self, String> {
        let bounds @ [x, y, w, h] = skin.bounds().ok_or("Found no skin")?;
        let scale = (TEXTURE_IOD / iod).min(1.0);
        let (sw, sh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
        let crop = Tiled::from_slice(w, h, [0; 4], &Tiled::from_raster(image).crop(x, y, w, h));
        let small = resized(&crop, sw, sh, Resampling::Bicubic);
        let (n, plane) = ((sw * sh) as usize, (sw * sh) as usize);
        let mut srgb = vec![[0u8; 4]; n];
        DisplayTransform::to_srgb(profile)
            .map_err(|e| e.to_string())?
            .convert(small.to_raster().pixels(), &mut srgb);
        let planes: Vec<f32> = (0..3 * plane).map(|i| f32::from(srgb[i % plane][i / plane]) / 255.0).collect();
        let wanted: Vec<f32> = (0..n)
            .map(|i| {
                let at = |p: usize, size: u32, from: u32| from + (((p as f32 + 0.5) / scale) as u32).min(size - 1);
                skin.at(at(i % sw as usize, w, x), at(i / sw as usize, h, y))
            })
            .collect();
        let made = model(&planes, sw as usize, sh as usize, &wanted)?.ok_or("Regenerate Skin was stopped")?;
        let made: Vec<Pixel> = (0..n)
            .map(|i| {
                let c = |c: usize| (made[c * plane + i].clamp(0.0, 1.0) * 65535.0).round() as u16;
                [c(0), c(1), c(2), u16::MAX]
            })
            .collect();
        let made = color::convert(&Tiled::from_slice(sw, sh, [0; 4], &made), &ColorProfile::srgb(), profile).map_err(|e| e.to_string())?;

        // The change in fine detail, about half way up the range so it can
        // be enlarged as a picture.
        let sigma = TEXTURE_DETAIL * iod * scale;
        let (made_low, small_low) = (filters::gaussian_blur(&made, sigma), filters::gaussian_blur(&small, sigma));
        let (made, made_low, small, small_low) = (made.to_raster(), made_low.to_raster(), small.to_raster(), small_low.to_raster());
        let change: Vec<Pixel> = (0..n)
            .map(|i| {
                let c = |c: usize| {
                    let detail = |a: &Raster, b: &Raster| i32::from(a.pixels()[i][c]) - i32::from(b.pixels()[i][c]);
                    (detail(&made, &made_low) - detail(&small, &small_low) + 32768).clamp(0, 65535) as u16
                };
                [c(0), c(1), c(2), u16::MAX]
            })
            .collect();
        let change = resized(&Tiled::from_slice(sw, sh, [0; 4], &change), w, h, Resampling::Bicubic).to_raster();
        let pixels = crop
            .to_raster()
            .pixels()
            .iter()
            .zip(change.pixels())
            .map(|(p, d)| {
                let c = |c: usize| (i32::from(p[c]) + i32::from(d[c]) - 32768).clamp(0, 65535) as u16;
                [c(0), c(1), c(2), p[3]]
            })
            .collect();
        let mask = (0..w * h).map(|i| (skin.at(x + i % w, y + i / w) * 65535.0).round() as u16).collect();
        Ok(Self { bounds, pixels, mask })
    }

    /// Add it as a layer above `above`, at `amount` % opacity; its id.
    fn add_layer(&self, doc: &mut omapix_engine::Document, above: usize, amount: f32) -> u64 {
        let [x, y, w, h] = self.bounds;
        let (dx, dy) = (x as i32, y as i32);
        let id = doc.next_layer_id();
        let pixels = Tiled::from_slice(w, h, [0; 4], &self.pixels).reframed(doc.width, doc.height, dx, dy, [0; 4]);
        let mut layer = Layer::from_pixels(id, TEXTURE, pixels);
        layer.mask = Some(Mask {
            pixels: Tiled::from_slice(w, h, 0, &self.mask).reframed(doc.width, doc.height, dx, dy, 0),
            enabled: true,
        });
        layer.opacity = amount / 100.0;
        doc.insert_above(above, layer);
        id
    }
}

/// The skin in `image` with its texture drawn anew, measured by its
/// largest face. The other models give the card up first. `tell` hears
/// how many tiles are done of how many, and stops it by answering `false`.
fn find_texture(image: Raster, profile: &ColorProfile, tell: impl FnMut(usize, usize) -> bool) -> Result<Texture, String> {
    let found = find_skin(image, profile)?;
    crate::generative_fill::free_the_card();
    Texture::new(&found.image, profile, &found.skin, found.iod, |image, width, height, wanted| {
        omapix_ai::flux::add_texture(image, width, height, wanted, tell)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::Document;

    #[test]
    fn regenerate_skin_says_how_far_it_has_got_and_stops_at_once() {
        let doc = Document::from_image("t.tif".into(), &Raster::new(60, 40, vec![[0; 4]; 60 * 40]), ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();
        let (tx, rx) = channel();
        let mut whitening = Whitening {
            running: Some((Part::Texture, editor.active, rx)),
            ..Default::default()
        };
        // Finding the skin, then drawing.
        assert_eq!((whitening.busy(), whitening.tiles()), (Some(Part::Texture), None));
        whitening.progress.of.store(9, Ordering::Relaxed);
        whitening.progress.done.store(3, Ordering::Relaxed);
        assert_eq!(whitening.tiles(), Some((3, 9)));

        // Stopped: not busy, the thread's told, and what it sends after is
        // dropped, with nothing added and nothing to say.
        let progress = Arc::clone(&whitening.progress);
        whitening.stop();
        assert!(progress.stop.load(Ordering::Relaxed));
        assert_eq!(whitening.poll(&mut editor), None);
        assert_eq!((whitening.busy(), whitening.tiles()), (None, None));
        assert!(tx.send(Err("Regenerate Skin was stopped".into())).is_err());
        assert_eq!(whitening.poll(&mut editor), None);
        assert_eq!(editor.doc.layers.len(), 1);

        // Stop only stops Regenerate Skin.
        let (_tx, rx) = channel();
        whitening.progress = Arc::default();
        whitening.running = Some((Part::Shine, editor.active, rx));
        whitening.stop();
        assert_eq!(whitening.poll(&mut editor), None);
        assert_eq!(whitening.busy(), Some(Part::Shine));
    }

    #[test]
    fn regenerated_texture_is_a_masked_layer_of_only_the_finer_detail_the_model_changed() {
        // Plain skin, its left half found as skin, with a face 560 px
        // between the eyes: shrunk by half for the model.
        let image = Raster::new(400, 300, vec![[42000, 33000, 28000, 65535]; 400 * 300]);
        let skin = Selection::rectangle(400, 300, (0.0, 0.0), (200.0, 300.0));
        let profile = ColorProfile::srgb();
        let mut seen = (0, 0);
        // The model draws fine dots everywhere and makes it all paler.
        let texture = Texture::new(&image, &profile, &skin, 560.0, |planes, w, h, wanted| {
            seen = (w, h);
            assert!(wanted[0] > 0.9 && wanted[w - 1] > 0.9);
            Ok(Some(
                planes.iter().enumerate().map(|(i, v)| v + 0.1 + if i % 7 == 0 { 0.2 } else { 0.0 }).collect(),
            ))
        })
        .unwrap();
        assert_eq!(seen, (100, 150));
        assert_eq!(texture.bounds, [0, 0, 200, 300]);
        let mean = |c: usize| texture.pixels.iter().map(|p| f64::from(p[c])).sum::<f64>() / texture.pixels.len() as f64;
        assert!((mean(0) - 42000.0).abs() < 300.0, "not paler: {}", mean(0));
        let spread = texture.pixels.iter().map(|p| (f64::from(p[0]) - mean(0)).abs()).sum::<f64>() / texture.pixels.len() as f64;
        assert!(spread > 300.0, "with the dots: {spread}");

        let mut doc = Document::from_image("t.tif".into(), &image, profile, 16);
        let id = texture.add_layer(&mut doc, 0, AMOUNT);
        let layer = doc.layer(id).unwrap();
        assert_eq!((layer.name.as_str(), layer.opacity), (TEXTURE, AMOUNT / 100.0));
        let after = doc.composite();
        assert_eq!(after.get(300, 150), image.get(300, 150), "not skin");
        assert!((0..200).any(|x| after.get(x, 150) != image.get(x, 150)), "skin");

        // A model that changes nothing leaves the image as it was.
        let same = Texture::new(&image, &ColorProfile::srgb(), &skin, 560.0, |planes, _, _, _| Ok(Some(planes.to_vec()))).unwrap();
        assert!(same.pixels.iter().all(|p| p.iter().zip(image.get(0, 0)).all(|(a, b)| a.abs_diff(b) < 300)));
    }

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
            ..Default::default()
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
    fn makeup_found_becomes_a_group_of_painted_layers_in_one_undo_step() {
        let image = Raster::new(600, 400, vec![[52000, 40000, 34000, 65535]; 600 * 400]);
        let doc = Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16);
        let mut editor = Editor::new(doc).unwrap();

        let (tx, rx) = channel();
        let mut whitening = Whitening {
            running: Some((Part::Makeup, editor.active, rx)),
            ..Default::default()
        };
        assert_eq!(whitening.busy(), Some(Part::Makeup));
        let lips = Selection::rectangle(600, 400, (250.0, 300.0), (350.0, 330.0));
        let cheeks = Selection::ellipse(600, 400, (100.0, 150.0), (200.0, 250.0));
        tx.send(Ok(Made::Makeup(vec![(makeup::Product::Blush, cheeks), (makeup::Product::Lipstick, lips)])))
            .unwrap();
        assert_eq!(whitening.poll(&mut editor), None);

        // The group's selected, to fade all of it; its layers are painted
        // on, not masked.
        let group = editor.doc.layer(editor.active).unwrap();
        assert!(group.is_group && group.name == "Makeup");
        assert_eq!(editor.target, Target::Pixels);
        let names: Vec<_> = editor.doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Background", "Blush", "Lipstick", "Makeup"]);
        let after = editor.doc.composite();
        assert!(after.get(300, 315)[1] < image.get(300, 315)[1] - 1000, "redder lips");
        assert_eq!(after.get(450, 100), image.get(450, 100));
        assert_eq!(editor.undo_label(), Some("Makeup"));
        editor.undo();
        assert_eq!(editor.doc.layers.len(), 1);
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
            ..Default::default()
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
            let Some(matte) = Matte::new(&found.image, &found.skin, found.iod) else {
                continue;
            };
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
            std::process::Command::new("magick")
                .args([&ppm_path, &format!("{out}/{name}-shine.png")])
                .status()
                .unwrap();
            std::fs::remove_file(ppm_path).unwrap();
        }
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a folder
    /// of them), the photo with its skin regenerated, as
    /// `<photo>-texture.png` in `OMAPIX_FACE_OUT`. `AMOUNT` sets the
    /// layer's opacity (100 by default). Needs the models
    /// (scripts/fetch-models.sh, with fill-flux2-klein-4b) and ImageMagick.
    #[test]
    #[ignore]
    fn regenerate_skin_in_photos() {
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        let amount = std::env::var("AMOUNT").map_or(100.0, |v| v.parse().unwrap());
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(mut doc) = omapix_engine::io::load(&path) else { continue };
            let t = std::time::Instant::now();
            let texture = match find_texture(doc.composite(), &doc.profile, |done, of| {
                eprintln!("{name}: {done} of {of}");
                true
            }) {
                Ok(texture) => texture,
                Err(e) => {
                    eprintln!("{name}: {e}");
                    continue;
                }
            };
            eprintln!("{name}: {:?} in {:?}", texture.bounds, t.elapsed());
            texture.add_layer(&mut doc, 0, amount);
            let after = doc.composite();
            let mut ppm = format!("P6 {} {} 255\n", doc.width, doc.height).into_bytes();
            let srgb = omapix_engine::DisplayTransform::to_srgb(&doc.profile).unwrap();
            let mut shown = vec![[0u8; 4]; after.pixels().len()];
            srgb.convert(after.pixels(), &mut shown);
            ppm.extend(shown.iter().flat_map(|p| [p[0], p[1], p[2]]));
            let ppm_path = format!("{out}/{name}-texture.ppm");
            std::fs::write(&ppm_path, ppm).unwrap();
            std::process::Command::new("magick")
                .args([&ppm_path, &format!("{out}/{name}-texture.png")])
                .status()
                .unwrap();
            std::fs::remove_file(ppm_path).unwrap();
        }
    }

    /// A look at real photos: for `OMAPIX_FACE_PHOTO` (a photo, or a folder
    /// of them), its faces before and made up, side by side as
    /// `<photo>-makeup.png` in `OMAPIX_FACE_OUT`. `AMOUNT` scales every
    /// layer's opacity (100 leaves them as they start). Needs the models
    /// (scripts/fetch-models.sh) and ImageMagick.
    #[test]
    #[ignore]
    fn makeup_in_photos() {
        use omapix_engine::selection::Combine;
        let (Ok(photos), Ok(out)) = (std::env::var("OMAPIX_FACE_PHOTO"), std::env::var("OMAPIX_FACE_OUT")) else {
            return;
        };
        let photos = std::path::Path::new(&photos);
        let mut paths: Vec<_> = match std::fs::read_dir(photos) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => vec![photos.to_owned()],
        };
        paths.sort();
        let amount = std::env::var("AMOUNT").map_or(100.0, |v| v.parse().unwrap());
        for path in paths {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(mut doc) = omapix_engine::io::load(&path) else { continue };
            let before = doc.composite();
            let t = std::time::Instant::now();
            let products = match crate::face_selection::find_makeup(&before, &doc.profile) {
                Ok(products) => products,
                Err(e) => {
                    eprintln!("{name}: {e}");
                    continue;
                }
            };
            let all = products.iter().map(|(_, on)| on.clone()).reduce(|a, b| a.combine(&b, Combine::Add)).unwrap();
            let Some([bx, by, bw, bh]) = all.bounds() else { continue };
            eprintln!("{name}: found in {:?}, {bw} × {bh} at ({bx}, {by})", t.elapsed());
            let group = omapix_engine::makeup::add_layers(&mut doc, 0, &products).unwrap();
            for layer in doc.layers.iter_mut().filter(|l| l.parent == Some(group)) {
                layer.opacity = (layer.opacity * amount / 100.0).min(1.0);
            }
            let after = doc.composite();
            let pad = bw / 5;
            let (x0, y0) = (bx.saturating_sub(pad), by.saturating_sub(pad));
            let (x1, y1) = ((bx + bw + pad).min(doc.width), (by + bh + pad).min(doc.height));
            let step = (x1 - x0).div_ceil(1000);
            let (ow, oh) = ((x1 - x0) / step, (y1 - y0) / step);
            let mut ppm = format!("P6 {} {} 255\n", ow * 2, oh).into_bytes();
            let srgb = omapix_engine::DisplayTransform::to_srgb(&doc.profile).unwrap();
            for y in 0..oh {
                for half in [&before, &after] {
                    let row: Vec<_> = (0..ow).map(|x| half.get(x0 + x * step, y0 + y * step)).collect();
                    let mut shown = vec![[0u8; 4]; row.len()];
                    srgb.convert(&row, &mut shown);
                    ppm.extend(shown.iter().flat_map(|p| [p[0], p[1], p[2]]));
                }
            }
            let ppm_path = format!("{out}/{name}-makeup.ppm");
            std::fs::write(&ppm_path, ppm).unwrap();
            std::process::Command::new("magick")
                .args([&ppm_path, &format!("{out}/{name}-makeup.png")])
                .status()
                .unwrap();
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
                std::process::Command::new("magick")
                    .args([&ppm_path, "-filter", "point", "-resize", "1800x>", &png])
                    .status()
                    .unwrap();
                std::fs::remove_file(ppm_path).unwrap();
            }
        }
    }
}
