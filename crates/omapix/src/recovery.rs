//! Crash recovery. An image with unsaved changes is written as an .ora to
//! `~/.cache/omapix/recovery/` once nothing has changed for [`IDLE`], and
//! the file is deleted when the image is saved or closed. Files still there
//! on the next start are offered back.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant, SystemTime};

use omapix_engine::{Document, ora};

use crate::editor::Editor;

/// How long an image must go unchanged before it's written.
pub const IDLE: Duration = Duration::from_secs(10);

/// Threads writing a recovery file.
const WRITERS: usize = 4;

/// `~/.cache/omapix/recovery`. `None` under `cargo test`, which mustn't
/// touch the real cache.
pub(crate) fn dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    (!cfg!(test)).then(|| cache.join("omapix").join("recovery"))
}

/// Kept beside a recovery file: where the image came from, and when it
/// was written.
#[derive(serde::Serialize, serde::Deserialize)]
struct Sidecar {
    path: PathBuf,
    saved_path: Option<PathBuf>,
    round_trip: Option<PathBuf>,
    /// Seconds since 1970.
    time: u64,
}

/// A recovery file left by a run that didn't close cleanly.
#[derive(Clone, Debug)]
pub struct Found {
    /// The .ora.
    pub file: PathBuf,
    /// The image's own file name, or "Untitled".
    pub name: String,
    pub time: SystemTime,
}

impl Found {
    /// How long ago it was written, as "5 min ago".
    pub fn ago(&self) -> String {
        let minutes = self.time.elapsed().unwrap_or_default().as_secs() / 60;
        match minutes {
            0 => "just now".into(),
            1..60 => format!("{minutes} min ago"),
            60..2880 => format!("{} h ago", minutes / 60),
            _ => format!("{} days ago", minutes / 1440),
        }
    }
}

/// The write running in the background.
struct Writing {
    serial: u64,
    revision: u64,
    stem: String,
    rx: Receiver<Result<(), String>>,
}

#[derive(Default)]
pub struct Recovery {
    dir: Option<PathBuf>,
    /// Each open image's revision as last seen, and when it was first seen,
    /// by image (see [`serial`]).
    seen: HashMap<u64, (u64, Instant)>,
    /// Each image's recovery file (its stem) and the revision it holds.
    files: HashMap<u64, (String, u64)>,
    writing: Option<Writing>,
    /// Images saved or closed while being written: their file goes as soon
    /// as the write ends.
    dropped: HashSet<u64>,
}

