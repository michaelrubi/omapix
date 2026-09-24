//! Every user action, with its menu label and Photoshop shortcut. Menus and
//! the keyboard both go through this list, so they can't disagree.

use egui::{Key, KeyboardShortcut, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Open,
    Save,
    SaveAs,
    ExportTiff,
    ExportJpeg,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    CopyMerged,
    Paste,
    NewLayer,
    DuplicateLayer,
    DeleteLayer,
    MergeDown,
    StampVisible,
    NewGroup,
    GroupLayers,
    UngroupLayers,
    ClippingMask,
    LockTransparent,
    BringToFront,
    RaiseLayer,
    LowerLayer,
    SendToBack,
    SelectLayerAbove,
    SelectLayerBelow,
    BlendingOptions,
    AddMask,
    DeleteMask,
    ToggleMask,
    MaskOverlay,
    Invert,
    SelectAll,
    Deselect,
    InvertSelection,
    Feather,
    FillForeground,
    FillBackground,
    Clear,
    NewCurves,
    NewLevels,
    NewHueSaturation,
    NewColorBalance,
    NewSelectiveColor,
    NewChannelMixer,
    NewColorLookup,
    GaussianBlur,
    HighPass,
    FrequencySeparation,
    DodgeAndBurn,
    DodgeAndBurnCurves,
    HighPassSharpening,
    ZoomIn,
    ZoomOut,
    FitOnScreen,
    ActualPixels,
    ReopenLast,
    ShowLayers,
    ShowHistory,
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::COMMAND
};
const CMD_ALT: Modifiers = Modifiers {
    alt: true,
    ..Modifiers::COMMAND
};
const CMD_ALT_SHIFT: Modifiers = Modifiers {
    shift: true,
    alt: true,
    ..Modifiers::COMMAND
};

impl Command {
    /// Every command.
    pub const ALL: &[Command] = &[
        Command::Open,
        Command::Save,
        Command::SaveAs,
        Command::ExportTiff,
        Command::ExportJpeg,
        Command::Quit,
        Command::Undo,
        Command::Redo,
        Command::Cut,
        Command::Copy,
        Command::CopyMerged,
        Command::Paste,
        Command::NewLayer,
        Command::DuplicateLayer,
        Command::DeleteLayer,
        Command::MergeDown,
        Command::StampVisible,
        Command::NewGroup,
        Command::GroupLayers,
        Command::UngroupLayers,
        Command::ClippingMask,
        Command::LockTransparent,
        Command::BringToFront,
        Command::RaiseLayer,
        Command::LowerLayer,
        Command::SendToBack,
        Command::SelectLayerAbove,
        Command::SelectLayerBelow,
        Command::BlendingOptions,
        Command::AddMask,
        Command::DeleteMask,
        Command::ToggleMask,
        Command::MaskOverlay,
        Command::Invert,
        Command::SelectAll,
        Command::Deselect,
        Command::InvertSelection,
        Command::Feather,
        Command::FillForeground,
        Command::FillBackground,
        Command::Clear,
        Command::NewCurves,
        Command::NewLevels,
        Command::NewHueSaturation,
        Command::NewColorBalance,
        Command::NewSelectiveColor,
        Command::NewChannelMixer,
        Command::NewColorLookup,
        Command::GaussianBlur,
        Command::HighPass,
        Command::FrequencySeparation,
        Command::DodgeAndBurn,
        Command::DodgeAndBurnCurves,
        Command::HighPassSharpening,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::FitOnScreen,
        Command::ActualPixels,
        Command::ReopenLast,
        Command::ShowLayers,
        Command::ShowHistory,
    ];

    /// Look a command up by its name in code, e.g. "FrequencySeparation".
    pub fn from_name(name: &str) -> Option<Command> {
        if name == "NewLut" {
            return Some(Command::NewColorLookup);
        }
        Self::ALL.iter().copied().find(|c| format!("{c:?}") == name)
    }

