//! Pen tablets on Wayland.
//!
//! winit 0.30 has no tablet input, so Omapix reads the Wayland tablet
//! protocol itself, on a thread of its own sharing winit's connection (as
//! the patched clipboard does). Once a client takes tablet events the
//! compositor no longer moves the mouse pointer with the pen, so the pen is
//! handed to egui here as pointer events, and its pressure kept for the
//! brushes. Its cursor has to be set here too.

use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};

use egui::{CursorIcon, PointerButton, pos2};
use wayland_client::backend::Backend;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, event_created_child};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{self, Shape};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1;
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2, zwp_tablet_pad_group_v2, zwp_tablet_pad_ring_v2, zwp_tablet_pad_strip_v2,
    zwp_tablet_pad_v2, zwp_tablet_seat_v2, zwp_tablet_tool_v2, zwp_tablet_v2,
};

use zwp_tablet_tool_v2::ZwpTabletToolV2;

/// The pen's side buttons, as Linux numbers them.
const BTN_STYLUS: u32 = 0x14b;
const BTN_STYLUS2: u32 = 0x14c;

/// What the pen did, in surface coordinates (logical pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pen {
    Moved(f32, f32),
    Button(PointerButton, bool),
    Gone,
}

#[derive(Default)]
struct Shared {
    events: Vec<Pen>,
    /// The tip's pressure (0–1), while the pen's near the tablet.
    pressure: Option<f32>,
    /// The tool near the tablet, and its `proximity_in` serial, to set its
    /// cursor with.
    near: Option<(ZwpTabletToolV2, u32)>,
    /// The cursor egui last asked for, and whether it's been shown for the
    /// pen since it came near.
    cursor: CursorIcon,
    cursor_shown: bool,
}

pub struct Tablet {
    shared: Arc<Mutex<Shared>>,
    /// Never dropped: winit closes the display as Omapix quits, and
    /// destroying these after that crashes it.
    wayland: ManuallyDrop<Wayland>,
    /// Where the pen was last, in egui points.
    last: egui::Pos2,
}

struct Wayland {
    connection: Connection,
    shapes: Option<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>,
    queue: QueueHandle<State>,
}

impl Tablet {
    /// Starts taking tablet input on Wayland, repainting when the pen moves.
    /// `None` elsewhere, or if the compositor has no tablet support.
    pub fn connect(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
        let RawDisplayHandle::Wayland(display) = cc.display_handle().ok()?.as_raw() else {
            return None;
        };
        // Safety: winit keeps the display open for as long as Omapix runs.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&connection).ok()?;
        let qh = queue.handle();
        let manager: zwp_tablet_manager_v2::ZwpTabletManagerV2 = globals.bind(&qh, 1..=1, ()).ok()?;
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).ok()?;
        manager.get_tablet_seat(&seat, &qh, ());
        let shapes = globals.bind(&qh, 1..=1, ()).ok();

        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut state = State {
            shared: Arc::clone(&shared),
            ctx: cc.egui_ctx.clone(),
            frame: Vec::new(),
        };
        std::thread::Builder::new()
            .name("tablet".into())
            .spawn(move || {
                while queue.blocking_dispatch(&mut state).is_ok() {}
                // The display's closed (Omapix is quitting): destroying the
                // queue and the objects on it now would crash.
                std::mem::forget((queue, state));
            })
            .ok()?;
        Some(Self {
            shared,
            wayland: ManuallyDrop::new(Wayland {
                connection,
                shapes,
                queue: qh,
            }),
            last: egui::Pos2::ZERO,
        })
    }

    /// Hands what the pen did since the last frame to egui, with the keys
    /// held.
    pub fn take(&mut self, raw: &mut egui::RawInput, zoom: f32, modifiers: egui::Modifiers) {
        let pens = std::mem::take(&mut self.shared.lock().unwrap().events);
        raw.events.extend(pens.into_iter().map(|pen| event(pen, &mut self.last, zoom, modifiers)));
    }

    /// The pen's pressure, 0–1, or 1 when painting with a mouse.
    pub fn pressure(&self) -> f32 {
        self.shared.lock().unwrap().pressure.unwrap_or(1.0)
    }

    /// Shows `icon` under the pen, as winit does under the mouse.
    pub fn set_cursor(&self, icon: CursorIcon) {
        let mut shared = self.shared.lock().unwrap();
        if shared.cursor == icon && shared.cursor_shown {
            return;
        }
        shared.cursor = icon;
        let Some((tool, serial)) = &shared.near else {
            return;
        };
        match shape(icon) {
            None => tool.set_cursor(*serial, None, 0, 0),
            Some(shape) => {
                if let Some(shapes) = &self.wayland.shapes {
                    let device = shapes.get_tablet_tool_v2(tool, &self.wayland.queue, ());
                    device.set_shape(*serial, shape);
                    device.destroy();
                }
            }
        }
        shared.cursor_shown = true;
        let _ = self.wayland.connection.flush();
    }
}

/// What the pen did as egui's event, with `last` where it was (egui
/// points, `zoom` logical pixels each).
fn event(pen: Pen, last: &mut egui::Pos2, zoom: f32, modifiers: egui::Modifiers) -> egui::Event {
    match pen {
        Pen::Moved(x, y) => {
            *last = pos2(x / zoom, y / zoom);
            egui::Event::PointerMoved(*last)
        }
        Pen::Button(button, pressed) => egui::Event::PointerButton {
            pos: *last,
            button,
            pressed,
            modifiers,
        },
        Pen::Gone => egui::Event::PointerGone,
    }
}