/// Which open image an editor is: the high half of its revisions.
fn serial(editor: &Editor) -> u64 {
    editor.revision() >> 32
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Recovery {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir, ..Default::default() }
    }

    /// Recovery files left by earlier runs, newest first.
    pub fn found(&self) -> Vec<Found> {
        let Some(dir) = &self.dir else { return Vec::new() };
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut found: Vec<Found> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ora"))
            .filter_map(|file| {
                let sidecar = read_sidecar(&file)?;
                let path = sidecar.saved_path.as_ref().unwrap_or(&sidecar.path);
                let name = path.file_name().map_or("Untitled".into(), |n| n.to_string_lossy().into_owned());
                let time = SystemTime::UNIX_EPOCH + Duration::from_secs(sidecar.time);
                Some(Found { file, name, time })
            })
            .collect();
        found.sort_by_key(|f| std::cmp::Reverse(f.time));
        found
    }

    /// Note what changed and write an image that's been left alone long
    /// enough. Returns how long until it's worth looking again.
    pub fn tick<'a>(&mut self, tabs: impl Iterator<Item = &'a Editor>, now: Instant, ctx: &egui::Context) -> Option<Duration> {
        let dir = self.dir.clone()?;
        if let Some(writing) = &self.writing
            && let Ok(result) = writing.rx.try_recv()
        {
            let Writing { serial, revision, stem, .. } = self.writing.take()?;
            if let Err(e) = result {
                log::warn!("couldn't write a recovery file: {e}");
            }
            if self.dropped.remove(&serial) {
                remove(&dir, &stem);
            } else {
                self.files.insert(serial, (stem, revision));
            }
        }
        let mut open = HashSet::new();
        let mut due = None;
        let mut wait = None::<Duration>;
        for editor in tabs {
            let (serial, revision) = (serial(editor), editor.revision());
            open.insert(serial);
            let seen = self.seen.entry(serial).or_insert((revision, now));
            if seen.0 != revision {
                *seen = (revision, now);
            }
            let written = self.files.get(&serial).is_some_and(|f| f.1 == revision);
            if !editor.modified || written || self.writing.as_ref().is_some_and(|w| w.serial == serial) {
                continue;
            }
            let left = IDLE.saturating_sub(now - seen.1);
            if left.is_zero() {
                due.get_or_insert(editor);
            } else {
                wait = Some(wait.map_or(left, |w| w.min(left)));
            }
        }
        self.seen.retain(|serial, _| open.contains(serial));
        if let Some(editor) = due {
            if self.writing.is_none() {
                self.write(&dir, editor, ctx);
            }
            // Another may be due once this write is done.
            wait = Some(Duration::from_millis(500));
        }
        wait
    }

    /// Write `editor`'s image in the background, to a temporary name that
    /// replaces its recovery file once it's whole.
    fn write(&mut self, dir: &Path, editor: &Editor, ctx: &egui::Context) {
        let serial = serial(editor);
        let stem = match self.files.get(&serial) {
            Some((stem, _)) => stem.clone(),
            None => self.new_stem(dir, &editor.doc),
        };
        let doc = editor.doc.clone();
        let sidecar = Sidecar {
            path: doc.path.clone(),
            saved_path: doc.saved_path.clone(),
            round_trip: doc.round_trip.clone(),
            time: now_secs(),
        };
        let (dir, name, ctx) = (dir.to_owned(), stem.clone(), ctx.clone());
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = write(&dir, &name, &doc, &sidecar);
            log::info!("wrote recovery file {name} in {:?}", started.elapsed());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.writing = Some(Writing {
            serial,
            revision: editor.revision(),
            stem,
            rx,
        });
    }

    /// A name for an image's recovery file: a hash of its path, or the first
    /// free "untitled-N".
    fn new_stem(&self, dir: &Path, doc: &Document) -> String {
        let path = doc.saved_path.as_ref().unwrap_or(&doc.path);
        if !path.as_os_str().is_empty() {
            let mut hasher = std::hash::DefaultHasher::new();
            path.hash(&mut hasher);
            return format!("{:016x}", hasher.finish());
        }
        let taken = |stem: &str| {
            dir.join(format!("{stem}.ora")).exists() || self.files.values().any(|f| f.0 == stem) || self.writing.as_ref().is_some_and(|w| w.stem == stem)
        };
        (1..).map(|n| format!("untitled-{n}")).find(|stem| !taken(stem)).unwrap_or_default()
    }

    /// `editor`'s image was saved or closed: its recovery file goes.
    pub fn forget(&mut self, editor: &Editor) {
        let serial = serial(editor);
        self.seen.remove(&serial);
        if self.writing.as_ref().is_some_and(|w| w.serial == serial) {
            self.dropped.insert(serial);
        }
        if let (Some(dir), Some((stem, _))) = (&self.dir, self.files.remove(&serial)) {
            remove(dir, &stem);
        }
    }

    /// `editor` was opened from the recovery file `file`, and holds what's
    /// in it: it carries on writing there.
    pub fn adopt(&mut self, editor: &Editor, file: &Path) {
        if let Some(stem) = file.file_stem() {
            self.files.insert(serial(editor), (stem.to_string_lossy().into_owned(), editor.revision()));
        }
    }

    /// Whether `path` is a recovery file.
    pub fn holds(&self, path: &Path) -> bool {
        self.dir.as_deref().is_some_and(|dir| path.parent() == Some(dir))
    }

    /// Delete a recovery file left by an earlier run.
    pub fn discard(&self, found: &Found) {
        if let (Some(dir), Some(stem)) = (&self.dir, found.file.file_stem()) {
            remove(dir, &stem.to_string_lossy());
        }
    }

    /// Omapix is closing cleanly: wait for a write under way, and delete
    /// every image's recovery file.
    pub fn close(&mut self) {
        if let Some(writing) = self.writing.take() {
            let _ = writing.rx.recv();
            self.files.insert(writing.serial, (writing.stem, writing.revision));
        }
        if let Some(dir) = &self.dir {
            for (stem, _) in self.files.values() {
                remove(dir, stem);
            }
        }
        self.files.clear();
    }
}

fn write(dir: &Path, stem: &str, doc: &Document, sidecar: &Sidecar) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let partial = dir.join(format!("{stem}.part"));
    // On threads of its own, so a brush's work on the shared pool doesn't
    // wait behind it.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(WRITERS).build().map_err(|e| e.to_string())?;
    pool.install(|| ora::save(doc, &partial)).map_err(|e| e.to_string())?;
    let text = toml::to_string(sidecar).map_err(|e| e.to_string())?;
    let sidecar_partial = dir.join(format!("{stem}.toml.part"));
    std::fs::write(&sidecar_partial, text).map_err(|e| e.to_string())?;
    std::fs::rename(&sidecar_partial, dir.join(format!("{stem}.toml"))).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, dir.join(format!("{stem}.ora"))).map_err(|e| e.to_string())
}

fn read_sidecar(file: &Path) -> Option<Sidecar> {
    toml::from_str(&std::fs::read_to_string(file.with_extension("toml")).ok()?).ok()
}

fn remove(dir: &Path, stem: &str) {
    for extension in ["ora", "toml", "part", "toml.part"] {
        let _ = std::fs::remove_file(dir.join(format!("{stem}.{extension}")));
    }
}

