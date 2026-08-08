mod app;
mod auth;
mod codegen;
mod curl_import;
mod http_client;
mod model;
mod openapi_import;
mod postman_format;
mod scripting;
mod storage;
mod syntax;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 750.0]),
        // glow (OpenGL), not wgpu (the eframe default) — wgpu's Vulkan/Metal
        // backend carries a noticeably higher baseline RAM footprint on
        // most systems (validation layers, extra buffers) than glow's
        // plain OpenGL path, for an app with no actual need for wgpu's
        // extra capability. `Cargo.toml`'s `eframe` dependency disables the
        // default `wgpu` feature and enables `glow` instead to match.
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "RustGirl",
        options,
        Box::new(|_cc| Ok(Box::new(app::App::new()))),
    )
}