/// The system cursor for egui's `icon`; `None` hides it.
fn shape(icon: CursorIcon) -> Option<Shape> {
    Some(match icon {
        CursorIcon::None => return None,
        CursorIcon::Crosshair => Shape::Crosshair,
        CursorIcon::PointingHand => Shape::Pointer,
        CursorIcon::Text => Shape::Text,
        CursorIcon::Grab => Shape::Grab,
        CursorIcon::Grabbing => Shape::Grabbing,
        CursorIcon::Move => Shape::Move,
        CursorIcon::AllScroll => Shape::AllScroll,
        CursorIcon::NotAllowed | CursorIcon::NoDrop => Shape::NotAllowed,
        CursorIcon::Wait => Shape::Wait,
        CursorIcon::Progress => Shape::Progress,
        CursorIcon::Help => Shape::Help,
        CursorIcon::ResizeHorizontal | CursorIcon::ResizeColumn => Shape::EwResize,
        CursorIcon::ResizeVertical | CursorIcon::ResizeRow => Shape::NsResize,
        CursorIcon::ResizeNeSw => Shape::NeswResize,
        CursorIcon::ResizeNwSe => Shape::NwseResize,
        _ => Shape::Default,
    })
}

/// The tablet thread's state.
struct State {
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
    /// What the pen's done since its last `frame` event.
    frame: Vec<Pen>,
}

impl Dispatch<ZwpTabletToolV2, ()> for State {
    fn event(
        state: &mut Self,
        tool: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_tablet_tool_v2::Event;
        let mut shared = state.shared.lock().unwrap();
        match event {
            Event::ProximityIn { serial, .. } => {
                shared.near = Some((tool.clone(), serial));
                shared.cursor_shown = false;
                shared.pressure = Some(0.0);
            }
            Event::ProximityOut => {
                shared.near = None;
                shared.pressure = None;
                state.frame.push(Pen::Gone);
            }
            Event::Motion { x, y } => state.frame.push(Pen::Moved(x as f32, y as f32)),
            Event::Pressure { pressure } => {
                shared.pressure = Some(pressure as f32 / 65535.0);
            }
            Event::Down { .. } => state.frame.push(Pen::Button(PointerButton::Primary, true)),
            Event::Up => state.frame.push(Pen::Button(PointerButton::Primary, false)),
            Event::Button {
                button,
                state: pressed,
                ..
            } => {
                let pressed = pressed == WEnum::Value(zwp_tablet_tool_v2::ButtonState::Pressed);
                match button {
                    BTN_STYLUS => state.frame.push(Pen::Button(PointerButton::Secondary, pressed)),
                    BTN_STYLUS2 => state.frame.push(Pen::Button(PointerButton::Middle, pressed)),
                    _ => {}
                }
            }
            Event::Frame { .. } => {
                shared.events.append(&mut state.frame);
                state.ctx.request_repaint();
            }
            Event::Removed => tool.destroy(),
            _ => {}
        }
    }
}

impl Dispatch<zwp_tablet_seat_v2::ZwpTabletSeatV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwp_tablet_seat_v2::ZwpTabletSeatV2,
        _: zwp_tablet_seat_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, zwp_tablet_seat_v2::ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (zwp_tablet_v2::ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (zwp_tablet_pad_v2::ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<zwp_tablet_pad_v2::ZwpTabletPadV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwp_tablet_pad_v2::ZwpTabletPadV2,
        _: zwp_tablet_pad_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, zwp_tablet_pad_v2::ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2,
        _: zwp_tablet_pad_group_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2, ()),
    ]);
}

/// Objects whose events Omapix doesn't need: pad buttons are best bound to
/// keys outside Omapix (docs/ROADMAP.md, "Tablet support").
macro_rules! ignore {
    ($($interface:ty),*) => {$(
        impl Dispatch<$interface, ()> for State {
            fn event(
                _: &mut Self,
                _: &$interface,
                _: <$interface as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore!(
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_v2::ZwpTabletV2,
    zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
    zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
    wl_seat::WlSeat,
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
    wp_cursor_shape_device_v1::WpCursorShapeDeviceV1
);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pen_moves_and_clicks_where_it_is_in_egui_points() {
        let mut last = egui::Pos2::ZERO;
        let alt = egui::Modifiers::ALT;
        assert_eq!(event(Pen::Moved(300.0, 150.0), &mut last, 1.5, alt), egui::Event::PointerMoved(pos2(200.0, 100.0)));
        assert_eq!(
            event(Pen::Button(PointerButton::Primary, true), &mut last, 1.5, alt),
            egui::Event::PointerButton {
                pos: pos2(200.0, 100.0),
                button: PointerButton::Primary,
                pressed: true,
                modifiers: alt,
            }
        );
        assert_eq!(event(Pen::Gone, &mut last, 1.5, alt), egui::Event::PointerGone);
    }

    #[test]
    fn a_hidden_cursor_hides_the_pens_too() {
        assert_eq!(shape(CursorIcon::None), None);
        assert_eq!(shape(CursorIcon::Crosshair), Some(Shape::Crosshair));
        assert_eq!(shape(CursorIcon::ContextMenu), Some(Shape::Default));
    }
}