    /// Every command with a shortcut, most modifiers first. egui matches
    /// Ctrl+Z even when Shift is also held, so Ctrl+Shift+Z has to be
    /// checked before it.
    pub const KEYBOARD_ORDER: &[Command] = &[
        Command::StampVisible,
        Command::InvertSelection,
        Command::UngroupLayers,
        Command::ClippingMask,
        Command::SaveAs,
        Command::Redo,
        Command::CopyMerged,
        Command::NewLayer,
        Command::BringToFront,
        Command::SendToBack,
        Command::ReopenLast,
        Command::Open,
        Command::Save,
        Command::Quit,
        Command::Undo,
        Command::Cut,
        Command::Copy,
        Command::Paste,
        Command::DuplicateLayer,
        Command::MergeDown,
        Command::GroupLayers,
        Command::RaiseLayer,
        Command::LowerLayer,
        Command::Invert,
        Command::SelectAll,
        Command::Deselect,
        Command::Feather,
        Command::SelectLayerAbove,
        Command::SelectLayerBelow,
        Command::FillForeground,
        Command::FillBackground,
        Command::Clear,
        Command::LockTransparent,
        Command::MaskOverlay,
        Command::ShowLayers,
        Command::NewCurves,
        Command::NewLevels,
        Command::NewHueSaturation,
        Command::NewColorBalance,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::FitOnScreen,
        Command::ActualPixels,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Command::Open => "Open…",
            Command::ReopenLast => "Reopen Last Document",
            Command::Save => "Save",
            Command::SaveAs => "Save As…",
            Command::ExportTiff => "Export as TIFF (16-bit)…",
            Command::ExportJpeg => "Export as JPEG (sRGB)…",
            Command::Quit => "Quit",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::Cut => "Cut",
            Command::Copy => "Copy",
            Command::CopyMerged => "Copy Merged",
            Command::Paste => "Paste",
            Command::NewLayer => "New Layer",
            Command::DuplicateLayer => "Duplicate Layer",
            Command::DeleteLayer => "Delete Layer",
            Command::MergeDown => "Merge Down",
            Command::StampVisible => "Stamp Visible",
            Command::NewGroup => "New Group",
            Command::GroupLayers => "Group Layers",
            Command::UngroupLayers => "Ungroup Layers",
            Command::ClippingMask => "Create Clipping Mask",
            Command::LockTransparent => "Lock Transparent Pixels",
            Command::BringToFront => "Bring to Front",
            Command::RaiseLayer => "Bring Forward",
            Command::LowerLayer => "Send Backward",
            Command::SendToBack => "Send to Back",
            Command::SelectLayerAbove => "Select Layer Above",
            Command::SelectLayerBelow => "Select Layer Below",
            Command::BlendingOptions => "Blending Options…",
            Command::AddMask => "Add Layer Mask",
            Command::DeleteMask => "Delete Layer Mask",
            Command::ToggleMask => "Disable/Enable Layer Mask",
            Command::MaskOverlay => "Mask Overlay",
            Command::Invert => "Invert",
            Command::SelectAll => "All",
            Command::Deselect => "Deselect",
            Command::InvertSelection => "Inverse",
            Command::Feather => "Feather…",
            Command::FillForeground => "Fill with Foreground",
            Command::FillBackground => "Fill with Background",
            Command::Clear => "Clear",
            Command::NewCurves => "Curves…",
            Command::NewLevels => "Levels…",
            Command::NewHueSaturation => "Hue/Saturation…",
            Command::NewColorBalance => "Color Balance…",
            Command::NewSelectiveColor => "Selective Color…",
            Command::NewChannelMixer => "Channel Mixer…",
            Command::NewColorLookup => "Color Lookup…",
            Command::GaussianBlur => "Gaussian Blur…",
            Command::HighPass => "High Pass…",
            Command::HighPassSharpening => "High Pass Sharpening…",
            Command::FrequencySeparation => "Frequency Separation…",
            Command::DodgeAndBurn => "Dodge & Burn Layer",
            Command::DodgeAndBurnCurves => "Dodge & Burn Curves",
            Command::ZoomIn => "Zoom In",
            Command::ZoomOut => "Zoom Out",
            Command::FitOnScreen => "Fit on Screen",
            Command::ActualPixels => "100%",
            Command::ShowLayers => "Layers",
            Command::ShowHistory => "History",
        }
    }

    pub fn shortcut(self) -> Option<KeyboardShortcut> {
        let s = |m, k| Some(KeyboardShortcut::new(m, k));
        match self {
            Command::Open => s(CMD, Key::O),
            Command::ReopenLast => s(CMD_SHIFT, Key::O),
            Command::Save => s(CMD, Key::S),
            Command::SaveAs => s(CMD_SHIFT, Key::S),
            Command::Quit => s(CMD, Key::Q),
            Command::Undo => s(CMD, Key::Z),
            Command::Redo => s(CMD_SHIFT, Key::Z),
            Command::Cut => s(CMD, Key::X),
            Command::Copy => s(CMD, Key::C),
            Command::CopyMerged => s(CMD_SHIFT, Key::C),
            Command::Paste => s(CMD, Key::V),
            Command::NewLayer => s(CMD_SHIFT, Key::N),
            Command::DuplicateLayer => s(CMD, Key::J),
            Command::MergeDown => s(CMD, Key::E),
            Command::StampVisible => s(CMD_ALT_SHIFT, Key::E),
            Command::GroupLayers => s(CMD, Key::G),
            Command::UngroupLayers => s(CMD_SHIFT, Key::G),
            Command::ClippingMask => s(CMD_ALT, Key::G),
            Command::LockTransparent => s(Modifiers::NONE, Key::Slash),
            Command::BringToFront => s(CMD_SHIFT, Key::CloseBracket),
            Command::RaiseLayer => s(CMD, Key::CloseBracket),
            Command::LowerLayer => s(CMD, Key::OpenBracket),
            Command::SendToBack => s(CMD_SHIFT, Key::OpenBracket),
            Command::SelectLayerAbove => s(Modifiers::ALT, Key::CloseBracket),
            Command::SelectLayerBelow => s(Modifiers::ALT, Key::OpenBracket),
            Command::Invert => s(CMD, Key::I),
            Command::SelectAll => s(CMD, Key::A),
            Command::Deselect => s(CMD, Key::D),
            Command::InvertSelection => s(CMD_SHIFT, Key::I),
            Command::Feather => s(Modifiers::SHIFT, Key::F6),
            Command::FillForeground => s(Modifiers::ALT, Key::Backspace),
            Command::FillBackground => s(CMD, Key::Backspace),
            Command::Clear => s(Modifiers::NONE, Key::Delete),
            Command::MaskOverlay => s(Modifiers::NONE, Key::Backslash),
            Command::ShowLayers => s(Modifiers::NONE, Key::F7),
            // Photoshop's shortcuts for these apply them destructively;
            // Omapix makes an adjustment layer instead.
            Command::NewCurves => s(CMD, Key::M),
            Command::NewLevels => s(CMD, Key::L),
            Command::NewHueSaturation => s(CMD, Key::U),
            Command::NewColorBalance => s(CMD, Key::B),
            Command::ZoomIn => s(CMD, Key::Equals),
            Command::ZoomOut => s(CMD, Key::Minus),
            Command::FitOnScreen => s(CMD, Key::Num0),
            Command::ActualPixels => s(CMD, Key::Num1),
            _ => None,
        }
    }

    /// Commands whose shortcut was pressed this frame, consuming the keys.
    /// `v_down` remembers between frames whether V is held.
    pub fn pressed(ctx: &egui::Context, v_down: &mut bool) -> Vec<Command> {
        ctx.input_mut(|i| {
            // With Shift held, egui reports [ and ] as the { and } they
            // type, so Ctrl+Shift+[ and Shift+[ (brush hardness, read
            // after this) would never match. Treat them as the brackets.
            for event in &mut i.events {
                if let egui::Event::Key {
                    key: key @ (Key::OpenCurlyBracket | Key::CloseCurlyBracket),
                    physical_key: Some(physical @ (Key::OpenBracket | Key::CloseBracket)),
                    ..
                } = event
                {
                    *key = *physical;
                }
            }
            let mut pressed: Vec<Command> = Self::KEYBOARD_ORDER
                .iter()
                .copied()
                .filter(|c| c.shortcut().is_some_and(|s| i.consume_shortcut(&s)))
                .collect();
            // Ctrl++ on keyboards where + is its own key.
            if i.consume_key(CMD, Key::Plus) {
                pressed.push(Command::ZoomIn);
            }
            // egui-winit turns Ctrl+C and Ctrl+X into Copy and Cut events
            // instead of key presses. Ctrl+V becomes a Paste event only when
            // the clipboard holds text, not an image, but its V is still
            // released: a V released without being pressed was Ctrl+V.
            for event in &i.events {
                match event {
                    egui::Event::Copy if i.modifiers.shift => pressed.push(Command::CopyMerged),
                    egui::Event::Copy => pressed.push(Command::Copy),
                    egui::Event::Cut => pressed.push(Command::Cut),
                    egui::Event::Key {
                        key: Key::V,
                        pressed: down,
                        ..
                    } => {
                        if !*down && !*v_down {
                            pressed.push(Command::Paste);
                        }
                        *v_down = *down;
                    }
                    _ => {}
                }
            }
            i.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)
                )
            });
            pressed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_is_in_keyboard_order() {
        for &c in Command::ALL {
            if c.shortcut().is_some() {
                assert!(
                    Command::KEYBOARD_ORDER.contains(&c),
                    "{c:?} missing from KEYBOARD_ORDER"
                );
            }
        }
    }

    #[test]
    fn names_round_trip() {
        for &c in Command::ALL {
            assert_eq!(Command::from_name(&format!("{c:?}")), Some(c));
        }
    }

    /// Commands pressed in a frame with these events and modifiers held.
    fn press(
        ctx: &egui::Context,
        v_down: &mut bool,
        modifiers: Modifiers,
        mut events: Vec<egui::Event>,
    ) -> Vec<Command> {
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut pressed = Vec::new();
        let mut output = ctx.run_ui(input, |ui| pressed = Command::pressed(ui.ctx(), v_down));
        // There's no renderer to upload textures to.
        output.textures_delta.clear();
        pressed
    }

    fn v(down: bool, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key: Key::V,
            physical_key: None,
            pressed: down,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn clipboard_shortcuts_arrive_as_egui_winit_sends_them() {
        let ctx = egui::Context::default();
        let mut v_down = false;
        let none = Modifiers::NONE;
        let copy = vec![egui::Event::Copy];
        assert_eq!(press(&ctx, &mut v_down, CMD, copy.clone()), [Command::Copy]);
        assert_eq!(
            press(&ctx, &mut v_down, CMD_SHIFT, copy),
            [Command::CopyMerged]
        );
        let cut = vec![egui::Event::Cut];
        assert_eq!(press(&ctx, &mut v_down, CMD, cut), [Command::Cut]);

        // Ctrl+V with an image on the clipboard: only the release arrives,
        // even if Ctrl was let go first.
        assert_eq!(press(&ctx, &mut v_down, CMD, vec![]), []);
        assert_eq!(
            press(&ctx, &mut v_down, none, vec![v(false, none)]),
            [Command::Paste]
        );
        // With text on the clipboard, a Paste event comes first.
        let paste = vec![egui::Event::Paste("text".into())];
        assert_eq!(press(&ctx, &mut v_down, CMD, paste), []);
        assert_eq!(
            press(&ctx, &mut v_down, CMD, vec![v(false, CMD)]),
            [Command::Paste]
        );
        // V on its own isn't pasting, however long it's held.
        assert_eq!(press(&ctx, &mut v_down, none, vec![v(true, none)]), []);
        assert_eq!(press(&ctx, &mut v_down, none, vec![v(true, none)]), []);
        assert_eq!(press(&ctx, &mut v_down, none, vec![v(false, none)]), []);
    }

    #[test]
    fn shifted_brackets_arrive_as_the_braces_they_type() {
        let ctx = egui::Context::default();
        let mut v_down = false;
        let brace = |key, physical_key| egui::Event::Key {
            key,
            physical_key: Some(physical_key),
            pressed: true,
            repeat: false,
            modifiers: CMD_SHIFT,
        };
        let front = vec![brace(Key::CloseCurlyBracket, Key::CloseBracket)];
        let back = vec![brace(Key::OpenCurlyBracket, Key::OpenBracket)];
        assert_eq!(
            press(&ctx, &mut v_down, CMD_SHIFT, front),
            [Command::BringToFront]
        );
        assert_eq!(
            press(&ctx, &mut v_down, CMD_SHIFT, back),
            [Command::SendToBack]
        );
    }

    #[test]
    fn more_specific_shortcuts_come_first() {
        let order = Command::KEYBOARD_ORDER;
        let pos = |c| order.iter().position(|&x| x == c).unwrap();
        assert!(pos(Command::Redo) < pos(Command::Undo));
        assert!(pos(Command::SaveAs) < pos(Command::Save));
        assert!(pos(Command::ReopenLast) < pos(Command::Open));
        assert!(pos(Command::StampVisible) < pos(Command::MergeDown));
        assert!(pos(Command::InvertSelection) < pos(Command::Invert));
        assert!(pos(Command::CopyMerged) < pos(Command::Copy));
        assert!(pos(Command::UngroupLayers) < pos(Command::GroupLayers));
        assert!(pos(Command::ClippingMask) < pos(Command::GroupLayers));
        assert!(pos(Command::BringToFront) < pos(Command::RaiseLayer));
        assert!(pos(Command::SendToBack) < pos(Command::LowerLayer));
    }
}
