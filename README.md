# Omapix

A fast, keyboard-first raster photo editor for [Omarchy](https://omarchy.org),
built around portrait retouching. It aims to feel like Photoshop, stay
lightweight, and look native to your Omarchy theme.

> Omapix is an independent project. It is not made by or affiliated with
> Omarchy.

**Status:** early. Milestone 1 (a colour-managed 16-bit image viewer) is in
progress. See [docs/DESIGN.md](docs/DESIGN.md) for the plan.

## Build and run

```bash
cargo run --release -- path/to/image.tif
```

Requires Rust, Vulkan and Little CMS 2 (`lcms2`).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
