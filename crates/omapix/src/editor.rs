//! One open document: its layers, undo history, selection, and the
//! background work that keeps the canvas up to date.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use omapix_engine::{DisplayTransform, Document};

use crate::canvas::{Canvas, Render};

/// Undo steps kept. Snapshots share unchanged tiles, so this mostly costs
/// memory for pixels that edits actually replaced.
const HISTORY_LIMIT: usize = 50;

/// Which part of the active layer edits apply to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Pixels,
    Mask,
}

#[derive(Clone)]
struct Snapshot {
    label: String,
    doc: Document,
    active: u64,
}

struct Job {
    label: String,
    rx: Receiver<(Document, u64)>,
}

pub struct Editor {
    pub doc: Document,
    /// Id of the selected layer.
    pub active: u64,
    pub target: Target,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Key of the continuous edit in progress (e.g. dragging the opacity
    /// slider), so a whole drag is one undo step.
    live: Option<String>,
    /// Incremented on every change to `doc`.
    revision: u64,
    /// Revision the canvas is showing or being rendered.
    rendering: Option<(u64, Receiver<Render>)>,
    rendered: u64,
    /// A slow operation running in the background. Edits are refused until
    /// it finishes.
    job: Option<Job>,
    /// Changed since last saved.
    pub modified: bool,
    pub canvas: Canvas,
}

impl Editor {
    pub fn new(doc: Document) -> Result<Self, String> {
        let transform = DisplayTransform::to_srgb(&doc.profile).map_err(|e| e.to_string())?;
        let canvas = Canvas::new(doc.width, doc.height, transform);
        let active = doc.layers.last().map_or(0, |l| l.id);
        Ok(Self {
            doc,
            active,
            target: Target::Pixels,
            undo: Vec::new(),
            redo: Vec::new(),
            live: None,
            revision: 1,
            rendering: None,
            rendered: 0,
            job: None,
            modified: false,
            canvas,
        })
    }

    pub fn active_index(&self) -> Option<usize> {
        self.doc.index_of(self.active)
    }

    pub fn busy(&self) -> Option<&str> {
        self.job.as_ref().map(|j| j.label.as_str())
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    fn snapshot(&self, label: &str) -> Snapshot {
        Snapshot {
            label: label.to_owned(),
            doc: self.doc.clone(),
            active: self.active,
        }
    }

    fn push_undo(&mut self, snapshot: Snapshot) {
        self.undo.push(snapshot);
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn changed(&mut self) {
        self.revision += 1;
        self.modified = true;
    }

    /// Apply an edit as one undo step. Returns false if a background job is
    /// running and the edit was refused.
    pub fn edit(&mut self, label: &str, f: impl FnOnce(&mut Document, &mut u64)) -> bool {
        if self.job.is_some() {
            return false;
        }
        self.live = None;
        let before = self.snapshot(label);
        f(&mut self.doc, &mut self.active);
        self.push_undo(before);
        self.changed();
        true
    }

    /// Apply part of a continuous edit. Consecutive calls with the same key
    /// form one undo step, until [`Self::end_live`].
    pub fn edit_live(&mut self, key: &str, f: impl FnOnce(&mut Document)) {
        if self.job.is_some() {
            return;
        }
        if self.live.as_deref() != Some(key) {
            let before = self.snapshot(key);
            self.push_undo(before);
            self.live = Some(key.to_owned());
        }
        f(&mut self.doc);
        self.changed();
    }

    pub fn end_live(&mut self) {
        self.live = None;
    }

    /// Run a slow edit on a background thread as one undo step.
    pub fn edit_in_background(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Document, &mut u64) + Send + 'static,
        ctx: &egui::Context,
    ) {
        if self.job.is_some() {
            return;
        }
        self.live = None;
        let (tx, rx) = channel();
        let mut doc = self.doc.clone();
        let mut active = self.active;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            f(&mut doc, &mut active);
            let _ = tx.send((doc, active));
            ctx.request_repaint();
        });
        self.job = Some(Job {
            label: label.to_owned(),
            rx,
        });
    }

    pub fn undo(&mut self) {
        if self.job.is_some() {
            return;
        }
        self.live = None;
        if let Some(prev) = self.undo.pop() {
            let current = Snapshot {
                label: prev.label.clone(),
                doc: self.doc.clone(),
                active: self.active,
            };
            self.redo.push(current);
            self.restore(prev);
        }
    }

    pub fn redo(&mut self) {
        if self.job.is_some() {
            return;
        }
        self.live = None;
        if let Some(next) = self.redo.pop() {
            let current = Snapshot {
                label: next.label.clone(),
                doc: self.doc.clone(),
                active: self.active,
            };
            self.undo.push(current);
            self.restore(next);
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        // Keep the save location: undo shouldn't forget where the file lives.
        let saved_path = self.doc.saved_path.clone();
        self.doc = snapshot.doc;
        self.doc.saved_path = saved_path;
        self.active = snapshot.active;
        self.fix_selection();
        self.changed();
    }

    /// Keep the selection pointing at something that exists.
    pub fn fix_selection(&mut self) {
        if self.doc.index_of(self.active).is_none() {
            self.active = self.doc.layers.last().map_or(0, |l| l.id);
        }
        let has_mask = self
            .doc
            .layer(self.active)
            .is_some_and(|l| l.mask.is_some());
        if self.target == Target::Mask && !has_mask {
            self.target = Target::Pixels;
        }
    }

    /// Pick up finished background work and start rendering if the document
    /// changed. Call once per frame.
    pub fn update(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.job
            && let Ok((doc, active)) = job.rx.try_recv()
        {
            let label = job.label.clone();
            self.job = None;
            let before = self.snapshot(&label);
            // A save may have finished while the job ran.
            let saved_path = self.doc.saved_path.clone();
            self.doc = doc;
            self.doc.saved_path = saved_path;
            self.active = active;
            self.push_undo(before);
            self.fix_selection();
            self.changed();
        }

        if let Some((revision, rx)) = &self.rendering
            && let Ok(render) = rx.try_recv()
        {
            self.rendered = *revision;
            self.canvas.set_render(Arc::new(render));
            self.rendering = None;
        }
        // One render at a time; when it finishes, the latest state is
        // rendered next, so fast slider drags skip intermediate states.
        if self.rendering.is_none() && self.rendered != self.revision {
            let (tx, rx) = channel();
            let doc = self.doc.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let render = Render::new(doc.composite());
                let _ = tx.send(render);
                ctx.request_repaint();
            });
            self.rendering = Some((self.revision, rx));
        }
    }

    /// Identifies the current state of the document; changes on every edit.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Mark the document saved, unless it changed since `revision` (the
    /// state that was written).
    pub fn mark_saved(&mut self, revision: u64) {
        if revision == self.revision {
            self.modified = false;
        }
    }
}
