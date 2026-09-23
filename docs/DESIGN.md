# Omapix design

Omapix is a fast, keyboard-first raster photo editor for Omarchy, built
around portrait retouching. It should feel like Photoshop to someone with
Photoshop muscle memory, and start and run like a native Omarchy app.

It is an independent project, not part of Omarchy.

## Principles

1. **Retouch-first.** Features are prioritised by a real portrait workflow:
   darktable base edit → retouch here → export. General-purpose features
   come after that workflow is complete.
2. **Photoshop muscle memory.** Default shortcuts, tool letters and
   navigation match Photoshop. No setup needed to be productive.
3. **Opinionated, not configurable.** Good defaults over preference dialogs.
   The one-click workflows (frequency separation, dodge & burn setup) are
   built in, not plugins.
4. **Omarchy-native.** Wayland-native, colours come from the active Omarchy
   theme and follow theme switches live, installs as one package.
5. **Lightweight.** Starts instantly, stays responsive on 24 MP 16-bit
   files. Every feature must justify its cost in startup time and memory.
6. **Raster only.** No vector or layout personas.

## Architecture

```
crates/
  omapix-engine   image data, colour management, file IO, processing
                  (no UI or GPU dependencies — testable headless)
  omapix          the app: egui UI on wgpu, canvas, input, theme
```

The engine never depends on the UI. This keeps processing testable from
unit tests, and leaves room to replace the UI toolkit later.

### Pixels

- Documents are stored at 16 bits per channel, RGBA, in the document's own
  colour space (the ICC profile embedded in the file, or sRGB if none).
  8-bit files are promoted to 16-bit on load.
- Milestone 1 holds each image as one contiguous buffer. The tile engine
  (below) replaces this before layers land in milestone 2.

### Colour management

- Little CMS 2 (`lcms2`) converts document colour → display colour.
- Milestone 1 displays in sRGB. Reading the monitor's ICC profile comes
  later.
- Editing maths stays in the document's space; only the display cache is
  converted.

### Display pipeline

1. On load, build a mip pyramid (each level half the size of the one before,
   2×2 box filter). When the document's profile is linear, averaging is
   physically correct; gamma-encoded documents get a small, acceptable error
   when zoomed out.
2. Split each level into 512 px display tiles with a 1 px border, so linear
   filtering never shows seams between tiles.
3. Tiles are converted to 8-bit sRGB and uploaded to the GPU lazily, the
   first time they are visible.
4. At each zoom, draw the smallest level that is still at least as large as
   the screen area. Above 100 %, pixels are drawn nearest-neighbour so they
   stay crisp, like Photoshop.

### Tile engine (before milestone 2)

- 256×256 tiles, copy-on-write, so undo stores only the tiles a stroke
  touched.
- Layers composite tile by tile. Blend modes run on the GPU for display and
  on the CPU for export, with shared test vectors to keep them identical.

## Milestones

### 1. Viewer (current)

- Open 16-bit (and 8-bit) TIFF with its embedded ICC profile; PNG and JPEG
  through the `image` crate.
- Colour-managed display to sRGB.
- GPU canvas with smooth pan and zoom on large files.
- Photoshop navigation: see the shortcut table below.
- Omarchy theme colours, updated live when the theme changes.
- Open from the command line, from Ctrl+O, or by dropping a file on the
  window.
- Status bar: zoom, document size, bit depth, colour profile, pixel under
  cursor.

### 2. Layers

Tile engine, pixel layers, groups, opacity, blend modes (including
Soft Light, Grain Extract and Grain Merge), layer masks, undo and redo,
native file format.

### 3. Retouch tools

Pressure-sensitive brush engine, clone stamp, healing brush, spot healing
brush, one-key frequency separation, dodge & burn setup.

### 4. Adjustments

Curves, Levels, Hue/Saturation, Color Balance as adjustment layers; LUT
loading; export with conversion to sRGB.

### 5. Beyond

Liquify, AI-assisted retouching (local models via ONNX Runtime on the GPU),
PSD import.

## Shortcuts (milestone 1)

| Action | Keys |
|---|---|
| Open | Ctrl+O |
| Fit on screen | Ctrl+0 |
| 100 % | Ctrl+1 |
| Zoom in / out | Ctrl+= / Ctrl+- |
| Zoom at cursor | Alt+scroll, Ctrl+scroll, pinch |
| Pan | Space+drag, middle-drag, scroll / Shift+scroll |
| Quit | Ctrl+Q |

## Licensing

GPL-3.0-or-later. Code may be reused from GPL-compatible projects such as
Krita, GIMP and darktable, with attribution in the file it lands in.
