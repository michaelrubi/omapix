//! The brushes loaded from Photoshop's brush files (`.abr`). Omapix keeps a
//! copy of each file in its `brushes` folder and reads them all when it
//! starts; the Brush Settings panel lists them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use egui::{RichText, Ui, vec2};
use omapix_engine::abr::AbrFile;
use omapix_engine::brush::{BrushSettings, Paint, Stroke, Surface};
use omapix_engine::brushes::{Preset, presets};
use omapix_engine::tiled::Tiled;
use omapix_engine::tip::Tip;

use crate::theme::Theme;

/// A brush's picture in the list: this many pixels square.
const THUMBNAIL: u32 = 40;
/// How high the stroke's preview is, in points.
const PREVIEW: f32 = 56.0;

/// What reading the folder found: each file's name and brushes, and what
/// to say about the file just added (and whether it failed).
type Read = (Vec<(String, Vec<Preset>)>, Option<(String, bool)>);

#[derive(Default)]
pub struct Library {
    /// Each file's name and its brushes.
    files: Vec<(String, Vec<Preset>)>,
    /// A picture of each brush's dab, made when the list is first shown.
    thumbnails: Vec<Vec<egui::TextureHandle>>,
    /// A stroke drawn with a brush, at a size in pixels: the preview, kept
    /// until either changes.
    preview: Option<(BrushSettings, [u32; 2], egui::TextureHandle)>,
    reading: Option<Receiver<Read>>,
}

/// Where the brush files are kept: `~/.config/omapix/brushes`.
pub fn folder() -> Option<PathBuf> {
    crate::recent::config_dir().map(|d| d.join("brushes"))
}

/// "A", "A and B", "A, B and C".
fn listed(items: &[&str]) -> String {
    match items {
        [rest @ .., last] if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => items.concat(),
    }
}

/// Every brush file in `folder`, in order of name. `added` is one just put
/// there: how it went is said, and it's taken out again if it can't be read.
fn read(folder: &Path, added: Option<&str>) -> Read {
    let entries = std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path());
    let mut paths: Vec<PathBuf> = entries.filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("abr"))).collect();
    paths.sort();
    let (mut files, mut said) = (Vec::new(), None);
    for path in paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let new = added == Some(&name);
        let found = AbrFile::open(&path).map(|file| presets(&file)).map_err(|e| e.to_string());
        match found.and_then(|(brushes, left_out)| if brushes.is_empty() { Err("no brushes in it".into()) } else { Ok((brushes, left_out)) }) {
            Ok((brushes, left_out)) => {
                if new {
                    let count = if brushes.len() == 1 { "1 brush".into() } else { format!("{} brushes", brushes.len()) };
                    let without = if left_out.is_empty() { String::new() } else { format!(", without their {}", listed(&left_out)) };
                    said = Some((format!("Loaded {count} from {name}{without}"), false));
                }
                let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                files.push((stem, brushes));
            }
            Err(why) if new => {
                let _ = std::fs::remove_file(&path);
                said = Some((format!("Could not load {name}: {why}"), true));
            }
            Err(why) => log::warn!("{}: {why}", path.display()),
        }
    }
    (files, said)
}

/// A picture of one dab of `brush`: white where it paints, to be tinted.
fn thumbnail(brush: &Preset) -> egui::ColorImage {
    let n = THUMBNAIL;
    let mut settings = BrushSettings {
        size: n as f32 - 4.0,
        hardness: brush.hardness.unwrap_or(1.0),
        size_pressure: false,
        opacity_pressure: false,
        ..Default::default()
    };
    // Its shape alone, with nothing varying.
    let (shape, tip) = (&mut settings.dynamics, brush.dynamics);
    (shape.angle, shape.roundness, shape.flip_x, shape.flip_y) = (tip.angle, tip.roundness, tip.flip_x, tip.flip_y);
    let mut out = Surface::Mask(Tiled::new(n, n, 0));
    let mut stroke = Stroke::new(settings, Paint::Mask(u16::MAX), out.clone());
    if let Some(tip) = &brush.tip {
        stroke = stroke.with_tip(tip.clone());
    }
    let tiles = stroke.add_point(n as f32 / 2.0, n as f32 / 2.0, 1.0);
    stroke.apply(&mut out, &tiles);
    picture(out, [n; 2])
}

