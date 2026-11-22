#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod diff;
mod diffview;
mod doc;
mod editor;
mod findfiles;
mod highlight;
mod history;
mod icon;
mod keymap;
mod search;
mod selection;
mod session;
mod text;
mod watch;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    history::init_local_offset(); // before any other thread starts
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([480.0, 320.0])
            .with_title("edit")
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(egui::IconData { rgba: icon::rgba(128), width: 128, height: 128 })),
        ..Default::default()
    };
    eframe::run_native("edit", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)))))
}
