# Changelog

## 0.1.0 (2026-10-20)

The first release of Omapix: a keyboard-first raster photo editor for portrait retouching on [Omarchy](https://omarchy.org), with Photoshop's shortcuts. Base edit in darktable, retouch in Omapix, export.

So far it has been used for real portrait work on one machine: Omarchy (Arch, Hyprland) with an NVIDIA GPU, and an Intel iGPU for the canvas. Reports from other GPUs and desktops are very welcome: open an issue and say how it went.

### What's in it

- **Editing:** 16-bit and colour-managed, including the monitor's profile and soft proofing. Layers, groups, masks, clipping masks, Blend If and Photoshop's blend modes. History, and tabs.
- **Tools:** the Photoshop set for retouching: brushes with Photoshop's soft tip and `.abr` brushes, Clone Stamp, Healing and Spot Healing, Dodge, Burn and Sponge, marquees, Lasso, Magic Wand, Quick and Object Selection, Free Transform, Liquify, and Select and Mask. Pen pressure, tilt and rotation from a tablet.
- **Brush Settings (F5):** Brush Tip Shape, Shape Dynamics, Scattering, Texture, Dual Brush, Color Dynamics, Transfer, Brush Pose, Noise and Wet Edges, read from Photoshop brush files too, and Smoothing (with Pulled String Mode) in the options bar.
- **Retouching setups:** frequency separation (two or three bands), Dodge & Burn Curves, High Pass sharpening and a Finish step.
- **AI retouching, on your own machine:** Auto Retouch, Heal Blemishes, Smooth Skin, Even Tone, Reduce Shine, Lighten Under Eyes, Whiten Teeth and Eyes, Makeup, Face-Aware Liquify, Body Reshape, Select Subject, Skin and Hair, Content-Aware and Generative Fill, Denoise and upscaling. Every result is an ordinary layer, mask or selection. The models are downloaded with `omapix-fetch-models` (or `scripts/fetch-models.sh`), and no longer need darktable.
- **Files:** opens TIFF, PNG, JPEG, PSD, PSB and OpenRaster. Saves layers as OpenRaster and exports PSD/PSB, 16-bit TIFF, JPEG and PNG, singly or in batches. darktable's "edit in Omapix" round trip.
- **For agents:** `omapix --mcp` serves the Model Context Protocol over stdio, to inspect, retouch and export documents headlessly, and a running Omapix takes commands over `$XDG_RUNTIME_DIR/omapix.sock`. Opening a file while Omapix runs opens it as a tab in that window.

### Install

On Arch: the `omapix` package (from this tag) or `omapix-git` from `packaging/arch`, or `make install` from source. See the [README](https://github.com/michaelrubi/omapix#install) and the [guide](https://github.com/michaelrubi/omapix/tree/v0.1.0/docs/guide).
