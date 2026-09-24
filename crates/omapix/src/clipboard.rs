//! The clipboard: the last cut or copy at full 16-bit quality, shared with
//! other apps as an sRGB PNG on the Wayland clipboard through wl-clipboard
//! (`wl-copy` and `wl-paste`).

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use omapix_engine::clip::Clip;

/// Image types other apps offer that Omapix can read, best first.
const IMAGE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/jpg"];

pub struct Clipboard {
    /// The last cut or copy made in Omapix.
    clip: Option<Arc<Clip>>,
    system: Arc<Mutex<System>>,
}

/// Omapix's hold on the system clipboard.
struct System {
    /// Whether to use it at all; off in tests, and once wl-copy turns out
    /// to be missing.
    enabled: bool,
    /// Bumped on every copy, and set in `published` once that copy is on
    /// the system clipboard, so a slow older copy can't overwrite a newer.
    generation: u64,
    published: u64,
    /// `wl-copy --foreground` serving the last copy. It exits once another
    /// app copies something.
    server: Option<Child>,
}

impl Clipboard {
    /// A clipboard that is shared with other apps if `system`.
    pub fn new(system: bool) -> Self {
        Self {
            clip: None,
            system: Arc::new(Mutex::new(System {
                enabled: system,
                generation: 0,
                published: 0,
                server: None,
            })),
        }
    }

    /// Put `clip` on the clipboard. Other apps get it once it's converted
    /// to PNG in the background.
    pub fn set(&mut self, clip: Clip) {
        let clip = Arc::new(clip);
        self.clip = Some(Arc::clone(&clip));
        let generation = {
            let mut system = self.system.lock().unwrap();
            if !system.enabled {
                return;
            }
            system.generation += 1;
            system.generation
        };
        let system = Arc::clone(&self.system);
        std::thread::spawn(move || {
            let png = match clip.to_png() {
                Ok(png) => png,
                Err(e) => return log::warn!("can't convert the copy for other apps: {e}"),
            };
            let mut system = system.lock().unwrap();
            if system.generation != generation {
                return;
            }
            system.published = generation;
            match serve(&png) {
                Ok(child) => {
                    if let Some(mut old) = system.server.replace(child) {
                        let _ = old.kill();
                        let _ = old.wait();
                    }
                }
                Err(e) => {
                    log::warn!("can't copy to the system clipboard: {e}");
                    if e.kind() == std::io::ErrorKind::NotFound {
                        system.enabled = false;
                    }
                }
            }
        });
    }

    /// The last copy made in Omapix, if the system clipboard still holds it
    /// (or is not in use). `None` means paste from the system clipboard.
    pub fn current(&self) -> Option<Arc<Clip>> {
        let clip = self.clip.as_ref()?;
        let mut system = self.system.lock().unwrap();
        let ours = !system.enabled
            || system.published != system.generation
            || system
                .server
                .as_mut()
                .is_some_and(|s| matches!(s.try_wait(), Ok(None)));
        ours.then(|| Arc::clone(clip))
    }
}

/// Offer `png` on the Wayland clipboard until another app copies.
fn serve(png: &[u8]) -> std::io::Result<Child> {
    let mut child = Command::new("wl-copy")
        .args(["--foreground", "--type", "image/png"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Dropping stdin closes it, so wl-copy knows it has everything.
    child.stdin.take().expect("piped").write_all(png)?;
    Ok(child)
}

/// Read an image another app copied. Blocks, so call off the UI thread.
pub fn read_system() -> Result<Clip, String> {
    let nothing = "Nothing to paste: the clipboard has no image".to_owned();
    let types = Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .map_err(|e| format!("Can't read the clipboard: {e}"))?;
    let types = String::from_utf8_lossy(&types.stdout);
    let Some(kind) = IMAGE_TYPES
        .into_iter()
        .find(|t| types.lines().any(|l| l.trim() == *t))
    else {
        return Err(nothing);
    };
    let data = Command::new("wl-paste")
        .args(["--no-newline", "--type", kind])
        .output()
        .map_err(|e| format!("Can't read the clipboard: {e}"))?;
    if !data.status.success() || data.stdout.is_empty() {
        return Err(nothing);
    }
    Clip::from_image(&data.stdout).map_err(|e| format!("Can't paste the clipboard's image: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::ColorProfile;
    use omapix_engine::tiled::Tiled;

    #[test]
    fn without_the_system_clipboard_the_last_copy_is_pasted() {
        let mut clipboard = Clipboard::new(false);
        assert!(clipboard.current().is_none());
        let pixels = Tiled::new(10, 10, [0; 4]);
        let clip = Clip::copy(&pixels, None, &ColorProfile::srgb()).unwrap();
        clipboard.set(clip);
        assert!(clipboard.current().is_some());
    }

    /// Goes through the real Wayland clipboard, replacing what's on it.
    /// Run with `cargo test -- --ignored system_clipboard`.
    #[test]
    #[ignore]
    fn system_clipboard_round_trip() {
        use omapix_engine::selection::Selection;
        let wait = || std::thread::sleep(std::time::Duration::from_millis(500));
        let mut clipboard = Clipboard::new(true);
        let pixels = Tiled::new(300, 200, [20000, 40000, 60000, 65535]);
        let pixels = Tiled::from_raster(&pixels.to_raster());
        let sel = Selection::rectangle(300, 200, (10.0, 10.0), (110.0, 60.0));
        let clip = Clip::copy(&pixels, Some(&sel), &ColorProfile::srgb()).unwrap();
        clipboard.set(clip);
        wait();
        // Other apps see a PNG of just the selection.
        let theirs = read_system().unwrap();
        assert_eq!(theirs.bounds, [0, 0, 100, 50]);
        assert!(!theirs.in_place);
        // Omapix pastes its own copy while the clipboard holds it...
        assert!(clipboard.current().is_some_and(|c| c.in_place));
        // ...and whatever another app copies after that.
        let text = Command::new("wl-copy")
            .arg("text")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        assert!(text.unwrap().success());
        wait();
        assert!(clipboard.current().is_none());
        assert!(read_system().is_err_and(|e| e.contains("no image")));
    }
}
