//! The Object Selection tool's model (SAM 2.1, see omapix-ai), on a thread
//! of its own: loaded on first use, the image encoded once per change of
//! the document, then each click or box answered in about a tenth of a
//! second.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use egui::Pos2;
use omapix_ai::sam::{Encoded, MASK_SIZE, Prompt, Sam};
use omapix_engine::selection::Combine;
use omapix_engine::tiled::Tiled;
use omapix_engine::{ColorProfile, DisplayTransform, refine};

use crate::canvas::Render;
use crate::editor::Editor;

/// A drag shorter than this (image pixels) is a click.
const CLICK: f32 = 3.0;

/// What a click or drag from `a` to `b` asks for: the object under a click,
/// or in the box dragged.
pub fn prompt(a: Pos2, b: Pos2) -> Prompt {
    if a.distance(b) < CLICK {
        Prompt::Point(a.x, a.y)
    } else {
        Prompt::Box(a.x, a.y, b.x, b.y)
    }
}

struct Request {
    /// To wake the UI when the answer's ready.
    ctx: egui::Context,
    /// The flattened image, and which revision of the document it shows.
    image: Arc<Render>,
    revision: u64,
    profile: ColorProfile,
    prompt: Prompt,
    how: Combine,
}

/// An object found: its coverage, to combine with the selection `how`.
pub struct Found {
    pub coverage: Tiled<u16>,
    pub how: Combine,
}

/// The worker thread: where to send requests, and where answers come back.
type Worker = (Sender<Request>, Receiver<Result<Found, String>>);

#[derive(Default)]
pub struct ObjectSelection {
    worker: Option<Worker>,
    /// Asked for, and not yet sent to the worker.
    asked: Vec<(Prompt, Combine)>,
    /// Sent and not yet answered.
    waiting: usize,
}

impl ObjectSelection {
    /// Find the object `prompt` points to, to combine with the selection
    /// `how`. Sent on the next [`Self::poll`].
    pub fn ask(&mut self, prompt: Prompt, how: Combine) {
        self.asked.push((prompt, how));
    }

    /// Send what's been asked about `editor`'s image, and return an answer
    /// once there is one.
    pub fn poll(&mut self, ctx: &egui::Context, editor: &Editor) -> Option<Result<Found, String>> {
        if let Some(image) = editor.canvas.render().filter(|_| !self.asked.is_empty()) {
            let (tx, _) = self.worker.get_or_insert_with(spawn);
            for (prompt, how) in self.asked.drain(..) {
                let request = Request {
                    ctx: ctx.clone(),
                    image: Arc::clone(image),
                    revision: editor.revision(),
                    profile: editor.doc.profile.clone(),
                    prompt,
                    how,
                };
                if tx.send(request).is_ok() {
                    self.waiting += 1;
                }
            }
        }
        let (_, rx) = self.worker.as_ref()?;
        match rx.try_recv() {
            Ok(found) => {
                self.waiting = self.waiting.saturating_sub(1);
                Some(found)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.worker = None;
                self.waiting = 0;
                None
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.waiting > 0 || !self.asked.is_empty()
    }
}

fn spawn() -> Worker {
    let (tx, requests) = channel::<Request>();
    let (answers, rx) = channel();
    std::thread::spawn(move || {
        let mut sam: Option<Sam> = None;
        let mut encoded: Option<(u64, Encoded)> = None;
        for request in requests {
            let found = find(&mut sam, &mut encoded, &request).map(|coverage| Found { coverage, how: request.how });
            if answers.send(found).is_err() {
                break;
            }
            request.ctx.request_repaint();
        }
        // Dropping CUDA sessions can crash Omapix as it quits (docs/AI.md),
        // so the model stays loaded until then.
        std::mem::forget(sam);
    });
    (tx, rx)
}

fn find(sam: &mut Option<Sam>, encoded: &mut Option<(u64, Encoded)>, request: &Request) -> Result<Tiled<u16>, String> {
    let sam = match sam {
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
    }
    let (_, image) = encoded.as_ref().expect("just encoded");
    let logits = sam.select(image, request.prompt)?;
    request
        .image
        .with_image(|image| refine::mask_coverage(&logits, MASK_SIZE, MASK_SIZE, image))
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
}
