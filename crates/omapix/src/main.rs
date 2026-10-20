mod app;
mod auto_retouch;
mod blending_options;
mod body_liquify;
mod brushes;
mod canvas;
mod channels_panel;
mod clipboard;
mod commands;
mod content_fill;
mod denoise;
mod drop;
mod editor;
mod even_tone;
mod face_liquify;
mod face_selection;
mod free_transform;
mod generative_fill;
mod gpu;
mod heal_blemishes;
mod histogram_panel;
mod history_panel;
mod hotkeys;
mod layers_panel;
mod live;
mod mcp;
mod monitor;
mod navigator_panel;
mod object_selection;
mod presets;
mod preview_box;
mod properties_panel;
mod recent;
mod select_and_mask;
mod select_subject;
mod settings;
mod smooth_skin;
mod tablet;
mod theme;
mod tools;
mod upscale;
mod whiten;

use std::path::PathBuf;

use eframe::egui_wgpu::WgpuSetup;
use eframe::wgpu::PowerPreference;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,omapix=info")).init();
    // `omapix --round-trip image.tif` is how darktable sends an image (see
    // darktable/omapix.lua): saving also writes it back to the TIFF.
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    // `omapix --mcp` serves the Model Context Protocol for AI agents (see mcp.rs).
    if args.first().is_some_and(|a| a == "--mcp") {
        mcp::serve(std::io::stdin().lock(), std::io::stdout().lock());
        return Ok(());
    }
    let round_trip = args.first().is_some_and(|a| a == "--round-trip");
    if round_trip {
        args.remove(0);
    }
    let paths: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();

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
        // What the GPU has: egui asks for 8192 px textures whatever it is,
        // so a GPU with less makes no device at all, and one with more
        // can't show larger live views (as PhotoCraft's 436ca356).
        setup.device_descriptor = std::sync::Arc::new(|adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("omapix"),
            required_limits: adapter.limits(),
            ..Default::default()
        });
    }

    eframe::run_native("omapix", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, paths, round_trip)))))
}
