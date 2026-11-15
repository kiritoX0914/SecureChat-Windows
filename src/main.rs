// SecureChat for Windows – egui front end.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ui;

fn main() -> eframe::Result<()> {
    let icon = image::load_from_memory(include_bytes!("../assets/icon-256.png")).ok().map(|i| {
        let rgba = i.to_rgba8();
        let (w, h) = rgba.dimensions();
        std::sync::Arc::new(egui::IconData { rgba: rgba.into_raw(), width: w, height: h })
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("SecureChat")
        .with_app_id("SecureChat")
        .with_inner_size([1040.0, 720.0])
        .with_min_inner_size([640.0, 420.0])
        .with_drag_and_drop(true);
    if let Some(icon) = icon { viewport = viewport.with_icon(icon); }
    let options = eframe::NativeOptions { viewport, persist_window: true, ..Default::default() };
    eframe::run_native("SecureChat", options, Box::new(|cc| Ok(Box::new(ui::App::new(cc)))))
}
