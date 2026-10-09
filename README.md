# Omapix

A fast, keyboard-first raster photo editor for [Omarchy](https://omarchy.org),
built around portrait retouching. It aims to feel like Photoshop, stay
lightweight, and look native to your Omarchy theme.

> Omapix is an independent project. It is not made by or affiliated with
> Omarchy.

It's built for one workflow: a base edit in darktable, retouching in
Omapix, then export.

**Status:** pre-release (0.1.0, nothing tagged yet). It's used for real
portrait work, but so far on one machine: Omarchy (Arch, Hyprland) with an
NVIDIA GPU. Other Wayland desktops and other GPUs should work and haven't
been tried. If you try one, [say how it went](CONTRIBUTING.md).

## What it does

- **Editing:** 16-bit and colour-managed, including the monitor's profile
  and soft proofing. Layers, groups, masks, clipping masks, Blend If and
  Photoshop's blend modes. Undo with a History panel. Several images open
  in tabs.
- **Tools:** Move, Brush, Eraser, Clone Stamp, Healing Brush, Spot Healing
  Brush, Dodge, Burn, Sponge, Gradient, Paint Bucket, Eyedropper, Crop, marquees,
  Lasso, Magic Wand, Quick Selection and Object Selection, with Free Transform,
  Auto-Align Layers, Liquify and Select and Mask. Pen pressure from a
  tablet. Photomerge for panoramas and Merge to HDR for brackets.
- **Adjustment layers:** Curves, Levels, Hue/Saturation, Color Balance,
  Selective Color, Channel Mixer and Color Lookup (`.cube` LUTs), with
  presets and export as a LUT.
- **Filters:** Gaussian Blur, Smart Blur, High Pass, Unsharp Mask, Smart
  Sharpen, Add Noise and Reduce Noise, previewed live.
- **Retouching setups in one click:** frequency separation, dodge & burn,
  High Pass sharpening, and a Finish step (sharpen, then grain).
- **AI retouching, on your own machine:** Auto Retouch, Heal Blemishes,
  Smooth Skin, Even Tone, Reduce Shine, Lighten Under Eyes, Whiten Teeth
  and Eyes, Makeup, Face-Aware Liquify, Body Reshape, Select Subject, Skin and
  Hair, Content-Aware Fill, Generative Fill, Denoise and upscaling. Every
  result is an ordinary layer, mask or selection. No cloud and no account
  (see [AI features](#ai-features)).
- **Files:** opens TIFF, PNG, JPEG, PSD, PSB and OpenRaster. Saves layers as
  OpenRaster (`.ora`, which Krita opens too) and exports them as PSD or
  PSB for Photoshop. Exports 16-bit TIFF, JPEG
  and PNG, one at a time or in batches, and Export for Web sizes one for
  posting and says how big the file will be. darktable gets an "edit in Omapix"
  export target that brings the result back beside the raw.

Shortcuts follow Photoshop, and can be changed in
`~/.config/omapix/hotkeys.toml`. See [docs/DESIGN.md](docs/DESIGN.md) for
the design and the main shortcuts, [docs/ROADMAP.md](docs/ROADMAP.md) for
what's done and what's next, and [docs/AI.md](docs/AI.md) for how the AI
features work.

## Install

Omapix needs Linux with Wayland (X11 is untested), a GPU with Vulkan, and
Little CMS 2. On Arch and Omarchy:

```bash
sudo pacman -S --needed lcms2 vulkan-icd-loader xdg-desktop-portal
```

Building needs a recent stable Rust (it's developed on 1.98).

### From source, for your user

```bash
git clone https://github.com/michaelrubi/omapix.git
cd omapix
make install
```

This puts the binary in `~/.local/bin`, adds a launcher entry and icon,
and, if darktable is set up, adds its "edit in Omapix" export target. It
doesn't change which app opens images by default. `make uninstall` removes
it all.

To run it without installing:

```bash
cargo run --release -- path/to/image.tif
```

### As an Arch package

[packaging/arch/PKGBUILD](packaging/arch/PKGBUILD) builds `omapix-git`
from the newest commit. It isn't on the AUR yet.

```bash
cd packaging/arch
makepkg -si
```

The package installs the model downloader as `omapix-fetch-models`. For
darktable's "edit in Omapix", add this line to `~/.config/darktable/luarc`:

```lua
require "omapix"
```

## AI features

These are optional: everything else works without them.

They run through the system's ONNX Runtime, on an NVIDIA GPU with CUDA if
there is one and otherwise on the CPU. On Arch that's `onnxruntime-cuda`
and `cudnn`, or `onnxruntime-cpu`. Omapix opens
`/usr/lib/libonnxruntime.so`; set `OMAPIX_ORT_LIBRARY` if yours is
elsewhere. Generative Fill needs an NVIDIA GPU with 8 GB of memory.

Omapix never goes online itself. A script downloads the models, checks
each file's SHA-256 and prints its licence first:

```bash
scripts/fetch-models.sh
```

That fetches about 800 MB into `~/.local/share/omapix/models`. Generative
Fill's model is 7.8 GB, and only comes when asked for:

```bash
scripts/fetch-models.sh fill-flux2-klein-4b
```

Three models have no download yet and come from darktable's AI
preferences instead: SAM 2.1 (Object Selection), NIND (Denoise) and
RealPLKSR (upscaling). Omapix uses darktable's copies where they are.
Help › AI Models shows what's installed and what's missing.

## Contributing

Bug reports, testing on other hardware, packaging and code are all
welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). The JPEG encoder Omapix is built
with, [zenjpeg](https://crates.io/crates/zenjpeg), is AGPL-3.0, so
binaries carry both licences. The AI models have their own licences,
listed in [crates/omapix-ai/models.txt](crates/omapix-ai/models.txt).
