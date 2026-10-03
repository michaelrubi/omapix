//! Object Selection and Quick Selection with SAM 2.1 (see omapix-ai), on a
//! thread of their own: the model is loaded on first use, the image
//! encoded once per change of the document, then each click, box or brush
//! stroke is answered in about a tenth of a second.
//!
//! What's being selected is a session: the prompt so far (a click, a box,
//! or Quick Selection's painted points), and the selection it's combined
//! with. While a stroke is painted or the threshold dragged, answers are
//! only shown (as [`ObjectSelection::preview`]); when it ends, the answer
//! becomes the selection, as one undo step. Later strokes, and the
//! threshold, change the same object for as long as the selection is the
//! one the session made.

use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use egui::Pos2;
use omapix_ai::sam::{Encoded, MASK_SIZE, Prompt, Sam};
use omapix_engine::selection::{Combine, Selection};
use omapix_engine::tiled::Tiled;
use omapix_engine::{ColorProfile, DisplayTransform, refine};

use crate::canvas::Render;
use crate::editor::Editor;

/// A drag shorter than this (image pixels) is a click.
const CLICK: f32 = 3.0;
/// The most painted points sent to the model; it does best with a few.
const MOST_POINTS: usize = 24;

/// What a click or drag from `a` to `b` asks for: the object under a click,
/// or in the box dragged.
pub fn prompt(a: Pos2, b: Pos2) -> Prompt {
    if a.distance(b) < CLICK {
        Prompt::Point(a.x, a.y)
    } else {
        Prompt::Box(a.x, a.y, b.x, b.y)
    }
}

/// What the worker's asked to do.
enum Job {
    /// Answer `Prompt`, refining the last answer if `true`.
    Select(Prompt, bool),
    /// Cut the last answer at a new threshold.
    Threshold,
}

struct Request {
    /// To wake the UI when the answer's ready.
    ctx: egui::Context,
    /// The flattened image, and which revision of the document it shows.
    image: Arc<Render>,
    revision: u64,
    profile: ColorProfile,
    job: Job,
    threshold: f32,
    /// Make the answer the selection, rather than just show it.
    commit: bool,
}

struct Answer {
    coverage: Result<Tiled<u16>, String>,
    commit: bool,
    /// Requests this answers: those overtaken by a newer one too.
    answers: usize,
}

/// The worker thread: where to send requests, and where answers come back.
type Worker = (Sender<Request>, Receiver<Answer>);

/// The object being selected.
struct Session {
    label: &'static str,
    prompt: Prompt,
    /// The selection it's combined with, and how.
    base: Option<Selection>,
    how: Combine,
    /// The document's revision once its answer became the selection.
    committed: Option<u64>,
    /// Quick Selection: the stroke being painted is of points in the
    /// object (or with Alt, not in it).
    inside: bool,
}

#[derive(Default)]
pub struct ObjectSelection {
    worker: Option<Worker>,
    /// Asked, to send on the next poll: the newest overtakes what's waiting.
    asked: Option<(Job, f32, bool)>,
    /// Sent and not yet answered.
    waiting: usize,
    session: Option<Session>,
    /// The selection as it will be, shown while a stroke is painted or the
    /// threshold dragged.
    pub preview: Option<Selection>,
}

impl ObjectSelection {
    /// Select the object a click or box points to, combined with the
    /// selection `how`.
    pub fn click(&mut self, editor: &Editor, prompt: Prompt, how: Combine, threshold: f32) {
        self.start(editor, "Object Selection", prompt.clone(), how);
        self.asked = Some((Job::Select(prompt, false), threshold, true));
    }