/// Open a recovery file as the image it was written from: it remembers
/// its own path, not the recovery file's.
pub fn load(file: &Path) -> omapix_engine::Result<Document> {
    let mut doc = ora::load(file)?;
    let sidecar = read_sidecar(file);
    doc.path = sidecar.as_ref().map(|s| s.path.clone()).unwrap_or_default();
    doc.saved_path = sidecar.as_ref().and_then(|s| s.saved_path.clone());
    doc.round_trip = sidecar.and_then(|s| s.round_trip);
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::{ColorProfile, Layer, Raster};

    fn wait_for_write(recovery: &mut Recovery, editors: &[&Editor], at: Instant, ctx: &egui::Context) {
        recovery.tick(editors.iter().copied(), at, ctx);
        while recovery.writing.is_some() {
            std::thread::sleep(Duration::from_millis(5));
            recovery.tick(editors.iter().copied(), at, ctx);
        }
    }

    #[test]
    fn untitled_images_get_files_of_their_own_and_a_clean_close_deletes_them() {
        let ctx = egui::Context::default();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/recovery-untitled");
        let _ = std::fs::remove_dir_all(&dir);
        let mut recovery = Recovery::new(Some(dir.clone()));
        let mut a = Editor::new(Document::blank(64, 64)).unwrap();
        let mut b = Editor::new(Document::blank(64, 64)).unwrap();
        a.modified = true;
        b.modified = true;
        let start = Instant::now();
        recovery.tick([&a, &b].into_iter(), start, &ctx);
        // One write at a time.
        wait_for_write(&mut recovery, &[&a, &b], start + IDLE, &ctx);
        wait_for_write(&mut recovery, &[&a, &b], start + IDLE, &ctx);
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["untitled-1.ora", "untitled-1.toml", "untitled-2.ora", "untitled-2.toml"]);
        assert!(recovery.found().iter().all(|f| f.name == "Untitled"));

        // Nothing new to write until it changes again.
        recovery.tick([&a, &b].into_iter(), start + IDLE * 2, &ctx);
        assert!(recovery.writing.is_none());
        a.edit("Edit", |_, _| {});
        recovery.tick([&a, &b].into_iter(), start + IDLE * 2, &ctx);
        assert!(recovery.writing.is_none(), "written as soon as it was edited");

        recovery.forget(&b);
        assert_eq!(recovery.found().len(), 1);
        recovery.close();
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A 24 MP image with five layers: how long its recovery file takes to
    /// write, and how long brush dabs take on the UI thread meanwhile.
    /// `cargo test --release -p omapix recovery_write_time -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn recovery_write_time() {
        let (w, h) = (6000u32, 4000u32);
        let mut seed = 1u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed & 0x3ff) as u16
        };
        // A photo's worth of detail: a gradient with sensor noise.
        let mut layer = |id: u64, tint: u16| {
            let pixels = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let base = ((x + y) * 6) as u16 / 2 + tint;
                    [base + noise(), base / 2 + noise(), base / 3 + noise(), u16::MAX]
                })
                .collect();
            Layer::from_raster(id, format!("Layer {id}"), &Raster::new(w, h, pixels))
        };
        let layers = (1..=5).map(|id| layer(id, id as u16 * 1000)).collect();
        let doc = Document::new("bench.tif".into(), ColorProfile::srgb(), 16, w, h, layers);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/recovery-bench");
        let _ = std::fs::remove_dir_all(&dir);
        let sidecar = Sidecar {
            path: doc.path.clone(),
            saved_path: None,
            round_trip: None,
            time: now_secs(),
        };
        for _ in 0..2 {
            let started = Instant::now();
            write(&dir, "bench", &doc, &sidecar).unwrap();
            let size = std::fs::metadata(dir.join("bench.ora")).unwrap().len();
            println!("write: {:?}, {} MB", started.elapsed(), size / 1_000_000);
        }

        let mut editor = Editor::new(doc.clone()).unwrap();
        let dabs = |editor: &mut Editor, y: f32| {
            let settings = omapix_engine::brush::BrushSettings {
                size: 200.0,
                hardness: 0.5,
                ..Default::default()
            };
            assert!(editor.begin_stroke(
                settings,
                omapix_engine::brush::Paint::Color([0, 0, 0, 65535]),
                crate::tools::Sample::Current,
                Default::default()
            ));
            let mut times = Vec::new();
            for i in 0..250 {
                let started = Instant::now();
                editor.stroke_to(200.0 + i as f32 * 20.0, y, 1.0);
                times.push(started.elapsed());
            }
            editor.end_stroke();
            times.sort();
            let total: Duration = times.iter().sum();
            (total / times.len() as u32, times[times.len() * 95 / 100], times[times.len() - 1])
        };
        println!("dabs alone (mean, 95th, worst): {:?}", dabs(&mut editor, 1000.0));
        let writing = std::thread::spawn(move || write(&dir, "bench", &doc, &sidecar));
        println!("dabs while writing (mean, 95th, worst): {:?}", dabs(&mut editor, 2000.0));
        let still_writing = !writing.is_finished();
        writing.join().unwrap().unwrap();
        assert!(still_writing, "the write ended before the strokes did");
    }
}
