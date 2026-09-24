# Omapix

A fast, keyboard-first raster photo editor for [Omarchy](https://omarchy.org),
built around portrait retouching. It aims to feel like Photoshop, stay
lightweight, and look native to your Omarchy theme.

> Omapix is an independent project. It is not made by or affiliated with
> Omarchy.

**Status:** early but usable for portrait retouching. It has 16-bit
colour-managed editing; layers with Photoshop's blend modes and masks;
undo; move tool; brush, eraser, clone stamp and healing brush; marquee and lasso
selections with feathering; Curves, Levels, Hue/Saturation, Color
Balance, Selective Color, Channel Mixer and Color Lookup (LUT) adjustment layers;
one-click frequency separation and dodge & burn
layers; and OpenRaster save with TIFF/JPEG export. See [docs/DESIGN.md](docs/DESIGN.md) for the design and shortcuts, and
[docs/ROADMAP.md](docs/ROADMAP.md) for what's next.

## Build and run

```bash
cargo run --release -- path/to/image.tif
```

To install for your user (binary in `~/.local/bin`, plus a launcher entry
and icon), run `make install`. It doesn't change which app opens images
by default. `make uninstall` removes it.

Requires Rust, Vulkan and Little CMS 2 (`lcms2`).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