    /// Quick Selection: a stroke starts at `p`. It goes on selecting the
    /// same object, taking away from it with Alt, if the selection is still
    /// the one it made; otherwise it starts a new one (added with Shift,
    /// subtracted with Alt).
    pub fn begin_stroke(&mut self, editor: &Editor, p: Pos2, how: Combine, threshold: f32) {
        let going_on = self.session.as_ref().is_some_and(|s| {
            s.label == "Quick Selection" && s.committed == Some(editor.revision())
        });
        if going_on {
            if let Some(session) = &mut self.session {
                session.inside = how != Combine::Subtract;
            }
        } else {
            self.start(editor, "Quick Selection", Prompt::Points(Vec::new()), how);
        }
        self.paint(p, 0.0, threshold);
    }

    /// Quick Selection: the stroke has reached `p`, painting with a brush of
    /// `radius`. The answer's shown, not yet made the selection.
    pub fn paint(&mut self, p: Pos2, radius: f32, threshold: f32) {
        let Some(Session {
            prompt: Prompt::Points(points),
            inside,
            ..
        }) = &mut self.session
        else {
            return;
        };
        // A point a brush's radius on from the last.
        if points.last().is_some_and(|&(x, y, _)| p.distance(egui::pos2(x, y)) < radius) {
            return;
        }
        points.push((p.x, p.y, *inside));
        let refine = points.len() > 1;
        self.asked = Some((Job::Select(Prompt::Points(sparse(points)), refine), threshold, false));
    }

    /// Quick Selection: the stroke's over; its answer becomes the selection.
    pub fn end_stroke(&mut self, threshold: f32) {
        if let Some(Session { prompt: Prompt::Points(points), .. }) = &self.session
            && !points.is_empty()
        {
            let prompt = Prompt::Points(sparse(points));
            self.asked = Some((Job::Select(prompt, true), threshold, true));
        }
    }

    /// The threshold's changed: cut the last answer at it again, showing
    /// it, or with `commit`, making it the selection. Only while the
    /// selection is still the one the session made.
    pub fn threshold(&mut self, editor: &Editor, threshold: f32, commit: bool) {
        if self.session.as_ref().is_some_and(|s| s.committed == Some(editor.revision())) {
            self.asked = Some((Job::Threshold, threshold, commit));
        }
    }

    fn start(&mut self, editor: &Editor, label: &'static str, prompt: Prompt, how: Combine) {
        let base = editor.doc.selection.clone().filter(|_| how != Combine::Replace);
        self.session = Some(Session {
            label,
            prompt,
            base,
            how,
            committed: None,
            inside: true,
        });
    }

    /// Send what's been asked about `editor`'s image, and show or apply an
    /// answer once there is one. Returns what went wrong, if anything.
    pub fn poll(&mut self, ctx: &egui::Context, editor: &mut Editor) -> Option<String> {
        if let Some(image) = editor.canvas.render()
            && let Some((job, threshold, commit)) = self.asked.take()
        {
            let (tx, _) = self.worker.get_or_insert_with(spawn);
            let request = Request {
                ctx: ctx.clone(),
                image: Arc::clone(image),
                revision: editor.revision(),
                profile: editor.doc.profile.clone(),
                job,
                threshold,
                commit,
            };
            if tx.send(request).is_ok() {
                self.waiting += 1;
            }
        }
        let answer = match self.worker.as_ref()?.1.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.worker = None;
                self.waiting = 0;
                return None;
            }
        };
        self.waiting = self.waiting.saturating_sub(answer.answers);
        let coverage = match answer.coverage {
            Ok(coverage) => coverage,
            Err(e) => {
                self.preview = None;
                return Some(e);
            }
        };
        let session = self.session.as_mut()?;
        let found = Selection::from_coverage(coverage);
        let selection = match &session.base {
            Some(base) => base.combine(&found, session.how),
            None => found,
        };
        if answer.commit {
            self.preview = None;
            editor.set_selection(session.label, selection, Combine::Replace);
            session.committed = Some(editor.revision());
        } else {
            self.preview = Some(selection);
        }
        None
    }

    pub fn busy(&self) -> bool {
        self.waiting > 0 || self.asked.is_some()
    }
}

