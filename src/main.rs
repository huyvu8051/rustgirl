mod app;
mod http_client;
mod model;
mod scripting;
mod storage;
mod syntax;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 750.0]),
        ..Default::default()
    };
    eframe::run_native(
        "RustGirl",
        options,
        Box::new(|_cc| Ok(Box::new(app::App::new()))),
    )
}
