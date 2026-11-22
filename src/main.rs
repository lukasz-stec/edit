#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod doc;
mod editor;
mod highlight;
mod keymap;
mod search;
mod selection;
mod session;
mod text;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([480.0, 320.0])
            .with_title("edit")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("edit", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)))))
}
