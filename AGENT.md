# AGENT.md

Omapix is a fast, keyboard-first raster photo editor for Omarchy, focused on portrait retouching (darktable base edit → Omapix retouch → export). It is designed to match Photoshop muscle memory and feel native to Omarchy.

## Principles

- **Photoshop muscle memory**: Shortcuts, tool letters, navigation, and conventions match Photoshop.
- **Retouch-first**: Built around portrait retouching (frequency separation, dodging & burning, healing, blemish removal, tone curves).
- **Opinionated & lightweight**: No unnecessary settings or abstractions. Starts fast, low memory overhead.
- **Engine/UI separation**: `omapix-engine` has no UI or GPU dependencies and is 100% headlessly testable.

## Architecture

```
crates/
  omapix-engine/   16-bit RGBA copy-on-write tiled raster (256×256 tiles),
                   Little CMS 2 colour management, OpenRaster (.ora) IO,
                   TIFF/JPEG export, filters, adjustments, blend modes.
  omapix/          Application layer: egui UI on wgpu canvas, input handling,
                   tool system, commands, panels, and Omarchy theme integration.
```

- **Coordinates & tiles**: Documents use 16-bit per channel RGBA (`[u16; 4]`). `Tiled<T>` divides layers into 256×256 copy-on-write tiles. Untouched areas share tiles; snapshots for undo cost almost nothing.
- **Display pipeline**: Mip pyramid downsampling into 512 px display tiles with 1 px borders for seamless filtering, uploaded lazily to the GPU.
- **Commands**: All actions go through `Command` in `crates/omapix/src/commands.rs` so menus, shortcuts, and headless scripts never diverge.

## Build and Test Commands

```bash
# Run unit tests (both engine and app UI headless tests)
cargo test

# Build release binary
cargo build --release

# Build and install to ~/.local/bin (the copy Michael actually runs)
make install

# Run application with an image
cargo run --release -- path/to/image.tif

# Test UI headlessly with OMAPIX_SCRIPT
OMAPIX_SCRIPT="FrequencySeparation,Size 300,Opacity 70,Stroke 1000 1000 1500 1000" \
  cargo run --release -- path/to/image.tif
```

## Conventions

- **Code edits**: Keep diffs minimal and surgical. Do not introduce speculative abstractions or unnecessary dependencies.
- **Hand testing**: Michael tests from the installed binary, not `cargo run`. After a change he'll try by hand, run `make install` and ask him to restart Omapix. `cargo test` and `clippy` don't rebuild the release binary.
- **Testing**: Whenever non-trivial UI or engine logic is added, write a headless unit test. In `omapix`, use the `Harness` pattern in `layers_panel.rs` (driving egui with synthetic events) to test interactions without a display.
- **Formatting**: Output code blocks flush-left (zero indentation).