/// At most [`MOST_POINTS`] of `points`, evenly spread, and every point
/// that's not in the object (there are few, and each matters).
fn sparse(points: &[(f32, f32, bool)]) -> Vec<(f32, f32, bool)> {
    let step = points.len().div_ceil(MOST_POINTS).max(1);
    points
        .iter()
        .enumerate()
        .filter(|&(i, &(_, _, inside))| !inside || i % step == 0 || i + 1 == points.len())
        .map(|(_, &p)| p)
        .collect()
}

/// Loaded on first use, and kept until Generative Fill needs the card.
static SAM: Mutex<Option<Sam>> = Mutex::new(None);

/// Drop the model, giving its GPU memory back. It loads again when next
/// used.
pub fn unload() {
    if let Ok(mut sam) = SAM.lock() {
        *sam = None;
    }
}

fn spawn() -> Worker {
    let (tx, requests) = channel::<Request>();
    let (answers, rx) = channel();
    std::thread::spawn(move || {
        let mut encoded: Option<(u64, Encoded)> = None;
        let mut logits: Option<Vec<f32>> = None;
        while let Ok(mut request) = requests.recv() {
            // Answer only the newest; a commit among those overtaken
            // commits it.
            let mut count = 1;
            while let Ok(newer) = requests.try_recv() {
                let commit = request.commit || newer.commit;
                request = Request { commit, ..newer };
                count += 1;
            }
            let coverage = find(&mut encoded, &mut logits, &request);
            let answer = Answer {
                coverage,
                commit: request.commit,
                answers: count,
            };
            if answers.send(answer).is_err() {
                break;
            }
            request.ctx.request_repaint();
        }
    });
    (tx, rx)
}

fn find(
    encoded: &mut Option<(u64, Encoded)>,
    logits: &mut Option<Vec<f32>>,
    request: &Request,
) -> Result<Tiled<u16>, String> {
    if let Job::Select(prompt, refine) = &request.job {
        let mut sam = SAM.lock().map_err(|e| e.to_string())?;
        let sam = match &mut *sam {
            Some(sam) => sam,
            None => sam.insert(Sam::load()?),
        };
        if encoded.as_ref().is_none_or(|(revision, _)| *revision != request.revision) {
            let transform = DisplayTransform::to_srgb(&request.profile).map_err(|e| e.to_string())?;
            let image = request.image.with_image(|image| {
                let mut srgb = vec![[0u8; 4]; image.pixels().len()];
                transform.convert(image.pixels(), &mut srgb);
                sam.encode(&srgb, image.width(), image.height())
            });
            *encoded = Some((request.revision, image.ok_or("The image isn't ready yet")??));
            *logits = None;
        }
        let (_, image) = encoded.as_ref().expect("just encoded");
        let previous = logits.as_deref().filter(|_| *refine);
        *logits = Some(sam.select(image, prompt, previous)?);
    }
    let logits = logits.as_deref().ok_or("Select an object first")?;
    request
        .image
        .with_image(|image| refine::mask_coverage(logits, MASK_SIZE, MASK_SIZE, request.threshold, image))
        .ok_or_else(|| "The image isn't ready yet".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn clicks_ask_for_the_object_under_them_and_drags_for_a_box() {
        assert_eq!(prompt(pos2(10.0, 20.0), pos2(11.0, 21.0)), Prompt::Point(10.0, 20.0));
        assert_eq!(prompt(pos2(10.0, 20.0), pos2(110.0, 5.0)), Prompt::Box(10.0, 20.0, 110.0, 5.0));
    }

    #[test]
    fn long_strokes_send_a_few_points_keeping_those_left_out() {
        let mut points: Vec<_> = (0..100).map(|i| (i as f32, 0.0, true)).collect();
        points[50].2 = false;
        let few = sparse(&points);
        assert!(few.len() <= MOST_POINTS + 2, "{}", few.len());
        assert!(few.contains(&(50.0, 0.0, false)));
        assert_eq!(few.last(), points.last());
        assert_eq!(sparse(&points[..5]), &points[..5]);
    }
}
