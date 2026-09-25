//! The Navigator panel: whole-image thumbnail, viewport outline, and zoom controls.

use std::time::{Duration, Instant};

use egui::{Color32, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::canvas::ZOOM_STEPS;
use crate::editor::Editor;
use crate::theme::Theme;

pub struct NavigatorPanel {
    texture: Option<egui::TextureHandle>,
    cached_revision: u64,
    last_update: Instant,
    zoom_input: Option<String>,
}

impl Default for NavigatorPanel {
    fn default() -> Self {
        Self {
            texture: None,
            cached_revision: 0,
            last_update: Instant::now() - Duration::from_secs(1),
            zoom_input: None,
        }
    }
}

impl NavigatorPanel {
    pub fn show(&mut self, ui: &mut Ui, editor: &mut Editor, theme: &Theme) {
        // Update thumbnail from the smallest pyramid level if document changed (throttled).
        let rev = editor.revision();
        let needs_update = self.texture.is_none() || self.cached_revision != rev;
        let throttled = self.last_update.elapsed() < Duration::from_millis(250);
        if needs_update
            && (!throttled || self.texture.is_none())
            && let Some(render) = editor.canvas.render()
        {
            // As many pixels as the thumbnail shows on screen, so it's sharp.
            let side = (ui.available_width().min(280.0) * ui.ctx().pixels_per_point()) as u32;
            let raster = render.level_at_least(side);
            let (w, h) = (raster.width() as usize, raster.height() as usize);
            if w > 0 && h > 0 {
                let samples = raster.pixels();
                let mut out = vec![[0u8; 4]; samples.len()];
                editor.canvas.transform().convert(samples, &mut out);
                let rgba: Vec<u8> = out.into_iter().flatten().collect();
                let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                match &mut self.texture {
                    Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.texture = Some(ui.ctx().load_texture(
                            "navigator-thumb",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
                self.cached_revision = rev;
                self.last_update = Instant::now();
            }
        }

        let max_w = ui.available_width().min(280.0);
        let max_h = 140.0;
        let img_w = editor.doc.width as f32;
        let img_h = editor.doc.height as f32;
        let aspect = (img_w / img_h.max(1.0)).max(0.01);

        let (fit_w, fit_h) = if aspect >= (max_w / max_h) {
            (max_w, (max_w / aspect).max(1.0))
        } else {
            ((max_h * aspect).max(1.0), max_h)
        };

        let (rect, response) = ui.allocate_exact_size(vec2(fit_w, fit_h), Sense::click_and_drag());
        let painter = ui.painter_at(rect.expand(2.0));
        painter.rect_filled(rect, 0.0, theme.darker_background);

        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, theme.muted), egui::StrokeKind::Outside);

        // Red rectangle showing visible area on screen
        if let Some((_level, (x0, y0, x1, y1))) = editor.canvas.visible_area() {
            let rx0 = rect.left() + (x0 as f32 / img_w) * rect.width();
            let ry0 = rect.top() + (y0 as f32 / img_h) * rect.height();
            let rx1 = rect.left() + (x1 as f32 / img_w) * rect.width();
            let ry1 = rect.top() + (y1 as f32 / img_h) * rect.height();
            let red_rect = Rect::from_min_max(pos2(rx0, ry0), pos2(rx1, ry1));
            painter.rect_stroke(red_rect, 0.0, Stroke::new(1.5, Color32::RED), egui::StrokeKind::Middle);
        }

        // Clicking or dragging in the thumbnail centres the view on that point
        if (response.clicked() || response.dragged())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let norm_x = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let norm_y = ((pointer.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            let img_p = pos2(norm_x * img_w, norm_y * img_h);
            editor.canvas.center_on(img_p);
        }

        // Zoom controls
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let current_zoom = editor.canvas.zoom();
            let zoom_label = editor.canvas.zoom_label();
            let text = self.zoom_input.get_or_insert_with(|| zoom_label.clone());
            if !ui.memory(|m| m.has_focus(ui.id().with("zoom_text"))) {
                *text = zoom_label;
            }
            let edit = ui.add(
                egui::TextEdit::singleline(text)
                    .id(ui.id().with("zoom_text"))
                    .desired_width(52.0),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let clean = text.trim().trim_end_matches('%');
                if let Ok(val) = clean.parse::<f32>()
                    && val > 0.0
                {
                    editor.canvas.set_zoom(val / 100.0);
                }
            }

            if ui.small_button("−").on_hover_text("Zoom Out").clicked() {
                editor.canvas.step_zoom(false);
            }

            let mut slider_val = zoom_to_slider(current_zoom);
            let prev_slider = slider_val;
            let slider = egui::Slider::new(&mut slider_val, 0.0..=(ZOOM_STEPS.len() - 1) as f64)
                .show_value(false);
            if ui.add(slider).changed() && (slider_val - prev_slider).abs() > 0.001 {
                let new_zoom = slider_to_zoom(slider_val);
                editor.canvas.set_zoom(new_zoom);
            }

            if ui.small_button("+").on_hover_text("Zoom In").clicked() {
                editor.canvas.step_zoom(true);
            }
        });

        ui.horizontal(|ui| {
            if ui.small_button("Fit").on_hover_text("Fit on Screen").clicked() {
                editor.canvas.fit();
            }
            if ui.small_button("100%").on_hover_text("Actual Pixels").clicked() {
                editor.canvas.actual_pixels();
            }
        });
    }
}

fn zoom_to_slider(z: f32) -> f64 {
    if z <= ZOOM_STEPS[0] {
        return 0.0;
    }
    let last = ZOOM_STEPS.len() - 1;
    if z >= ZOOM_STEPS[last] {
        return last as f64;
    }
    for i in 0..last {
        let z0 = ZOOM_STEPS[i];
        let z1 = ZOOM_STEPS[i + 1];
        if (z0..=z1).contains(&z) {
            let t = (z - z0) / (z1 - z0);
            return i as f64 + t as f64;
        }
    }
    0.0
}

fn slider_to_zoom(s: f64) -> f32 {
    let s = s.clamp(0.0, (ZOOM_STEPS.len() - 1) as f64);
    let i = (s.floor() as usize).min(ZOOM_STEPS.len() - 2);
    let t = (s - i as f64) as f32;
    ZOOM_STEPS[i] + t * (ZOOM_STEPS[i + 1] - ZOOM_STEPS[i])
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, pos2, vec2};
    use omapix_engine::{ColorProfile, Document, Raster};

    struct Harness {
        ctx: egui::Context,
        panel: NavigatorPanel,
        editor: Editor,
        theme: Theme,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let (w, h) = (400, 400);
            let image = Raster::new(w, h, vec![[30000, 30000, 30000, 65535]; (w * h) as usize]);
            let doc = Document::from_image("test.tif".into(), &image, ColorProfile::srgb(), 16);
            let mut editor = Editor::new(doc).unwrap();
            editor.canvas.lay_out_for_test(
                Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0)),
                2.0,
            );
            Self {
                ctx: egui::Context::default(),
                panel: NavigatorPanel::default(),
                editor,
                theme: Theme::default(),
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.time += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 600.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (panel, editor, theme) = (&mut self.panel, &mut self.editor, &self.theme);
            let mut output = self.ctx.run_ui(input, |ui| panel.show(ui, editor, theme));
            output.textures_delta.clear();
        }
    }

    #[test]
    fn dragging_in_navigator_moves_canvas_view() {
        let mut h = Harness::new();
        // Layout initial frame
        h.frame(vec![]);

        h.editor.canvas.center_on(pos2(50.0, 50.0));
        let initial_area = h.editor.canvas.visible_area();
        assert!(initial_area.is_some());

        // Drag inside navigator thumbnail to point (200, 100)
        let click_pt = pos2(120.0, 80.0);
        h.frame(vec![
            Event::PointerMoved(click_pt),
            Event::PointerButton {
                pos: click_pt,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ]);

        let dragged_pt = pos2(200.0, 120.0);
        h.frame(vec![
            Event::PointerMoved(dragged_pt),
        ]);

        h.frame(vec![
            Event::PointerMoved(dragged_pt),
            Event::PointerButton {
                pos: dragged_pt,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]);

        let new_area = h.editor.canvas.visible_area();
        assert_ne!(initial_area, new_area, "dragging in navigator moves visible area");
    }
}
