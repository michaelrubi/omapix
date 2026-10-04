# Contributing to Omapix

Omapix is a small, opinionated project, built around one photographer's
portrait workflow and tested on one machine. That makes outside help
useful in ways that aren't all code.

Issues and pull requests go to
[github.com/michaelrubi/omapix](https://github.com/michaelrubi/omapix).

## Ways to help

- **Try it on your machine.** It has only run on Omarchy (Arch, Hyprland)
  with an NVIDIA GPU. A report from an AMD or Intel GPU, another Wayland
  compositor, X11, a tablet, or another distribution is worth having, even
  if all it says is "it works".
- **Report bugs.** See below for what to put in one.
- **Say where it differs from Photoshop.** A shortcut, a modifier key or a
  tool that doesn't behave as Photoshop's does is a bug here, not a matter
  of taste.
- **Package it.** There's an Arch package in
  [packaging/arch](packaging/arch). The roadmap's
  [section 8](docs/ROADMAP.md#8-releases-packaging-and-contributors) lists
  what's still missing.
- **Write code.** The roadmap lists what's next, and the "Later:" lines
  under finished items are mostly small and self-contained.

## Reporting a bug

Open an issue with:

- what you did, what you expected, and what happened
- the commit you built (`git rev-parse --short HEAD`)
- your distribution, compositor and GPU
- for a problem with one file: its format, bit depth and size, and the
  file itself if you can share it
- for a crash or anything odd: the output of running Omapix from a
  terminal with `RUST_LOG=info omapix photo.tif`
- for an AI feature: what Help › AI Models shows for the model, including
  whether it says GPU or CPU

## Before you write code

Open an issue first for anything bigger than a fix. Omapix says no to a
lot, and it's better to find that out before the work than after:

- **Retouch-first.** Features are chosen by what a portrait workflow
  needs.
- **Photoshop muscle memory.** If Photoshop has the feature, Omapix copies
  its name, menu, shortcut and behaviour.
- **Opinionated.** Good defaults instead of settings.
- **Lightweight.** A feature has to justify what it costs in startup time,
  memory and dependencies.
- **Raster only,** with no plugins and no scripting beyond
  `OMAPIX_SCRIPT`.
- **Nothing online.** Omapix never opens a network connection. Models are
  downloaded by a script the user runs.

[docs/DESIGN.md](docs/DESIGN.md) has these in full.

## Building and testing

You need a recent stable Rust, Little CMS 2 (`lcms2`) and Vulkan. See the
[README](README.md#install).

```bash
cargo test
```

```bash
cargo clippy --workspace --all-targets
```

Both should pass with no warnings before you open a pull request. There's
no CI yet, so nothing else will check. The tests need no display, GPU or
AI models. A few are timed and can fail on a busy machine; run them again
before deciding you broke something.

To try a change by hand, `cargo run --release -- photo.tif`, or
`make install` to replace the copy in `~/.local/bin`.

`OMAPIX_SCRIPT` drives the app through the same code as the mouse and
menus, which is useful for reproducing a bug and for timing things:

```bash
OMAPIX_SCRIPT="FrequencySeparation,Size 300,Opacity 70,Stroke 1000 1000 1500 1000" cargo run --release -- photo.tif
```

Tests marked `#[ignore]` need something the repository doesn't have: the
AI models, a photo named in an environment variable, or the real Wayland
clipboard. Each says what it needs in the comment above it.

## How the code is laid out

```
crates/
  omapix-engine   pixels, colour management, file formats, filters and
                  compositing. No UI and no GPU, so all of it is tested
                  headless.
  omapix-ai       ONNX Runtime and the models. No UI.
  omapix          the app: egui on wgpu, tools, commands, panels, theme.
```

- **Keep the engine free of UI and GPU code.** Image maths goes in
  `omapix-engine` with unit tests on small synthetic images, even when
  it's there to serve an AI feature.
- **Everything the user can do is a `Command`**
  (`crates/omapix/src/commands.rs`), so menus, shortcuts and
  `OMAPIX_SCRIPT` can't drift apart.
- **Test what you add.** UI behaviour is tested by driving egui with
  made-up events; the `Harness` in `crates/omapix/src/layers_panel.rs` is
  the pattern to copy.
- **Keep changes small.** No new abstraction until something needs it, and
  no new dependency without a reason that's worth its build time.
- **Don't run `cargo fmt`.** The code isn't formatted with rustfmt's
  defaults, and reformatting a file buries the change in it. Match the
  code around yours, and turn off format-on-save.

## Pull requests

- One feature or fix in each, on a branch from `main`.
- Commit messages say what changed for the user, as the existing ones do:
  "Export as PNG", "Face-Aware Liquify: Preview".
- If the change finishes something on the roadmap, strike it through there
  and write what was built in its place, with anything left over as a
  "Later:" line. If it adds a shortcut, add it to the table in DESIGN.md.
- Say how you tested it, and on what hardware.

[AGENTS.md](AGENTS.md) has the same rules in short, for coding agents.

## Licences

Omapix is GPL-3.0-or-later, and contributions are taken under the same
licence.

- **Code from elsewhere** is fine from GPL-compatible projects (Krita,
  GIMP and darktable among them). Say where it came from in the file it
  lands in.
- **New dependencies** need GPL-compatible licences.
- **AI models** in the default set need weights and training data that
  both allow commercial use, since Omapix is for client work
  ([docs/AI.md](docs/AI.md), principle 4). A model goes in
  `crates/omapix-ai/models.txt` with its licence, its author, and each
  file's size, SHA-256 and a download URL fixed to one revision.
