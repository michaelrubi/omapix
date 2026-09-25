mod app;
mod blending_options;
mod canvas;
mod channels_panel;
mod clipboard;
mod commands;
mod drop;
mod editor;
mod free_transform;
mod gpu;
mod history_panel;
mod hotkeys;
mod layers_panel;
mod live;
mod properties_panel;
mod recent;
mod settings;
mod theme;
mod tools;

use std::path::PathBuf;

use eframe::egui_wgpu::WgpuSetup;
use eframe::wgpu::PowerPreference;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,omapix=info"))
        .init();
    // `omapix --round-trip image.tif` is how darktable sends an image (see
    // darktable/omapix.lua): saving also writes it back to the TIFF.
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let round_trip = args.first().is_some_and(|a| a == "--round-trip");
    if round_trip {
        args.remove(0);
    }
    let path = args.into_iter().next().map(PathBuf::from);

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("omapix")
            .with_title("Omapix")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([480.0, 320.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    // Drawing the canvas is light work. Prefer the integrated GPU so opening
    // Omapix doesn't wake a laptop's discrete GPU and drain the battery.
    if let WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.power_preference = PowerPreference::LowPower;
    }

    eframe::run_native(
        "omapix",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, path, round_trip)))),
    )
}