/// A stroke with `brush` on a strip `width` by `height` pixels, as a pen
/// pressing harder in the middle would draw it: white where it paints. A
/// brush too big for the strip is drawn smaller.
fn stroke_picture(brush: &BrushSettings, tip: Option<Arc<Tip>>, [width, height]: [u32; 2]) -> egui::ColorImage {
    let (w, h) = (width as f32, height as f32);
    let settings = BrushSettings { size: brush.size.min(h / 2.0), opacity: 1.0, ..*brush };
    let mut out = Surface::Mask(Tiled::new(width, height, 0));
    let mut stroke = Stroke::new(settings, Paint::Mask(u16::MAX), out.clone());
    if let Some(tip) = tip {
        stroke = stroke.with_tip(tip);
    }
    let margin = settings.size / 2.0 + 2.0;
    let mut tiles = Vec::new();
    for i in 0..=80 {
        let t = i as f32 / 80.0;
        let y = h / 2.0 - (t * std::f32::consts::TAU).sin() * h * 0.2;
        tiles.extend(stroke.add_point(margin + t * (w - 2.0 * margin), y, (t * std::f32::consts::PI).sin().max(0.05)));
    }
    tiles.sort_unstable();
    tiles.dedup();
    stroke.apply(&mut out, &tiles);
    stroke.finish(&mut out);
    picture(out, [width, height])
}

/// What was painted on a mask, as white that's as opaque as the paint.
fn picture(painted: Surface, [width, height]: [u32; 2]) -> egui::ColorImage {
    let Surface::Mask(mask) = painted else { unreachable!() };
    let rgba: Vec<u8> = mask.to_vec().iter().flat_map(|v| [255, 255, 255, (v >> 8) as u8]).collect();
    egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba)
}

