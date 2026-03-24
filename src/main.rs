#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod change_format;
mod nvram;
mod presets;
mod scewin;
mod validation;

fn main() -> eframe::Result<()> {
    app::run()
}
