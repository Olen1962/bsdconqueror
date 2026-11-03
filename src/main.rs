// src/main.rs
mod core;
mod ui;

use eframe::egui;
use ui::BSDConquerorApp;

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([850.0, 550.0])
            .with_title("bsdconqueror - Memory Scanner (Native FreeBSD)"),
        ..Default::default()
    };

    eframe::run_native(
        "bsdconqueror",
        options,
        Box::new(|cc| Ok(Box::new(BSDConquerorApp::new(cc)))),
    )
}
