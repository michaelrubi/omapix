//! Files dropped on the window.
//!
//! winit 0.30 doesn't implement drag and drop on Wayland, so drops are taken
//! by egui's Wayland clipboard (patched to, see `vendor/smithay-clipboard`)
//! and handed to egui here as winit would hand them.

use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

static DROPPED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A dropped file, as egui takes it.
#[derive(Debug)]
pub struct DroppedPath(pub PathBuf);

impl egui::DroppedFile for DroppedPath {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Starts taking drops, repainting `ctx` when one arrives.
pub fn listen(ctx: egui::Context) {
    smithay_clipboard::on_drop(move |uri_list| {
        DROPPED.lock().unwrap().extend(paths(&uri_list));
        ctx.request_repaint();
    });
}

/// Adds the files dropped since the last frame to `raw`.
pub fn take(raw: &mut egui::RawInput) {
    let dropped = std::mem::take(&mut *DROPPED.lock().unwrap());
    raw.dropped_files
        .extend(dropped.into_iter().map(|p| Arc::new(DroppedPath(p)) as egui::DroppedFileHandle));
}

/// The local files in a `text/uri-list`.
fn paths(uri_list: &str) -> Vec<PathBuf> {
    uri_list
        .lines()
        .filter_map(|line| line.trim().strip_prefix("file://"))
        // Skip a host name, as in file://localhost/path.
        .filter_map(|rest| rest.find('/').map(|i| &rest[i..]))
        .map(|path| PathBuf::from(std::ffi::OsString::from_vec(percent_decode(path))))
        .collect()
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match hex.filter(|_| bytes[i] == b'%').and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_list_gives_the_local_files() {
        let list = "# from Nautilus\r\nfile:///home/me/My%20Photos/caf%C3%A9.jpg\r\n\
                    file://localhost/tmp/a.tif\r\nhttps://example.com/b.png\r\nfile:///100%.png\r\n";
        assert_eq!(
            paths(list),
            [
                PathBuf::from("/home/me/My Photos/café.jpg"),
                PathBuf::from("/tmp/a.tif"),
                PathBuf::from("/100%.png"),
            ]
        );
    }
}