impl Library {
    /// Read the brush files in `folder` in the background. `added` is the
    /// name of one just put there.
    pub fn load(&mut self, folder: PathBuf, added: Option<String>, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read(&folder, added.as_deref()));
            ctx.request_repaint();
        });
        self.reading = Some(rx);
    }

    /// Copy `file` into `folder` and read it, with the brushes already there.
    pub fn import(&mut self, file: &Path, folder: &Path, ctx: &egui::Context) -> Result<(), String> {
        let name = file.file_name().ok_or("That isn't a file")?;
        let copy = folder.join(name);
        // Copying a file onto itself would empty it.
        let there = file.canonicalize().ok().is_some_and(|f| copy.canonicalize().ok() == Some(f));
        if !there {
            let copied = std::fs::create_dir_all(folder).and_then(|_| std::fs::copy(file, &copy));
            copied.map_err(|e| format!("Could not copy {} to {}: {e}", name.to_string_lossy(), folder.display()))?;
        }
        self.load(folder.to_path_buf(), Some(name.to_string_lossy().into_owned()), ctx);
        Ok(())
    }

    /// Take in what was read in the background, once it's done. Gives back
    /// what to say, and whether it's that something went wrong.
    pub fn poll(&mut self) -> Option<(String, bool)> {
        let (files, said) = self.reading.as_ref()?.try_recv().ok()?;
        (self.reading, self.files) = (None, files);
        self.thumbnails.clear();
        self.preview = None;
        said
    }

    /// The sampled tip a brush's settings name, if it's among the brushes.
    pub fn tip(&self, id: Option<u64>) -> Option<Arc<Tip>> {
        let id = id?;
        let tips = self.files.iter().flat_map(|(_, brushes)| brushes).filter_map(|b| b.tip.as_ref());
        tips.into_iter().find(|tip| tip.id() == id).cloned()
    }

    /// A stroke drawn with `brush`, across the panel: drawn again when the
    /// brush changes.
    pub fn preview(&mut self, ui: &mut Ui, theme: &Theme, brush: &BrushSettings) {
        let width = ui.available_width().floor();
        let scale = ui.ctx().pixels_per_point();
        let size = [(width * scale) as u32, (PREVIEW * scale) as u32];
        if size[0] == 0 {
            return;
        }
        if !self.preview.as_ref().is_some_and(|(of, at, _)| of == brush && *at == size) {
            let image = stroke_picture(brush, self.tip(brush.tip), size);
            let texture = ui.ctx().load_texture("brush-preview", image, egui::TextureOptions::LINEAR);
            self.preview = Some((*brush, size, texture));
        }
        if let Some((_, _, texture)) = &self.preview {
            ui.add(egui::Image::new(texture).fit_to_exact_size(vec2(width, PREVIEW)).tint(theme.foreground));
        }
    }

    /// The list of brushes, a file at a time. Gives back the one clicked.
    /// `current` is the tool's brush, whose tip is marked.
    pub fn show(&mut self, ui: &mut Ui, theme: &Theme, current: &BrushSettings) -> Option<Preset> {
        if self.reading.is_some() {
            ui.label(RichText::new("Loading brushes…").color(theme.dark_foreground));
        } else if self.files.is_empty() {
            ui.label(RichText::new("Load… brings in a Photoshop brush file (.abr)").color(theme.dark_foreground));
        }
        if self.thumbnails.len() != self.files.len() {
            let texture = |brush: &Preset| ui.ctx().load_texture("brush", thumbnail(brush), egui::TextureOptions::LINEAR);
            self.thumbnails = self.files.iter().map(|(_, brushes)| brushes.iter().map(texture).collect()).collect();
        }
        let mut chosen = None;
        // Three rows of them before it scrolls.
        egui::ScrollArea::vertical().id_salt("brushes").max_height(150.0).show(ui, |ui| {
            for ((file, brushes), thumbnails) in self.files.iter().zip(&self.thumbnails) {
                ui.label(RichText::new(file).color(theme.dark_foreground).small());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                    for (brush, thumbnail) in brushes.iter().zip(thumbnails) {
                        let tip = brush.tip.as_ref().map(|tip| tip.id());
                        let image = egui::Image::new(thumbnail).fit_to_exact_size(vec2(32.0, 32.0)).tint(theme.foreground);
                        let button = egui::Button::image(image).selected(tip.is_some() && tip == current.tip);
                        let hint = format!("{} · {} px", brush.name, brush.size.round());
                        if ui.add(button).on_hover_text(hint).clicked() {
                            chosen = Some(brush.clone());
                        }
                    }
                });
            }
        });
        chosen
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use omapix_engine::abr::{AbrSample, write_v6};
    use omapix_engine::descriptor::{Descriptor, UnicodeString, Value};

    /// A brush file with a round brush and one with a sampled tip.
    pub(crate) fn brush_file() -> Vec<u8> {
        let name = |name: &str| Descriptor::new("brushPreset").with("Nm  ", Value::Text(UnicodeString::new(name)));
        let round = Descriptor::new("computedBrush").with("Dmtr", Value::Double(20.0)).with("Hrdn", Value::Double(50.0));
        let leaf = Descriptor::new("sampledBrush").with("Dmtr", Value::Double(60.0)).with("sampledData", Value::Text(UnicodeString::new("leaf")));
        let textured = name("Leaf").with("Brsh", Value::Descriptor(leaf)).with("useTexture", Value::Boolean(true));
        let presets = [name("Soft").with("Brsh", Value::Descriptor(round)), textured];
        let pixels = (0..30 * 20).map(|i| if i % 30 < 15 { 255 } else { 0 }).collect();
        let tip = AbrSample { id: "leaf".into(), width: 30, height: 20, depth: 8, data: pixels };
        write_v6(2, &[tip], &[], &presets, true).unwrap()
    }

    /// Wait for the background to finish, and give back what it says.
    pub(crate) fn loaded(library: &mut Library) -> Option<(String, bool)> {
        while library.reading.is_some() {
            if let Some(said) = library.poll() {
                return Some(said);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        None
    }

    #[test]
    fn importing_a_brush_file_keeps_a_copy_and_reads_it_again_next_time() {
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir().join(format!("omapix-brushes-{}", std::process::id()));
        let (downloads, folder) = (dir.join("downloads"), dir.join("config/brushes"));
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("Nature.abr"), brush_file()).unwrap();
        std::fs::write(downloads.join("Broken.ABR"), b"not a brush file").unwrap();

        let mut library = Library::default();
        library.import(&downloads.join("Nature.abr"), &folder, &ctx).unwrap();
        assert_eq!(loaded(&mut library), Some(("Loaded 2 brushes from Nature.abr, without their Texture".into(), false)));
        assert!(folder.join("Nature.abr").exists());
        let names: Vec<_> = library.files.iter().map(|(file, brushes)| (file.as_str(), brushes.len())).collect();
        assert_eq!(names, [("Nature", 2)]);
        // The sampled tip is found by what a tool's settings keep of it.
        let leaf = library.files[0].1[1].tip.clone().unwrap();
        assert_eq!(library.tip(Some(leaf.id())), Some(leaf.clone()));
        assert_eq!((library.tip(Some(1)), library.tip(None)), (None, None));

        // One that can't be read is said, and not kept.
        library.import(&downloads.join("Broken.ABR"), &folder, &ctx).unwrap();
        let (said, failed) = loaded(&mut library).unwrap();
        assert!(said.starts_with("Could not load Broken.ABR: ") && failed, "{said}");
        assert!(!folder.join("Broken.ABR").exists() && library.files.len() == 1);

        // Importing the copy itself leaves it as it is.
        library.import(&folder.join("Nature.abr"), &folder, &ctx).unwrap();
        assert!(loaded(&mut library).is_some_and(|(_, failed)| !failed));
        assert_eq!(std::fs::read(folder.join("Nature.abr")).unwrap(), brush_file());

        // The next run reads the folder without saying anything.
        let mut next = Library::default();
        next.load(folder, None, &ctx);
        assert_eq!(loaded(&mut next), None);
        assert_eq!(next.tip(Some(leaf.id())).map(|tip| tip.size()), Some((30, 20)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_preview_is_a_stroke_with_the_brush_drawn_again_when_it_changes() {
        // How much paint is at a pixel of the strip.
        let alpha = |image: &egui::ColorImage, x: usize, y: usize| image.pixels[y * 240 + x].a();
        let soft = BrushSettings { size: 20.0, hardness: 1.0, ..Default::default() };
        let image = stroke_picture(&soft, None, [240, 56]);
        // How much paint there is in a block of the strip.
        let paint = |image: &egui::ColorImage, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>| -> u32 {
            xs.flat_map(|x| ys.clone().map(move |y| (x, y))).map(|(x, y)| u32::from(alpha(image, x, y))).sum()
        };
        // An S: up on the left and down on the right, solid in the middle.
        assert!(paint(&image, 40..90, 0..28) > 4 * paint(&image, 40..90, 28..56));
        assert!(paint(&image, 150..200, 28..56) > 4 * paint(&image, 150..200, 0..28));
        assert_eq!(alpha(&image, 120, 28), 255);
        // Thin and faint at the ends, where the pen is light, and nothing
        // in the corners.
        assert!(paint(&image, 14..24, 0..56) > 0 && 4 * paint(&image, 14..24, 0..56) < paint(&image, 115..125, 0..56));
        assert_eq!(paint(&image, 0..240, 0..3) + paint(&image, 0..240, 53..56), 0);
        // A brush too big for the strip is drawn at half its height.
        let big = stroke_picture(&BrushSettings { size: 2000.0, size_pressure: false, opacity_pressure: false, ..soft }, None, [240, 56]);
        assert!(alpha(&big, 120, 18) == 255 && alpha(&big, 120, 38) == 255 && alpha(&big, 120, 54) == 0);

        // In the panel it's kept until the brush changes.
        let ctx = egui::Context::default();
        let mut library = Library::default();
        let shown = |library: &mut Library, brush: &BrushSettings| {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 200.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| library.preview(ui, &Theme::default(), brush));
            out.textures_delta.clear();
            library.preview.as_ref().map(|(of, size, texture)| (*of, *size, texture.id()))
        };
        let first = shown(&mut library, &soft).unwrap();
        assert_eq!((first.0, first.1[1]), (soft, 56));
        assert_eq!(shown(&mut library, &soft).unwrap().2, first.2, "the same picture");
        let wet = BrushSettings { hardness: 0.2, ..soft };
        let second = shown(&mut library, &wet).unwrap();
        assert!(second.0 == wet && second.2 != first.2);
    }

    #[test]
    fn a_thumbnail_shows_the_tips_own_shape() {
        let mut library = Library::default();
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir().join(format!("omapix-brush-thumbnails-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Nature.abr"), brush_file()).unwrap();
        library.load(dir.clone(), None, &ctx);
        loaded(&mut library);
        std::fs::remove_dir_all(&dir).unwrap();
        let [soft, leaf] = &library.files[0].1[..] else { panic!() };
        let alpha = |image: &egui::ColorImage, x: usize, y: usize| image.pixels[y * THUMBNAIL as usize + x].a();
        // A soft round one: full in the middle, fading, nothing in the corners.
        let image = thumbnail(soft);
        assert!(alpha(&image, 20, 20) == 255 && (1..255).contains(&alpha(&image, 33, 20)) && alpha(&image, 1, 1) == 0);
        // The leaf: its left half, and wider than it's high.
        let image = thumbnail(leaf);
        assert!(alpha(&image, 10, 20) == 255 && alpha(&image, 30, 20) == 0 && alpha(&image, 10, 4) == 0);
    }
}
