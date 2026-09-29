//! Photoshop's 100 % preview box in a filter's dialog: a part of the image,
//! dragged to look around, showing the result, and the image as it was
//! while the button is held on it.

use egui::{Pos2, Rect, Response, Sense, pos2, vec2};
use omapix_engine::Pixel;

use crate::editor::Editor;

/// The box's side, as Photoshop's.
pub const SIDE: u32 = 240;

pub struct PreviewBox {
    rect: Rect,
    response: Response,
    /// The part of the image it shows: its top left corner, and its size.
    pub corner: (u32, u32),
    pub size: (u32, u32),
}

impl PreviewBox {
    /// Lay out a box on an image `width` × `height` round `centre`, moving
    /// `centre` as it's dragged, and keeping the box on the image.
    pub fn new(ui: &mut egui::Ui, centre: &mut Pos2, (width, height): (u32, u32)) -> Self {
        let (w, h) = (SIDE.min(width), SIDE.min(height));
        let (rect, response) = ui
            .vertical_centered(|ui| ui.allocate_exact_size(vec2(w as f32, h as f32), Sense::drag()))
            .inner;
        if response.dragged() {
            *centre -= response.drag_delta();
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        let at = |c: f32, side: u32, size: u32| (c - side as f32 / 2.0).clamp(0.0, (size - side) as f32).round() as u32;
        let corner = (at(centre.x, w, width), at(centre.y, h, height));
        *centre = pos2(corner.0 as f32 + w as f32 / 2.0, corner.1 as f32 + h as f32 / 2.0);
        Self {
            rect,
            response,
            corner,
            size: (w, h),
        }
    }

    /// Whether it's being dragged.
    pub fn dragged(&self) -> bool {
        self.response.dragged()
    }

    /// `pixels` (the box's size, in the document's colours) as a texture
    /// to show in it.
    pub fn texture(&self, ctx: &egui::Context, editor: &Editor, name: &str, pixels: &[Pixel]) -> egui::TextureHandle {
        let mut rgba = vec![[0u8; 4]; pixels.len()];
        editor.canvas.transform().convert(pixels, &mut rgba);
        let (w, h) = self.size;
        let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], bytemuck::cast_slice(&rgba));
        ctx.load_texture(name, image, egui::TextureOptions::NEAREST)
    }

    /// Show `after`, or `before` while the button is held on the box.
    pub fn show(&self, ui: &mut egui::Ui, after: &egui::TextureHandle, before: &egui::TextureHandle) {
        let shown = if self.response.is_pointer_button_down_on() { before } else { after };
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter().image(shown.id(), self.rect, uv, egui::Color32::WHITE);
        self.frame(ui);
    }

    /// Just its outline, while there's nothing to show yet.
    pub fn frame(&self, ui: &mut egui::Ui) {
        let stroke = egui::Stroke::new(1.0, ui.visuals().window_stroke().color);
        ui.painter().rect_stroke(self.rect, 0.0, stroke, egui::StrokeKind::Outside);
    }
}
