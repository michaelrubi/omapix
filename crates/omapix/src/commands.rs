//! Every user action, with its menu label and Photoshop shortcut. Menus and
//! the keyboard both go through this list, so they can't disagree.

use egui::{Key, KeyboardShortcut, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    New,
    NewFromClipboard,
    Open,
    Close,
    Save,
    SaveAs,
    ExportTiff,
    ExportJpeg,
    ExportPng,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    CopyMerged,
    Paste,
    PasteInPlace,
    PasteInto,
    FreeTransform,
    AutoAlignLayers,
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
    LockPixels,
    LockPosition,
    LockAll,
    BringToFront,
    RaiseLayer,
    LowerLayer,
    SendToBack,
    SelectLayerAbove,
    SelectLayerBelow,
    BlendingOptions,
    AddMask,
    AddMaskHideAll,
    DeleteMask,
    ToggleMask,
    MaskDensity,
    MaskOverlay,
    Invert,
    SelectAll,
    Deselect,
    InvertSelection,
    SelectSubject,
    SelectSkin,
    SelectHair,
    SelectEyes,
    SelectLips,
    SelectTeeth,
    BorderSelection,
    SmoothSelection,
    ExpandSelection,
    ContractSelection,
    Feather,
    SelectAndMask,
    SelectionEdges,
    ProofColors,
    GamutWarning,
    ProofSetupWeb,
    ProofSetupCustom,
    QuickMask,
    LoadSelectionRed,
    LoadSelectionGreen,
    LoadSelectionBlue,
    LoadSelectionLuminosity,
    LoadSelectionTransparency,
    LoadSelectionLayerMask,
    SaveSelection,
    DeleteChannel,
    ViewComposite,
    ViewRed,
    ViewGreen,
    ViewBlue,
    FillForeground,
    FillBackground,
    Clear,
    ContentAwareFill,
    GenerativeFill,
    NewCurves,
    NewLevels,
    NewHueSaturation,
    NewColorBalance,
    NewSelectiveColor,
    NewChannelMixer,
    NewColorLookup,
    SaveAdjustmentPreset,
    ExportAdjustmentLut,
    AddNoise,
    ReduceNoise,
    Denoise,
    GaussianBlur,
    SmartBlur,
    HighPass,
    UnsharpMask,
    SmartSharpen,
    AutoRetouch,
    HealBlemishes,
    SmoothSkin,
    EvenTone,
    ReduceShine,
    LightenUnderEyes,
    WhitenTeeth,
    WhitenEyes,
    FrequencySeparation,
    FrequencySeparation3,
    DodgeAndBurn,
    DodgeAndBurnCurves,
    HighPassSharpening,
    ZoomIn,
    ZoomOut,
    FitOnScreen,
    ActualPixels,
    ReopenLast,
    ShowLayers,
    ShowChannels,
    ShowHistory,
    ShowNavigator,
    ShowHistogram,
    NextImage,
    PreviousImage,
    Rotate180,
    Rotate90Cw,
    Rotate90Ccw,
    FlipCanvasHorizontal,
    FlipCanvasVertical,
    ImageSize,
    CanvasSize,
    Crop,
    Liquify,
    BatchExport,
    Photomerge,
    MergeToHdr,
    AiModels,
    Finish,
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
        Command::New,
        Command::NewFromClipboard,
        Command::Open,
        Command::Close,
        Command::Save,
        Command::SaveAs,
        Command::ExportTiff,
        Command::ExportJpeg,
        Command::ExportPng,
        Command::Quit,
        Command::Undo,
        Command::Redo,
        Command::Cut,
        Command::Copy,
        Command::CopyMerged,
        Command::Paste,
        Command::PasteInPlace,
        Command::PasteInto,
        Command::FreeTransform,
        Command::AutoAlignLayers,
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
        Command::LockPixels,
        Command::LockPosition,
        Command::LockAll,
        Command::BringToFront,
        Command::RaiseLayer,
        Command::LowerLayer,
        Command::SendToBack,
        Command::SelectLayerAbove,
        Command::SelectLayerBelow,
        Command::BlendingOptions,
        Command::AddMask,
        Command::AddMaskHideAll,
        Command::DeleteMask,
        Command::ToggleMask,
        Command::MaskDensity,
        Command::MaskOverlay,
        Command::Invert,
        Command::SelectAll,
        Command::Deselect,
        Command::InvertSelection,
        Command::SelectSubject,
        Command::SelectSkin,
        Command::SelectHair,
        Command::SelectEyes,
        Command::SelectLips,
        Command::SelectTeeth,
        Command::BorderSelection,
        Command::SmoothSelection,
        Command::ExpandSelection,
        Command::ContractSelection,
        Command::Feather,
        Command::SelectAndMask,
        Command::SelectionEdges,
        Command::ProofColors,
        Command::GamutWarning,
        Command::ProofSetupWeb,
        Command::ProofSetupCustom,
        Command::QuickMask,
        Command::LoadSelectionRed,
        Command::LoadSelectionGreen,
        Command::LoadSelectionBlue,
        Command::LoadSelectionLuminosity,
        Command::LoadSelectionTransparency,
        Command::LoadSelectionLayerMask,
        Command::SaveSelection,
        Command::DeleteChannel,
        Command::ViewComposite,
        Command::ViewRed,
        Command::ViewGreen,
        Command::ViewBlue,
        Command::FillForeground,
        Command::FillBackground,
        Command::Clear,
        Command::ContentAwareFill,
        Command::GenerativeFill,
        Command::NewCurves,
        Command::NewLevels,
        Command::NewHueSaturation,
        Command::NewColorBalance,
        Command::NewSelectiveColor,
        Command::NewChannelMixer,
        Command::NewColorLookup,
        Command::SaveAdjustmentPreset,
        Command::ExportAdjustmentLut,
        Command::AddNoise,
        Command::ReduceNoise,
        Command::Denoise,
        Command::GaussianBlur,
        Command::SmartBlur,
        Command::HighPass,
        Command::UnsharpMask,
        Command::SmartSharpen,
        Command::AutoRetouch,
        Command::HealBlemishes,
        Command::SmoothSkin,
        Command::EvenTone,
        Command::ReduceShine,
        Command::LightenUnderEyes,
        Command::WhitenTeeth,
        Command::WhitenEyes,
        Command::FrequencySeparation,
        Command::FrequencySeparation3,
        Command::DodgeAndBurn,
        Command::DodgeAndBurnCurves,
        Command::HighPassSharpening,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::FitOnScreen,
        Command::ActualPixels,
        Command::ReopenLast,
        Command::ShowLayers,
        Command::ShowChannels,
        Command::ShowHistory,
        Command::ShowNavigator,
        Command::ShowHistogram,
        Command::NextImage,
        Command::PreviousImage,
        Command::Rotate180,
        Command::Rotate90Cw,
        Command::Rotate90Ccw,
        Command::FlipCanvasHorizontal,
        Command::FlipCanvasVertical,
        Command::ImageSize,
        Command::CanvasSize,
        Command::Crop,
        Command::Liquify,
        Command::BatchExport,
        Command::Photomerge,
        Command::MergeToHdr,
        Command::AiModels,
        Command::Finish,
    ];

    /// Look a command up by its name in code, e.g. "FrequencySeparation".
    pub fn from_name(name: &str) -> Option<Command> {
        if name == "NewLut" {
            return Some(Command::NewColorLookup);
        }
        if name == "HideSelectionEdges" || name == "ShowSelectionEdges" {
            return Some(Command::SelectionEdges);
        }
        Self::ALL.iter().copied().find(|c| format!("{c:?}") == name)
    }

    /// Every command with a shortcut, most modifiers first. egui matches
    /// Ctrl+Z even when Shift is also held, so Ctrl+Shift+Z has to be
    /// checked before it.
    pub const KEYBOARD_ORDER: &[Command] = &[
        Command::StampVisible,
        Command::GamutWarning,
        Command::PasteInto,
        Command::InvertSelection,
        Command::UngroupLayers,
        Command::ClippingMask,
        Command::ImageSize,
        Command::CanvasSize,
        Command::Liquify,
        Command::SaveAs,
        Command::Redo,
        Command::CopyMerged,
        Command::NewLayer,
        Command::PasteInPlace,
        Command::BringToFront,
        Command::SendToBack,
        Command::ReopenLast,
        Command::PreviousImage,
        Command::NextImage,
        Command::New,
        Command::Open,
        Command::Close,
        Command::Save,
        Command::Quit,
        Command::Undo,
        Command::Cut,
        Command::Copy,
        Command::Paste,
        Command::FreeTransform,
        Command::DuplicateLayer,
        Command::MergeDown,
        Command::GroupLayers,
        Command::RaiseLayer,
        Command::LowerLayer,
        Command::Invert,
        Command::SelectAll,
        Command::Deselect,
        Command::SelectionEdges,
        Command::ProofColors,
        Command::Feather,
        Command::SelectAndMask,
        Command::SelectLayerAbove,
        Command::SelectLayerBelow,
        Command::FillForeground,
        Command::FillBackground,
        Command::Clear,
        Command::LockTransparent,
        Command::LockPixels,
        Command::LockPosition,
        Command::LockAll,
        Command::MaskOverlay,
        Command::QuickMask,
        Command::ShowLayers,
        Command::NewCurves,
        Command::NewLevels,
        Command::NewHueSaturation,
        Command::NewColorBalance,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::FitOnScreen,
        Command::ActualPixels,
        Command::ViewComposite,
        Command::ViewRed,
        Command::ViewGreen,
        Command::ViewBlue,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Command::New => "New…",
            Command::NewFromClipboard => "New from Clipboard",
            Command::Open => "Open…",
            Command::Close => "Close",
            Command::ReopenLast => "Reopen Last Document",
            Command::Save => "Save",
            Command::SaveAs => "Save As…",
            Command::ExportTiff => "Export as TIFF (16-bit)…",
            Command::ExportJpeg => "Export as JPEG (sRGB)…",
            Command::ExportPng => "Export as PNG (sRGB)…",
            Command::Quit => "Quit",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::Cut => "Cut",
            Command::Copy => "Copy",
            Command::CopyMerged => "Copy Merged",
            Command::Paste => "Paste",
            Command::PasteInPlace => "Paste in Place",
            Command::PasteInto => "Paste Into",
            Command::FreeTransform => "Free Transform",
            Command::AutoAlignLayers => "Auto-Align Layers",
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
            Command::LockPixels => "Lock Image Pixels",
            Command::LockPosition => "Lock Position",
            Command::LockAll => "Lock All",
            Command::BringToFront => "Bring to Front",
            Command::RaiseLayer => "Bring Forward",
            Command::LowerLayer => "Send Backward",
            Command::SendToBack => "Send to Back",
            Command::SelectLayerAbove => "Select Layer Above",
            Command::SelectLayerBelow => "Select Layer Below",
            Command::BlendingOptions => "Blending Options…",
            Command::AddMask => "Add Layer Mask",
            Command::AddMaskHideAll => "Add Layer Mask (Hide All)",
            Command::DeleteMask => "Delete Layer Mask",
            Command::ToggleMask => "Disable/Enable Layer Mask",
            Command::MaskDensity => "Mask Density…",
            Command::MaskOverlay => "Mask Overlay",
            Command::Invert => "Invert",
            Command::SelectAll => "All",
            Command::Deselect => "Deselect",
            Command::InvertSelection => "Inverse",
            Command::SelectSubject => "Subject",
            Command::SelectSkin => "Skin",
            Command::SelectHair => "Hair",
            Command::SelectEyes => "Eyes",
            Command::SelectLips => "Lips",
            Command::SelectTeeth => "Teeth",
            Command::BorderSelection => "Border…",
            Command::SmoothSelection => "Smooth…",
            Command::ExpandSelection => "Expand…",
            Command::ContractSelection => "Contract…",
            Command::Feather => "Feather…",
            Command::SelectAndMask => "Select and Mask…",
            Command::SelectionEdges => "Selection Edges",
            Command::ProofColors => "Proof Colors",
            Command::GamutWarning => "Gamut Warning",
            Command::ProofSetupWeb => "Web (sRGB)",
            Command::ProofSetupCustom => "Custom Profile…",
            Command::QuickMask => "Edit in Quick Mask Mode",
            Command::LoadSelectionRed => "Red",
            Command::LoadSelectionGreen => "Green",
            Command::LoadSelectionBlue => "Blue",
            Command::LoadSelectionLuminosity => "Luminosity",
            Command::LoadSelectionTransparency => "Transparency",
            Command::LoadSelectionLayerMask => "Layer Mask",
            Command::SaveSelection => "Save Selection",
            Command::DeleteChannel => "Delete Channel",
            Command::ViewComposite => "RGB",
            Command::ViewRed => "Red",
            Command::ViewGreen => "Green",
            Command::ViewBlue => "Blue",
            Command::FillForeground => "Fill with Foreground",
            Command::FillBackground => "Fill with Background",
            Command::Clear => "Clear",
            Command::ContentAwareFill => "Content-Aware Fill",
            Command::GenerativeFill => "Generative Fill…",
            Command::NewCurves => "Curves…",
            Command::NewLevels => "Levels…",
            Command::NewHueSaturation => "Hue/Saturation…",
            Command::NewColorBalance => "Color Balance…",
            Command::NewSelectiveColor => "Selective Color…",
            Command::NewChannelMixer => "Channel Mixer…",
            Command::NewColorLookup => "Color Lookup…",
            Command::SaveAdjustmentPreset => "Save Adjustment Preset…",
            Command::ExportAdjustmentLut => "Export Adjustments as LUT…",
            Command::AddNoise => "Add Noise…",
            Command::ReduceNoise => "Reduce Noise…",
            Command::Denoise => "Denoise…",
            Command::GaussianBlur => "Gaussian Blur…",
            Command::SmartBlur => "Smart Blur…",
            Command::HighPass => "High Pass…",
            Command::UnsharpMask => "Unsharp Mask…",
            Command::SmartSharpen => "Smart Sharpen…",
            Command::HighPassSharpening => "High Pass Sharpening…",
            Command::AutoRetouch => "Auto Retouch…",
            Command::HealBlemishes => "Heal Blemishes…",
            Command::SmoothSkin => "Smooth Skin…",
            Command::EvenTone => "Even Tone…",
            Command::ReduceShine => "Reduce Shine",
            Command::LightenUnderEyes => "Lighten Under Eyes",
            Command::WhitenTeeth => "Whiten Teeth",
            Command::WhitenEyes => "Whiten Eyes",
            Command::FrequencySeparation => "Frequency Separation…",
            Command::FrequencySeparation3 => "Frequency Separation (3 Bands)…",
            Command::DodgeAndBurn => "Dodge & Burn Layer",
            Command::DodgeAndBurnCurves => "Dodge & Burn Curves",
            Command::ZoomIn => "Zoom In",
            Command::ZoomOut => "Zoom Out",
            Command::FitOnScreen => "Fit on Screen",
            Command::ActualPixels => "100%",
            Command::ShowLayers => "Layers",
            Command::ShowChannels => "Channels",
            Command::ShowHistory => "History",
            Command::ShowNavigator => "Navigator",
            Command::NextImage => "Next Image",
            Command::PreviousImage => "Previous Image",
            Command::ShowHistogram => "Histogram",
            Command::Rotate180 => "180°",
            Command::Rotate90Cw => "90° Clockwise",
            Command::Rotate90Ccw => "90° Counter Clockwise",
            Command::FlipCanvasHorizontal => "Flip Canvas Horizontal",
            Command::FlipCanvasVertical => "Flip Canvas Vertical",
            Command::ImageSize => "Image Size…",
            Command::CanvasSize => "Canvas Size…",
            Command::Crop => "Crop",
            Command::Liquify => "Liquify…",
            Command::BatchExport => "Batch Export…",
            Command::Photomerge => "Photomerge…",
            Command::MergeToHdr => "Merge to HDR…",
            Command::AiModels => "AI Models…",
            Command::Finish => "Finish",
        }
    }

    pub fn default_shortcut(self) -> Option<KeyboardShortcut> {
        let s = |m, k| Some(KeyboardShortcut::new(m, k));
        match self {
            Command::New => s(CMD, Key::N),
            Command::Open => s(CMD, Key::O),
            Command::Close => s(CMD, Key::W),
            Command::ReopenLast => s(CMD_SHIFT, Key::O),
            Command::Save => s(CMD, Key::S),
            Command::SaveAs => s(CMD_SHIFT, Key::S),
            Command::Quit => s(CMD, Key::Q),
            Command::NextImage => s(CMD, Key::Tab),
            Command::PreviousImage => s(CMD_SHIFT, Key::Tab),
            Command::Undo => s(CMD, Key::Z),
            Command::Redo => s(CMD_SHIFT, Key::Z),
            Command::Cut => s(CMD, Key::X),
            Command::Copy => s(CMD, Key::C),
            Command::CopyMerged => s(CMD_SHIFT, Key::C),
            Command::Paste => s(CMD, Key::V),
            Command::PasteInPlace => s(CMD_SHIFT, Key::V),
            Command::PasteInto => s(CMD_ALT_SHIFT, Key::V),
            Command::FreeTransform => s(CMD, Key::T),
            Command::NewLayer => s(CMD_SHIFT, Key::N),
            Command::DuplicateLayer => s(CMD, Key::J),
            Command::MergeDown => s(CMD, Key::E),
            Command::StampVisible => s(CMD_ALT_SHIFT, Key::E),
            Command::GroupLayers => s(CMD, Key::G),
            Command::UngroupLayers => s(CMD_SHIFT, Key::G),
            Command::ClippingMask => s(CMD_ALT, Key::G),
            Command::LockTransparent => s(Modifiers::NONE, Key::Slash),
            Command::LockAll => s(CMD, Key::Slash),
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
            Command::SelectionEdges => s(CMD, Key::H),
            Command::ProofColors => s(CMD, Key::Y),
            Command::GamutWarning => s(CMD_SHIFT, Key::Y),
            Command::Feather => s(Modifiers::SHIFT, Key::F6),
            Command::SelectAndMask => s(CMD_ALT, Key::R),
            Command::ImageSize => s(CMD_ALT, Key::I),
            Command::CanvasSize => s(CMD_ALT, Key::C),
            Command::Liquify => s(CMD_SHIFT, Key::X),
            Command::FillForeground => s(Modifiers::ALT, Key::Backspace),
            Command::FillBackground => s(CMD, Key::Backspace),
            Command::Clear => s(Modifiers::NONE, Key::Delete),
            Command::MaskOverlay => s(Modifiers::NONE, Key::Backslash),
            Command::QuickMask => s(Modifiers::NONE, Key::Q),
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
            Command::ViewComposite => s(CMD, Key::Num2),
            Command::ViewRed => s(CMD, Key::Num3),
            Command::ViewGreen => s(CMD, Key::Num4),
            Command::ViewBlue => s(CMD, Key::Num5),
            _ => None,
        }
    }

    pub fn shortcut(self) -> Option<KeyboardShortcut> {
        crate::hotkeys::current().command(self)
    }

    /// Commands whose shortcut was pressed this frame, consuming the keys.
    /// `v_down` remembers between frames whether V is held.
    pub fn pressed(ctx: &egui::Context, v_down: &mut bool) -> Vec<Command> {
        Self::pressed_with(ctx, v_down, crate::hotkeys::current())
    }

    /// [`Self::pressed`] with these shortcuts.
    pub fn pressed_with(
        ctx: &egui::Context,
        v_down: &mut bool,
        hotkeys: &crate::hotkeys::Hotkeys,
    ) -> Vec<Command> {
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
            let mut pressed: Vec<Command> = hotkeys
                .keyboard_order()
                .iter()
                .copied()
                .filter(|&c| hotkeys.command(c).is_some_and(|s| i.consume_shortcut(&s)))
                .collect();
            // Ctrl++ on keyboards where + is its own key.
            if hotkeys.command(Command::ZoomIn).is_some() && i.consume_key(CMD, Key::Plus) {
                pressed.push(Command::ZoomIn);
            }
            // egui-winit turns Ctrl+C and Ctrl+X into Copy and Cut events
            // instead of key presses. Ctrl+V becomes a Paste event only when
            // the clipboard holds text, not an image, but its V is still
            // released: a V released without being pressed was Ctrl+V.
            // Whether `command`'s shortcut is `key` with the Shift and Alt held now.
            let held = |command: Command, key: Key| {
                hotkeys.command(command).is_some_and(|s| {
                    s.logical_key == key && s.modifiers.alt == i.modifiers.alt && s.modifiers.shift == i.modifiers.shift
                })
            };
            for event in &i.events {
                match event {
                    // Ctrl+Alt+C (Canvas Size) comes as a Copy too, and
                    // Ctrl+Shift+X (Liquify) as a Cut.
                    egui::Event::Copy if held(Command::CanvasSize, Key::C) => pressed.push(Command::CanvasSize),
                    egui::Event::Cut if held(Command::Liquify, Key::X) => pressed.push(Command::Liquify),
                    egui::Event::Copy if i.modifiers.shift => pressed.push(Command::CopyMerged),
                    egui::Event::Copy => pressed.push(Command::Copy),
                    egui::Event::Cut => pressed.push(Command::Cut),
                    egui::Event::Key {
                        key: Key::V,
                        pressed: down,
                        modifiers,
                        ..
                    } => {
                        if !*down && !*v_down {
                            // Whichever paste V is with the Shift and Alt held.
                            let held = |m: Modifiers| modifiers.shift == m.shift && modifiers.alt == m.alt;
                            let paste = [Command::Paste, Command::PasteInPlace, Command::PasteInto]
                                .into_iter()
                                .find(|&c| {
                                    hotkeys.command(c).is_some_and(|s| s.logical_key == Key::V && held(s.modifiers))
                                });
                            if let Some(paste) = paste.filter(|p| !pressed.contains(p)) {
                                pressed.push(paste);
                            }
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
        // Ctrl+Alt+C comes as a Copy with Alt held.
        let copy = vec![egui::Event::Copy];
        assert_eq!(press(&ctx, &mut v_down, CMD_ALT, copy), [Command::CanvasSize]);
        let cut = vec![egui::Event::Cut];
        assert_eq!(press(&ctx, &mut v_down, CMD, cut.clone()), [Command::Cut]);
        assert_eq!(press(&ctx, &mut v_down, CMD_SHIFT, cut), [Command::Liquify]);

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
        // Ctrl+Shift+V (Paste in Place)
        assert_eq!(
            press(&ctx, &mut v_down, CMD_SHIFT, vec![v(false, CMD_SHIFT)]),
            [Command::PasteInPlace]
        );
        // Ctrl+Alt+Shift+V (Paste Into)
        assert_eq!(
            press(&ctx, &mut v_down, CMD_ALT_SHIFT, vec![v(false, CMD_ALT_SHIFT)]),
            [Command::PasteInto]
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
        assert!(pos(Command::PreviousImage) < pos(Command::NextImage));
        assert!(pos(Command::NewLayer) < pos(Command::New));
        assert!(pos(Command::StampVisible) < pos(Command::MergeDown));
        assert!(pos(Command::InvertSelection) < pos(Command::Invert));
        assert!(pos(Command::CopyMerged) < pos(Command::Copy));
        assert!(pos(Command::UngroupLayers) < pos(Command::GroupLayers));
        assert!(pos(Command::ClippingMask) < pos(Command::GroupLayers));
        assert!(pos(Command::BringToFront) < pos(Command::RaiseLayer));
        assert!(pos(Command::SendToBack) < pos(Command::LowerLayer));
    }

    #[test]
    fn ctrl_h_triggers_selection_edges() {
        let ctx = egui::Context::default();
        let mut v_down = false;
        let event = egui::Event::Key {
            key: Key::H,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: CMD,
        };
        assert_eq!(
            press(&ctx, &mut v_down, CMD, vec![event]),
            [Command::SelectionEdges]
        );
    }
}
