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
    NewLayer,
    DuplicateLayer,
    DeleteLayer,
    MergeDown,
    StampVisible,
    RaiseLayer,
    LowerLayer,
    AddMask,
    DeleteMask,
    ToggleMask,
    Invert,
    NewCurves,
    NewLevels,
    NewHueSaturation,
    NewColorBalance,
    GaussianBlur,
    FrequencySeparation,
    DodgeAndBurn,
    ZoomIn,
    ZoomOut,
    FitOnScreen,
    ActualPixels,
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers {
    shift: true,
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
        Command::NewLayer,
        Command::DuplicateLayer,
        Command::DeleteLayer,
        Command::MergeDown,
        Command::StampVisible,
        Command::RaiseLayer,
        Command::LowerLayer,
        Command::AddMask,
        Command::DeleteMask,
        Command::ToggleMask,
        Command::Invert,
        Command::NewCurves,
        Command::NewLevels,
        Command::NewHueSaturation,
        Command::NewColorBalance,
        Command::GaussianBlur,
        Command::FrequencySeparation,
        Command::DodgeAndBurn,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::FitOnScreen,
        Command::ActualPixels,
    ];

    /// Look a command up by its name in code, e.g. "FrequencySeparation".
    pub fn from_name(name: &str) -> Option<Command> {
        Self::ALL.iter().copied().find(|c| format!("{c:?}") == name)
    }

    /// Every command with a shortcut, most modifiers first. egui matches
    /// Ctrl+Z even when Shift is also held, so Ctrl+Shift+Z has to be
    /// checked before it.
    pub const KEYBOARD_ORDER: &[Command] = &[
        Command::StampVisible,
        Command::SaveAs,
        Command::Redo,
        Command::NewLayer,
        Command::Open,
        Command::Save,
        Command::Quit,
        Command::Undo,
        Command::DuplicateLayer,
        Command::MergeDown,
        Command::RaiseLayer,
        Command::LowerLayer,
        Command::Invert,
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
            Command::Save => "Save",
            Command::SaveAs => "Save As…",
            Command::ExportTiff => "Export as TIFF (16-bit)…",
            Command::ExportJpeg => "Export as JPEG (sRGB)…",
            Command::Quit => "Quit",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::NewLayer => "New Layer",
            Command::DuplicateLayer => "Duplicate Layer",
            Command::DeleteLayer => "Delete Layer",
            Command::MergeDown => "Merge Down",
            Command::StampVisible => "Stamp Visible",
            Command::RaiseLayer => "Bring Forward",
            Command::LowerLayer => "Send Backward",
            Command::AddMask => "Add Layer Mask",
            Command::DeleteMask => "Delete Layer Mask",
            Command::ToggleMask => "Disable/Enable Layer Mask",
            Command::Invert => "Invert",
            Command::NewCurves => "Curves…",
            Command::NewLevels => "Levels…",
            Command::NewHueSaturation => "Hue/Saturation…",
            Command::NewColorBalance => "Color Balance…",
            Command::GaussianBlur => "Gaussian Blur…",
            Command::FrequencySeparation => "Frequency Separation…",
            Command::DodgeAndBurn => "Dodge & Burn Layer",
            Command::ZoomIn => "Zoom In",
            Command::ZoomOut => "Zoom Out",
            Command::FitOnScreen => "Fit on Screen",
            Command::ActualPixels => "100%",
        }
    }

    pub fn shortcut(self) -> Option<KeyboardShortcut> {
        let s = |m, k| Some(KeyboardShortcut::new(m, k));
        match self {
            Command::Open => s(CMD, Key::O),
            Command::Save => s(CMD, Key::S),
            Command::SaveAs => s(CMD_SHIFT, Key::S),
            Command::Quit => s(CMD, Key::Q),
            Command::Undo => s(CMD, Key::Z),
            Command::Redo => s(CMD_SHIFT, Key::Z),
            Command::NewLayer => s(CMD_SHIFT, Key::N),
            Command::DuplicateLayer => s(CMD, Key::J),
            Command::MergeDown => s(CMD, Key::E),
            Command::StampVisible => s(CMD_ALT_SHIFT, Key::E),
            Command::RaiseLayer => s(CMD, Key::CloseBracket),
            Command::LowerLayer => s(CMD, Key::OpenBracket),
            Command::Invert => s(CMD, Key::I),
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
    pub fn pressed(ctx: &egui::Context) -> Vec<Command> {
        ctx.input_mut(|i| {
            let mut pressed: Vec<Command> = Self::KEYBOARD_ORDER
                .iter()
                .copied()
                .filter(|c| c.shortcut().is_some_and(|s| i.consume_shortcut(&s)))
                .collect();
            // Ctrl++ on keyboards where + is its own key.
            if i.consume_key(CMD, Key::Plus) {
                pressed.push(Command::ZoomIn);
            }
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

    #[test]
    fn more_specific_shortcuts_come_first() {
        let order = Command::KEYBOARD_ORDER;
        let pos = |c| order.iter().position(|&x| x == c).unwrap();
        assert!(pos(Command::Redo) < pos(Command::Undo));
        assert!(pos(Command::SaveAs) < pos(Command::Save));
        assert!(pos(Command::StampVisible) < pos(Command::MergeDown));
    }
}
